// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{TransformBudget, TransformError};
use crate::yaml::{self, YamlLimits, YamlPolicy};
use jsonpath_rust::{
    JsonPath,
    parser::{FilterExpression, JsonPathIndex, Operand},
};
use serde_json::Value;
use std::{cmp::Ordering, fmt};

#[derive(Clone, Debug, Eq, PartialEq)]
enum Segment {
    Field(String),
    Index(usize),
}

type Location = Vec<Segment>;

#[derive(Clone, Copy, Eq, PartialEq)]
enum Comparison {
    Equal,
    Unequal,
    Less,
    Greater,
    LeOrEq,
    GrOrEq,
}

#[derive(Clone)]
struct ComparisonOperators(Vec<(Comparison, JsonPath)>);

impl ComparisonOperators {
    fn parse(budget: &mut TransformBudget<'_>) -> Result<Self, TransformError> {
        let mut operators = Vec::new();
        for (comparison, expression) in [
            (Comparison::Equal, "$[?(@ == 0)]"),
            (Comparison::Unequal, "$[?(@ != 0)]"),
            (Comparison::Less, "$[?(@ < 0)]"),
            (Comparison::Greater, "$[?(@ > 0)]"),
            (Comparison::LeOrEq, "$[?(@ <= 0)]"),
            (Comparison::GrOrEq, "$[?(@ >= 0)]"),
        ] {
            budget.checkpoint()?;
            let path = JsonPath::try_from(expression).map_err(|_| {
                TransformError::Invalid("Invalid JSONPath comparison reference".into())
            })?;
            operators.push((comparison, path));
        }
        Ok(Self(operators))
    }

    fn classify(
        &self,
        filter: &FilterExpression<Value>,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Comparison, TransformError> {
        let FilterExpression::Atom(_, sign, _) = filter else {
            return Err(TransformError::Unsupported("compound JSONPath filter"));
        };
        // 0.7.5 exposes Atom but not its operator's type. Compare the inferred
        // operator values through PartialEq, never Debug/Display or evaluation.
        for (comparison, reference) in &self.0 {
            budget.checkpoint()?;
            if let JsonPath::Chain(parts) = reference
                && let [
                    JsonPath::Root,
                    JsonPath::Index(JsonPathIndex::Filter(FilterExpression::Atom(_, expected, _))),
                ] = parts.as_slice()
                && sign == expected
            {
                return Ok(*comparison);
            }
        }
        Err(TransformError::Unsupported("JSONPath filter operator"))
    }
}

/// Parsed JSONPath mutation, not CLI JMESPath or an unbounded crate evaluator.
#[derive(Clone)]
pub struct JsonPathReplacement {
    path: JsonPath,
    operators: ComparisonOperators,
}

impl fmt::Debug for JsonPathReplacement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JsonPathReplacement")
            .finish_non_exhaustive()
    }
}

impl JsonPathReplacement {
    /// Parse and validate supported syntax before discovering matches or writing.
    ///
    /// The external crate supplies syntax only. A bounded interpreter below owns
    /// concrete paths, source ordering, comparisons and no-create mutation.
    ///
    /// # Errors
    /// Rejects invalid/relative expressions, unproven operators and syntax bounds.
    pub fn compile(
        expression: &str,
        budget: &mut TransformBudget<'_>,
    ) -> Result<Self, TransformError> {
        budget.checkpoint()?;
        if expression.len() > budget.limits.max_pattern_bytes {
            return Err(TransformError::Limit);
        }
        if !expression.trim_start().starts_with('$') {
            return Err(TransformError::Invalid(
                "find_key must be an absolute JSONPath starting with '$'".into(),
            ));
        }
        let mut depth = 0_usize;
        let mut current_markers = 0_usize;
        let mut quoted = None;
        for character in expression.chars() {
            budget.checkpoint()?;
            if character == '\\' {
                return Err(TransformError::Unsupported(
                    "escaped JSONPath property names",
                ));
            }
            if let Some(quote) = quoted {
                if character == quote {
                    quoted = None;
                }
                continue;
            }
            if matches!(character, '\'' | '"') {
                quoted = Some(character);
            } else if character == '@' {
                current_markers += 1;
                budget.depth(current_markers)?;
            } else if matches!(character, '[' | '(') {
                depth = depth.checked_add(1).ok_or(TransformError::Limit)?;
                budget.depth(depth)?;
            } else if matches!(character, ']' | ')') {
                depth = depth.saturating_sub(1);
            }
        }
        if expression.contains(":0]") || expression.contains(":0:") || expression.contains("::-") {
            return Err(TransformError::Unsupported(
                "zero slice endpoints or negative slice steps",
            ));
        }
        let path = JsonPath::try_from(expression)
            .map_err(|_| TransformError::Invalid("Invalid JSONPath expression".into()))?;
        if let JsonPath::Chain(parts) = &path
            && (!matches!(parts.first(), Some(JsonPath::Root))
                || parts
                    .iter()
                    .skip(1)
                    .any(|part| matches!(part, JsonPath::Root)))
        {
            return Err(TransformError::Unsupported(
                "repeated or non-leading JSONPath root",
            ));
        }
        let operators = ComparisonOperators::parse(budget)?;
        validate(&path, &operators, budget, 0)?;
        Ok(Self { path, operators })
    }

