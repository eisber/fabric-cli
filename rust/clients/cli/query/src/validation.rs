// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use jpx_core::ast::{Ast, Comparator};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{QueryError, QueryErrorKind, unsupported};

/// Decode JSON literals outside jpx's `ValueVisitor` path. With serde's
/// arbitrary_precision feature that path mistakes a genuine object member
/// named "$serde_json::private::Number" for serde's synthetic numeric marker.
pub(crate) fn parse(expression: &str) -> Result<Ast, QueryError> {
    let mut masked = expression.as_bytes().to_vec();
    let mut literals = BTreeMap::new();
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
        let value = match fabric_core_json::decode(raw.as_bytes()) {
            Err(_) => {
                // RawValue validates syntax without interpreting object
                // members as serde's private numeric representation.
                if serde_json::from_str::<&serde_json::value::RawValue>(&raw).is_ok() {
                    return Err(unsupported());
                }
                if !raw.contains(['"', '[', ']', '{', '}', '\\', '\n', '\r']) {
                    return Err(unsupported());
                }
                return Err(QueryError {
                    kind: QueryErrorKind::Syntax,
                });
            }
            Ok(value) => value,
        };
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
        // Feed only a benign numeric literal to jpx. Preserve byte length,
        // including multibyte source characters, so every AST offset still
        // addresses the original expression. Valid JSON is never empty.
        masked[start + 1..end].fill(b' ');
        masked[start + 1] = b'0';
        literals.insert(start, value);
    }
    let masked = String::from_utf8(masked).expect("only complete literal interiors were replaced");
    let mut ast = jpx_core::parse(&masked).map_err(QueryError::from)?;
    let mut pending = vec![&mut ast];
    while let Some(ast) = pending.pop() {
        match ast {
            Ast::Literal { offset, value } => {
                if let Some(decoded) = literals.remove(offset) {
                    *value = decoded;
                }
            }
            Ast::Comparison { lhs, rhs, .. }
            | Ast::Projection { lhs, rhs, .. }
            | Ast::And { lhs, rhs, .. }
            | Ast::Or { lhs, rhs, .. }
            | Ast::Subexpr { lhs, rhs, .. } => {
                pending.extend([lhs.as_mut(), rhs.as_mut()]);
            }
            Ast::Condition {
                predicate, then, ..
            } => pending.extend([predicate.as_mut(), then.as_mut()]),
            Ast::Expref { ast, .. }
            | Ast::Flatten { node: ast, .. }
            | Ast::ObjectValues { node: ast, .. }
            | Ast::Not { node: ast, .. } => pending.push(ast.as_mut()),
            Ast::MultiList { elements, .. } | Ast::Function { args: elements, .. } => {
                pending.extend(elements.iter_mut());
            }
            Ast::MultiHash { elements, .. } => {
                pending.extend(elements.iter_mut().map(|pair| &mut pair.value));
            }
            _ => {}
        }
    }
    if !literals.is_empty() {
        return Err(QueryError {
            kind: QueryErrorKind::Syntax,
        });
    }
    Ok(ast)
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
                    || (number.as_i64().is_none() && number.as_u64().is_none())
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
    let runtime = crate::functions::runtime(false);
    let mut pending = vec![(ast, false, 1)];
    let mut sensitive = false;
    while let Some((ast, allow_expref, depth)) = pending.pop() {
        if depth > crate::budget::MAX_DEPTH {
            return Err(QueryError {
                kind: QueryErrorKind::ResourceLimit,
            });
        }
        match ast {
            Ast::Comparison { lhs, rhs, .. } => {
                sensitive = true;
                pending.extend([
                    (lhs.as_ref(), false, depth + 1),
                    (rhs.as_ref(), false, depth + 1),
                ]);
            }
            Ast::Function { name, args, .. } => {
                if name.starts_with("__fab_") {
                    return Err(QueryError {
                        kind: QueryErrorKind::UnknownFunction,
                    });
                }
                crate::functions::validate_call(&runtime, name, args.len())?;
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
                    pending.push((arg, allowed, depth + 1));
                }
            }
            Ast::Index { idx: i32::MIN, .. } => return Err(unsupported()),
            Ast::Slice { step, .. } if step.unsigned_abs() > 1_000_000 => return Err(unsupported()),
            Ast::Condition {
                predicate, then, ..
            } => pending.extend([
                (predicate.as_ref(), false, depth + 1),
                (then.as_ref(), false, depth + 1),
            ]),
            Ast::Expref { ast, .. } => {
                if !allow_expref {
                    return Err(unsupported());
                }
                pending.push((ast, false, depth + 1));
            }
            Ast::Flatten { node, .. } | Ast::ObjectValues { node, .. } | Ast::Not { node, .. } => {
                pending.push((node, false, depth + 1))
            }
            Ast::MultiList { elements, .. } => {
                pending.extend(elements.iter().map(|element| (element, false, depth + 1)))
            }
            Ast::MultiHash { elements, .. } => {
                pending.extend(elements.iter().map(|pair| (&pair.value, false, depth + 1)))
            }
            Ast::Projection { lhs, rhs, .. }
            | Ast::And { lhs, rhs, .. }
            | Ast::Or { lhs, rhs, .. }
            | Ast::Subexpr { lhs, rhs, .. } => pending.extend([
                (lhs.as_ref(), false, depth + 1),
                (rhs.as_ref(), false, depth + 1),
            ]),
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
