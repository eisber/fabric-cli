// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use jpx_core::ast::Comparator;
use jpx_core::functions::{
    ArgumentType as Type, CustomFunction, Function, Signature, custom_error, invalid_type_error,
};
use jpx_core::{Context, JmespathError, Runtime, ValueExt};
use serde_json::{Number, Value};

use crate::scalar;

fn unsupported(ctx: &Context<'_>) -> JmespathError {
    custom_error(
        ctx,
        "unsupported: Python numeric result is outside the supported JSON boundary",
    )
}

fn integer(value: i128, ctx: &Context<'_>) -> Result<Value, JmespathError> {
    if let Ok(value) = i64::try_from(value) {
        Ok(Value::from(value))
    } else if let Ok(value) = u64::try_from(value) {
        Ok(Value::from(value))
    } else {
        Err(unsupported(ctx))
    }
}

fn finite(value: f64, ctx: &Context<'_>) -> Result<Value, JmespathError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| unsupported(ctx))
}

fn as_integer(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn sum(values: &[Value], ctx: &Context<'_>) -> Result<Value, JmespathError> {
    let mut integers = 0_i128;
    let mut floats = None;
    let mut correction = 0.0;
    for value in values {
        if floats.is_none()
            && let Some(value) = as_integer(value)
        {
            integers = integers
                .checked_add(value)
                .ok_or_else(|| unsupported(ctx))?;
            continue;
        }
        let previous = floats.unwrap_or(integers as f64);
        let next = value.as_f64().expect("validated numeric array");
        let total = previous + next;
        correction += if previous.abs() >= next.abs() {
            (previous - total) + next
        } else {
            (next - total) + previous
        };
        floats = Some(total);
    }
    match floats {
        // Python changed compensated float summation in 3.12. Refuse results
        // whose bytes depend on that runtime difference rather than guessing.
        Some(value) if value + correction != value => Err(unsupported(ctx)),
        Some(value) => finite(value, ctx),
        None => integer(integers, ctx),
    }
}

fn to_number(
    value: &Value,
    numeric_sensitive: bool,
    ctx: &Context<'_>,
) -> Result<Value, JmespathError> {
    let Value::String(text) = value else {
        return Ok(if value.is_number() {
            value.clone()
        } else {
            Value::Null
        });
    };
    let text = text.trim();
    if text.chars().any(|c| !c.is_ascii() && c.is_numeric()) {
        return Err(unsupported(ctx));
    }
    let bytes = text.as_bytes();
    if bytes.iter().enumerate().any(|(i, &c)| {
        c == b'_'
            && (i == 0
                || i + 1 == bytes.len()
                || !bytes[i - 1].is_ascii_digit()
                || !bytes[i + 1].is_ascii_digit())
    }) {
        return Ok(Value::Null);
    }
    let text = text.replace('_', "");
    let result = if let Ok(value) = text.parse::<i128>() {
        integer(value, ctx)?
    } else if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-'))
        && text.chars().any(|c| c.is_ascii_digit())
    {
        // A syntactically valid decimal integer must not silently become f64.
        let digits = text
            .strip_prefix('+')
            .or_else(|| text.strip_prefix('-'))
            .unwrap_or(&text);
        if digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(unsupported(ctx));
        }
        Value::Null
    } else if let Ok(value) = text.parse::<f64>() {
        finite(value, ctx)?
    } else {
        Value::Null
    };
    if numeric_sensitive && crate::validation::wide_integer(&result) {
        return Err(unsupported(ctx));
    }
    Ok(result)
}

fn python_equal(left: &Value, right: &Value) -> bool {
    let numeric = |value: &Value| match value {
        Value::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        _ => None,
    };
    if let (Some(left), Some(right)) = (as_integer(left), as_integer(right)) {
        return left == right;
    }
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| python_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| python_equal(left, right))
                })
        }
        _ => match (numeric(left), numeric(right)) {
            (Some(left), Some(right)) => left == right,
            _ => left == right,
        },
    }
}

struct BoundedFunction(Box<dyn Function>);

impl Function for BoundedFunction {
    fn evaluate(&self, args: &[Value], ctx: &mut Context<'_>) -> Result<Value, JmespathError> {
        if args.iter().any(crate::validation::wide_integer) {
            return Err(unsupported(ctx));
        }
        self.0.evaluate(args, ctx)
    }
}