    /// Mutate only existing selected concrete paths, preserving no-op input bytes.
    ///
    /// The optional replacement is already environment/dynamic-value resolved.
    /// None means missing environment: still parse syntax but do not reformat.
    /// YAML emission supports flat scalar mappings only; other changed YAML trees
    /// fail explicitly until their independent PyYAML emission contract is ported.
    ///
    /// # Errors
    /// Rejects unsupported trees/comparisons, malformed content and finite
    /// depth/node/visit/match/output/cancellation/time exhaustion before returning.
    pub fn apply(
        &self,
        content: &str,
        replacement: Option<&Value>,
        is_yaml: bool,
        budget: &mut TransformBudget<'_>,
    ) -> Result<String, TransformError> {
        budget.text(content)?;
        let mut data = if is_yaml {
            yaml::decode(
                content,
                YamlPolicy::Config,
                YamlLimits {
                    max_bytes: budget.limits.max_bytes,
                    max_depth: budget.limits.max_depth,
                    max_nodes: budget.limits.max_visits,
                },
            )
            .map_err(|_| {
                TransformError::Unsupported("YAML value outside the parameter JSON domain")
            })?
        } else {
            serde_json::from_str(content)
                .map_err(|_| TransformError::Invalid("Invalid JSON parameter content".into()))?
        };
        if is_yaml && data.is_null() {
            return Ok(content.into());
        }
        validate_value(&data, budget, 0)?;
        if let Some(replacement) = replacement {
            validate_value(replacement, budget, 0)?;
        }
        let paths = select(
            &self.path,
            &data,
            vec![Vec::new()],
            &self.operators,
            budget,
            0,
        )?;
        let Some(replacement) = replacement.filter(|_| !paths.is_empty()) else {
            return Ok(content.into());
        };
        for (index, path) in paths.iter().enumerate() {
            budget.checkpoint()?;
            for other in &paths[..index] {
                budget.checkpoint()?;
                if !path.is_empty()
                    && !other.is_empty()
                    && path != other
                    && (path.starts_with(other) || other.starts_with(path))
                {
                    return Err(TransformError::Unsupported(
                        "overlapping ancestor/descendant JSONPath mutation",
                    ));
                }
            }
        }
        let replacement_length = fabric_core_json::json_dumps(replacement).len();
        let mut encoded_length = fabric_core_json::json_dumps(&data).len();
        if encoded_length > budget.limits.max_bytes {
            return Err(TransformError::Limit);
        }
        for path in paths {
            budget.matched()?;
            // jsonpath_ng Root.update returns a value; the source caller ignores
            // that return but still marks replacement_made and emits JSON.
            if path.is_empty() {
                continue;
            }
            let target = get_mut(&mut data, &path).ok_or_else(|| {
                TransformError::Invalid("Matched JSONPath no longer exists".into())
            })?;
            encoded_length = encoded_length
                .checked_sub(fabric_core_json::json_dumps(target).len())
                .and_then(|length| length.checked_add(replacement_length))
                .filter(|length| *length <= budget.limits.max_bytes)
                .ok_or(TransformError::Limit)?;
            *target = replacement.clone();
        }
        budget.checkpoint()?;
        let output = if is_yaml {
            emit_flat_yaml(&data, budget)?
        } else {
            fabric_core_json::json_dumps(&data)
        };
        budget.text(&output)?;
        Ok(output)
    }
}

