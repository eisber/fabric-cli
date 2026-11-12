// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use jpx_core::ast::{Ast, Comparator};
use serde_json::Value;

use crate::{QueryError, QueryErrorKind, unsupported};

pub(crate) fn literals(expression: &str) -> Result<(), QueryError> {
    let mut characters = expression.char_indices().peekable();
    while let Some((start, quote)) = characters.next() {
        if !matches!(quote, '\'' | '"' | '`') {
            continue;
        }
        let mut end = None;
        while let Some((offset, character)) = characters.next() {
            if character == '\\' {
                characters.next();
            } else if character == quote {
                end = Some(offset);
                break;
            }
        }
        if quote != '`' {
            continue;
        }
        let Some(end) = end else { continue };
        let raw = expression[start + 1..end].replace("\\`", "`");
        // Python retains deprecated unquoted backtick strings. The selected
        // parser intentionally accepts strict JSON literals only.
        if serde_json::from_str::<Value>(&raw).is_err() {
            if !raw.contains(['"', '[', ']', '{', '}', '\\', '\n', '\r']) {
                return Err(unsupported());
            }
            continue;
        }
        let mut quoted = false;
        let mut escaped = false;
        let mut token = String::new();
        for character in raw.chars().chain(std::iter::once(' ')) {
            if quoted {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    quoted = false;
                }
                continue;
            }
            if character == '"' {
                quoted = true;
            }
            if character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | 'e' | 'E') {
                token.push(character);
            } else {
                if !token.is_empty()
                    && !token.contains(['.', 'e', 'E'])
                    && token.parse::<i64>().is_err()
                    && token.parse::<u64>().is_err()
                {
                    return Err(unsupported());
                }
                token.clear();
            }
        }
    }
    Ok(())
}

pub(crate) fn wide_integer(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Number(number) if !number.is_f64() => {
                if number
                    .as_i64()
                    .is_some_and(|n| n.unsigned_abs() > (1_u64 << 53))
                    || number.as_u64().is_some_and(|n| n > (1_u64 << 53))
                {
                    return true;
                }
            }
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => pending.extend(values.values()),
            _ => {}
        }
    }
    false
}

pub(crate) fn validate(ast: &Ast) -> Result<bool, QueryError> {
    let mut pending = vec![(ast, false)];
    let mut sensitive = false;
    while let Some((ast, allow_expref)) = pending.pop() {
        match ast {
            Ast::Comparison { lhs, rhs, .. } => {
                sensitive = true;
                pending.extend([(lhs.as_ref(), false), (rhs.as_ref(), false)]);
            }
            Ast::Function { name, args, .. } => {
                if name.starts_with("__fab_") {
                    return Err(QueryError {
                        kind: QueryErrorKind::UnknownFunction,
                    });
                }
                sensitive |= matches!(
                    name.as_str(),
                    "sort"
                        | "sort_by"
                        | "min"
                        | "max"
                        | "min_by"
                        | "max_by"
                        | "contains"
                        | "sum"
                        | "avg"
                );
                for (index, arg) in args.iter().enumerate() {
                    let allowed = (name == "map" && index == 0)
                        || (matches!(name.as_str(), "sort_by" | "min_by" | "max_by") && index == 1);
                    pending.push((arg, allowed));
                }
            }
            Ast::Index { idx: i32::MIN, .. } => return Err(unsupported()),
            Ast::Slice { step, .. } if step.unsigned_abs() > 1_000_000 => return Err(unsupported()),
            Ast::Condition {
                predicate, then, ..
            } => pending.extend([(predicate.as_ref(), false), (then.as_ref(), false)]),
            Ast::Expref { ast, .. } => {
                if !allow_expref {
                    return Err(unsupported());
                }
                pending.push((ast, false));
            }
            Ast::Flatten { node, .. } | Ast::ObjectValues { node, .. } | Ast::Not { node, .. } => {
                pending.push((node, false))
            }
            Ast::MultiList { elements, .. } => {
                pending.extend(elements.iter().map(|element| (element, false)))
            }
            Ast::MultiHash { elements, .. } => {
                pending.extend(elements.iter().map(|pair| (&pair.value, false)))
            }
            Ast::Projection { lhs, rhs, .. }
            | Ast::And { lhs, rhs, .. }
            | Ast::Or { lhs, rhs, .. }
            | Ast::Subexpr { lhs, rhs, .. } => {
                pending.extend([(lhs.as_ref(), false), (rhs.as_ref(), false)])
            }
            _ => {}
        }
    }
    Ok(sensitive)
}

pub(crate) fn adapt_comparisons(ast: &mut Ast) {
    match ast {
        Ast::Comparison { lhs, rhs, .. }
        | Ast::Projection { lhs, rhs, .. }
        | Ast::And { lhs, rhs, .. }
        | Ast::Or { lhs, rhs, .. }
        | Ast::Subexpr { lhs, rhs, .. } => {
            adapt_comparisons(lhs);
            adapt_comparisons(rhs);
        }
        Ast::Condition {
            predicate, then, ..
        } => {
            adapt_comparisons(predicate);
            adapt_comparisons(then);
        }
        Ast::Expref { ast, .. } => adapt_comparisons(ast),
        Ast::Flatten { node, .. } | Ast::ObjectValues { node, .. } | Ast::Not { node, .. } => {
            adapt_comparisons(node)
        }
        Ast::MultiList { elements, .. } | Ast::Function { args: elements, .. } => {
            for element in elements {
                adapt_comparisons(element);
            }
        }
        Ast::MultiHash { elements, .. } => {
            for pair in elements {
                adapt_comparisons(&mut pair.value);
            }
        }
        _ => {}
    }
    if let Ast::Comparison {
        offset,
        comparator,
        lhs,
        rhs,
    } = ast
    {
        *ast = Ast::Function {
            offset: *offset,
            name: match comparator {
                Comparator::Equal => "__fab_eq",
                Comparator::NotEqual => "__fab_ne",
                Comparator::LessThan => "__fab_lt",
                Comparator::LessThanEqual => "__fab_le",
                Comparator::GreaterThan => "__fab_gt",
                Comparator::GreaterThanEqual => "__fab_ge",
            }
            .to_owned(),
            args: vec![*lhs.clone(), *rhs.clone()],
        };
    }
}