pub(crate) fn runtime(numeric_sensitive: bool) -> Runtime {
    let mut runtime = Runtime::new();
    runtime.register_builtin_functions();
    for (name, comparator) in [
        ("__fab_eq", Comparator::Equal),
        ("__fab_ne", Comparator::NotEqual),
        ("__fab_lt", Comparator::LessThan),
        ("__fab_le", Comparator::LessThanEqual),
        ("__fab_gt", Comparator::GreaterThan),
        ("__fab_ge", Comparator::GreaterThanEqual),
    ] {
        runtime.register_function(
            name,
            Box::new(CustomFunction::new(
                Signature::new(vec![Type::Any, Type::Any], None),
                Box::new(move |args, _| {
                    // Python's equality helper excludes bool/number only at the
                    // outer level; nested containers use Python's native equality.
                    if matches!(comparator, Comparator::Equal | Comparator::NotEqual) {
                        let special = (args[0].is_boolean() && args[1].is_number())
                            || (args[1].is_boolean() && args[0].is_number());
                        let equal = !special && python_equal(&args[0], &args[1]);
                        Ok(Value::Bool(if name == "__fab_eq" { equal } else { !equal }))
                    } else {
                        Ok(args[0]
                            .compare(&comparator, &args[1])
                            .map_or(Value::Null, Value::Bool))
                    }
                }),
            )),
        );
    }
    for name in ["abs", "ceil", "floor"] {
        runtime.register_function(
            name,
            Box::new(CustomFunction::new(
                Signature::new(vec![Type::Number], None),
                Box::new(move |args, ctx| {
                    if let Some(value) = as_integer(&args[0]) {
                        return integer(if name == "abs" { value.abs() } else { value }, ctx);
                    }
                    let value = args[0].as_f64().expect("validated number");
                    if name == "abs" {
                        finite(value.abs(), ctx)
                    } else {
                        let rounded = if name == "ceil" {
                            value.ceil()
                        } else {
                            value.floor()
                        };
                        integer(rounded as i128, ctx)
                    }
                }),
            )),
        );
    }
    for name in ["sum", "avg"] {
        runtime.register_function(
            name,
            Box::new(CustomFunction::new(
                Signature::new(vec![Type::TypedArray(Box::new(Type::Number))], None),
                Box::new(move |args, ctx| {
                    let values = args[0].as_array().expect("validated array");
                    if name == "avg" && values.is_empty() {
                        return Ok(Value::Null);
                    }
                    let total = sum(values, ctx)?;
                    if name == "avg" {
                        finite(
                            total.as_f64().expect("numeric total") / values.len() as f64,
                            ctx,
                        )
                    } else {
                        Ok(total)
                    }
                }),
            )),
        );
    }
    runtime.register_function(
        "to_number",
        Box::new(CustomFunction::new(
            Signature::new(vec![Type::Any], None),
            Box::new(move |args, ctx| to_number(&args[0], numeric_sensitive, ctx)),
        )),
    );
    runtime.register_function(
        "to_string",
        Box::new(CustomFunction::new(
            Signature::new(vec![Type::Any], None),
            Box::new(|args, _| {
                Ok(match &args[0] {
                    Value::String(_) => args[0].clone(),
                    value => Value::String(scalar::json(value)),
                })
            }),
        )),
    );
    runtime.register_function(
        "contains",
        Box::new(CustomFunction::new(
            Signature::new(
                vec![Type::Union(vec![Type::String, Type::Array]), Type::Any],
                None,
            ),
            Box::new(|args, ctx| match &args[0] {
                Value::String(subject) => args[1]
                    .as_str()
                    .map(|search| Value::Bool(subject.contains(search)))
                    .ok_or_else(|| invalid_type_error(ctx, 1, "string", &args[1])),
                Value::Array(subject) => Ok(Value::Bool(
                    subject.iter().any(|value| python_equal(value, &args[1])),
                )),
                _ => unreachable!("validated subject"),
            }),
        )),
    );
    for name in [
        "__fab_eq", "__fab_ne", "__fab_lt", "__fab_le", "__fab_gt", "__fab_ge", "sort", "sort_by",
        "min", "max", "min_by", "max_by", "contains", "sum", "avg",
    ] {
        let function = runtime
            .deregister_function(name)
            .expect("registered standard adapter");
        runtime.register_function(name, Box::new(BoundedFunction(function)));
    }
    runtime
}