fn validate(
    path: &JsonPath,
    operators: &ComparisonOperators,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<(), TransformError> {
    budget.depth(depth)?;
    match path {
        JsonPath::Root
        | JsonPath::Field(_)
        | JsonPath::Descent(_)
        | JsonPath::Wildcard
        | JsonPath::Empty => {}
        JsonPath::Chain(parts) => {
            for part in parts {
                validate(part, operators, budget, depth + 1)?;
            }
        }
        JsonPath::Current(part) => validate(part, operators, budget, depth + 1)?,
        JsonPath::Index(index) => match index {
            JsonPathIndex::Single(value) if value.as_u64().is_some() => {}
            JsonPathIndex::UnionKeys(_) => {}
            JsonPathIndex::UnionIndex(values)
                if values.iter().all(|value| value.as_u64().is_some()) => {}
            JsonPathIndex::Slice(_, _, step) if *step != 0 => {}
            JsonPathIndex::Filter(filter) => validate_filter(filter, operators, budget, depth + 1)?,
            _ => return Err(TransformError::Unsupported("JSONPath index domain")),
        },
        _ => {
            return Err(TransformError::Unsupported(
                "JSONPath function or recursive wildcard",
            ));
        }
    }
    Ok(())
}

fn validate_filter(
    filter: &FilterExpression<Value>,
    operators: &ComparisonOperators,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<(), TransformError> {
    budget.depth(depth)?;
    let FilterExpression::Atom(Operand::Dynamic(path), _, Operand::Static(value)) = filter else {
        return Err(TransformError::Unsupported(
            "compound or dynamic-right JSONPath filter",
        ));
    };
    operators.classify(filter, budget)?;
    if value.is_array() || value.is_object() {
        return Err(TransformError::Unsupported(
            "structured JSONPath filter comparison",
        ));
    }
    validate_filter_target(path, budget, depth + 1)
}

fn validate_filter_target(
    path: &JsonPath,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<(), TransformError> {
    budget.depth(depth)?;
    match path {
        JsonPath::Current(path) => validate_filter_target(path, budget, depth + 1),
        JsonPath::Chain(parts) => {
            for part in parts {
                validate_filter_target(part, budget, depth + 1)?;
            }
            Ok(())
        }
        JsonPath::Field(_) | JsonPath::Empty => Ok(()),
        JsonPath::Index(JsonPathIndex::Single(value)) if value.as_u64().is_some() => Ok(()),
        _ => Err(TransformError::Unsupported(
            "non-current/simple JSONPath filter target",
        )),
    }
}

pub(super) fn validate_value(
    value: &Value,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<(), TransformError> {
    let mut bytes = 0_usize;
    value_size(value, budget, depth, &mut bytes)
}

fn value_size(
    value: &Value,
    budget: &mut TransformBudget<'_>,
    depth: usize,
    bytes: &mut usize,
) -> Result<(), TransformError> {
    budget.depth(depth)?;
    let add = match value {
        Value::String(value) => value.len(),
        _ => 1,
    };
    *bytes = bytes
        .checked_add(add)
        .filter(|bytes| *bytes <= budget.limits.max_bytes)
        .ok_or(TransformError::Limit)?;
    match value {
        Value::Array(values) => {
            for value in values {
                value_size(value, budget, depth + 1, bytes)?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                budget.text(key)?;
                *bytes = bytes
                    .checked_add(key.len())
                    .filter(|bytes| *bytes <= budget.limits.max_bytes)
                    .ok_or(TransformError::Limit)?;
                value_size(value, budget, depth + 1, bytes)?;
            }
        }
        Value::String(value) => budget.text(value)?,
        _ => {}
    }
    Ok(())
}

fn get<'a>(mut value: &'a Value, path: &[Segment]) -> Option<&'a Value> {
    for segment in path {
        value = match segment {
            Segment::Field(key) => value.get(key)?,
            Segment::Index(index) => value.get(*index)?,
        };
    }
    Some(value)
}

fn get_mut<'a>(mut value: &'a mut Value, path: &[Segment]) -> Option<&'a mut Value> {
    for segment in path {
        value = match segment {
            Segment::Field(key) => value.get_mut(key)?,
            Segment::Index(index) => value.get_mut(*index)?,
        };
    }
    Some(value)
}

fn child(
    path: &[Segment],
    segment: Segment,
    budget: &mut TransformBudget<'_>,
) -> Result<Location, TransformError> {
    budget.depth(path.len() + 1)?;
    let mut result = path.to_vec();
    result.push(segment);
    Ok(result)
}

fn append(
    output: &mut Vec<Location>,
    path: Location,
    budget: &mut TransformBudget<'_>,
) -> Result<(), TransformError> {
    budget.checkpoint()?;
    if output.len() >= budget.limits.max_matches {
        return Err(TransformError::Limit);
    }
    output.push(path);
    Ok(())
}

fn children(
    data: &Value,
    path: &[Segment],
    budget: &mut TransformBudget<'_>,
) -> Result<Vec<Location>, TransformError> {
    let mut output = Vec::new();
    match get(data, path) {
        Some(Value::Array(values)) => {
            for index in 0..values.len() {
                let path = child(path, Segment::Index(index), budget)?;
                append(&mut output, path, budget)?;
            }
        }
        Some(Value::Object(values)) => {
            for key in values.keys() {
                let path = child(path, Segment::Field(key.clone()), budget)?;
                append(&mut output, path, budget)?;
            }
        }
        _ => {}
    }
    Ok(output)
}

fn select(
    query: &JsonPath,
    data: &Value,
    input: Vec<Location>,
    operators: &ComparisonOperators,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<Vec<Location>, TransformError> {
    budget.depth(depth)?;
    if let JsonPath::Chain(parts) = query {
        let mut current = input;
        for part in parts {
            current = select(part, data, current, operators, budget, depth + 1)?;
        }
        return Ok(current);
    }
    if let JsonPath::Current(path) = query {
        return select(path, data, input, operators, budget, depth + 1);
    }
    if matches!(query, JsonPath::Root | JsonPath::Empty) {
        return Ok(input);
    }
    let mut output = Vec::new();
    for path in input {
        budget.checkpoint()?;
        let Some(value) = get(data, &path) else {
            continue;
        };
        match query {
            JsonPath::Field(key) => {
                if value
                    .as_object()
                    .is_some_and(|object| object.contains_key(key))
                {
                    let path = child(&path, Segment::Field(key.clone()), budget)?;
                    append(&mut output, path, budget)?;
                }
            }
            JsonPath::Wildcard => {
                for path in children(data, &path, budget)? {
                    append(&mut output, path, budget)?;
                }
            }
            JsonPath::Descent(key) => {
                let mut stack = vec![path];
                while let Some(path) = stack.pop() {
                    budget.depth(path.len())?;
                    if get(data, &path)
                        .and_then(Value::as_object)
                        .is_some_and(|object| object.contains_key(key))
                    {
                        let found = child(&path, Segment::Field(key.clone()), budget)?;
                        append(&mut output, found, budget)?;
                    }
                    stack.extend(children(data, &path, budget)?.into_iter().rev());
                    if stack.len() > budget.limits.max_visits {
                        return Err(TransformError::Limit);
                    }
                }
            }
            JsonPath::Index(index) => match index {
                JsonPathIndex::Single(index) => {
                    if let Some(index) =
                        index.as_u64().and_then(|index| usize::try_from(index).ok())
                        && value.as_array().is_some_and(|array| index < array.len())
                    {
                        let path = child(&path, Segment::Index(index), budget)?;
                        append(&mut output, path, budget)?;
                    }
                }
                JsonPathIndex::UnionKeys(keys) => {
                    for key in keys {
                        if value
                            .as_object()
                            .is_some_and(|object| object.contains_key(key))
                        {
                            let path = child(&path, Segment::Field(key.clone()), budget)?;
                            append(&mut output, path, budget)?;
                        }
                    }
                }
                JsonPathIndex::UnionIndex(indices) => {
                    for index in indices {
                        if let Some(index) =
                            index.as_u64().and_then(|index| usize::try_from(index).ok())
                            && value.as_array().is_some_and(|array| index < array.len())
                        {
                            let path = child(&path, Segment::Index(index), budget)?;
                            append(&mut output, path, budget)?;
                        }
                    }
                }
                JsonPathIndex::Slice(start, end, step) => {
                    if let Some(values) = value.as_array() {
                        let length =
                            i64::try_from(values.len()).map_err(|_| TransformError::Limit)?;
                        let endpoint = |index: i32| {
                            if index < 0 {
                                (length + i64::from(index)).clamp(0, length)
                            } else {
                                i64::from(index).min(length)
                            }
                        };
                        let start = endpoint(*start) as usize;
                        let end = if *end == 0 {
                            values.len()
                        } else {
                            endpoint(*end) as usize
                        };
                        for index in (start..end).step_by(*step) {
                            let path = child(&path, Segment::Index(index), budget)?;
                            append(&mut output, path, budget)?;
                        }
                    }
                }
                JsonPathIndex::Filter(filter) => {
                    for candidate in children(data, &path, budget)? {
                        if matches_filter(filter, data, &candidate, operators, budget, depth + 1)? {
                            append(&mut output, candidate, budget)?;
                        }
                    }
                }
            },
            _ => return Err(TransformError::Unsupported("JSONPath evaluation branch")),
        }
    }
    Ok(output)
}

fn matches_filter(
    filter: &FilterExpression<Value>,
    data: &Value,
    candidate: &[Segment],
    operators: &ComparisonOperators,
    budget: &mut TransformBudget<'_>,
    depth: usize,
) -> Result<bool, TransformError> {
    let FilterExpression::Atom(Operand::Dynamic(path), _, Operand::Static(expected)) = filter
    else {
        return Err(TransformError::Unsupported("JSONPath filter"));
    };
    let comparison = operators.classify(filter, budget)?;
    for path in select(
        path,
        data,
        vec![candidate.to_vec()],
        operators,
        budget,
        depth + 1,
    )? {
        budget.checkpoint()?;
        let actual = get(data, &path).expect("selected existing filter path");
        let converted;
        let actual = if expected.as_i64().is_some()
            && let Value::String(value) = actual
        {
            let trimmed = value.trim();
            let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
            if digits.contains('_')
                || digits
                    .chars()
                    .any(|character| character.is_numeric() && !character.is_ascii())
            {
                return Err(TransformError::Unsupported(
                    "extended Python integer string filter coercion",
                ));
            }
            let value = match trimmed.parse::<i64>() {
                Ok(value) => value,
                Err(_)
                    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    return Err(TransformError::Unsupported(
                        "large Python integer string filter coercion",
                    ));
                }
                Err(_) => continue,
            };
            converted = Value::from(value);
            &converted
        } else {
            actual
        };
        let order = compare(actual, expected)?;
        let matched = match comparison {
            Comparison::Equal => order == Some(Ordering::Equal),
            Comparison::Unequal => order != Some(Ordering::Equal),
            Comparison::Less => order == Some(Ordering::Less),
            Comparison::Greater => order == Some(Ordering::Greater),
            Comparison::LeOrEq => matches!(order, Some(Ordering::Less | Ordering::Equal)),
            Comparison::GrOrEq => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
        };
        if order.is_none() && !matches!(comparison, Comparison::Equal | Comparison::Unequal) {
            return Err(TransformError::Invalid(
                "JSONPath comparison has incompatible source types".into(),
            ));
        }
        if matched {
            return Ok(true);
        }
    }
    Ok(false)
}

fn compare(actual: &Value, expected: &Value) -> Result<Option<Ordering>, TransformError> {
    match (actual, expected) {
        (Value::String(left), Value::String(right)) => Ok(Some(left.cmp(right))),
        (Value::Null, Value::Null) => Ok(Some(Ordering::Equal)),
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => Err(
            TransformError::Unsupported("structured JSONPath comparison"),
        ),
        _ => {
            let number = |value: &Value| match value {
                Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
                Value::Number(value) => value.as_f64(),
                _ => None,
            };
            match (number(actual), number(expected)) {
                (Some(left), Some(right))
                    if left.abs() <= 9_007_199_254_740_991.0
                        && right.abs() <= 9_007_199_254_740_991.0 =>
                {
                    Ok(left.partial_cmp(&right))
                }
                (Some(_), Some(_)) => Err(TransformError::Unsupported(
                    "large JSONPath numeric comparison",
                )),
                _ => Ok(None),
            }
        }
    }
}

fn emit_flat_yaml(
    value: &Value,
    budget: &mut TransformBudget<'_>,
) -> Result<String, TransformError> {
    let object = value
        .as_object()
        .ok_or(TransformError::Unsupported("non-mapping YAML emission"))?;
    let mut keys: Vec<_> = object.keys().collect();
    keys.sort();
    let mut output = String::new();
    for key in keys {
        let scalar = |value: &Value| -> Result<String, TransformError> {
            match value {
                Value::String(value) => {
                    if value.contains(['\n', '\r', '\t']) {
                        return Err(TransformError::Unsupported(
                            "multiline/control YAML scalar emission",
                        ));
                    }
                    let plain = !value.is_empty()
                        && value.trim() == value
                        && !value.starts_with([
                            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>',
                            '\'', '"', '%', '@', '`',
                        ])
                        && !value.contains(": ")
                        && !value.contains(" #");
                    let decoded = yaml::decode(value, YamlPolicy::Config, YamlLimits::default());
                    if plain && matches!(decoded, Ok(Value::String(ref parsed)) if parsed == value)
                    {
                        Ok(value.clone())
                    } else {
                        Ok(format!("'{}'", value.replace('\'', "''")))
                    }
                }
                Value::Null => Ok("null".into()),
                Value::Bool(value) => Ok(value.to_string()),
                Value::Number(value) => Ok(value.to_string()),
                _ => Err(TransformError::Unsupported("nested YAML mutation emission")),
            }
        };
        let line = format!(
            "{}: {}\n",
            scalar(&Value::String(key.clone()))?,
            scalar(&object[key])?
        );
        budget.push(&mut output, &line)?;
    }
    Ok(output)
}
