// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Admission control for the pinned, cloning jpx interpreter.
//!
//! jpx has a recursion cap but no allocation/work budget hook. Before
//! invoking it, bound the *expanded* results of every AST node, including
//! discarded pipe stages and expression-reference invocations. No JSON results
//! are built here. Unknown values use conservative upper bounds, not guessed
//! types or sampled rows. Consequently some otherwise valid queries can exceed
//! the bound even when their actual evaluation would be small.

use std::fmt::{self, Write};

use jpx_core::ast::Ast;
use serde_json::Value;

use crate::{QueryError, QueryErrorKind};

pub(crate) const MAX_QUERY_BYTES: usize = 64 * 1024;
const COPY_BYTES: u64 = 64 * 1024 * 1024;
const WORK: u64 = 1_000_000;
pub(crate) const MAX_DEPTH: usize = 128;
const SLOT: u64 = 64;

fn limit() -> QueryError {
    QueryError {
        kind: QueryErrorKind::ResourceLimit,
    }
}

pub(crate) fn expression(expression: &str) -> Result<(), QueryError> {
    if expression.len() > MAX_QUERY_BYTES {
        return Err(limit());
    }
    Ok(())
}

/// Logical copy weight, not an allocator-specific RSS promise: 64 bytes per
/// JSON node, plus up to six bytes per source string/key byte (JSON escaping).
/// `len` and `child` bound immediate container width and any immediate child.
#[derive(Clone, Copy)]
struct Bound<'a> {
    size: u64,
    len: u64,
    child: u64,
    depth: usize,
    known: Option<&'a Value>,
}

impl Bound<'_> {
    fn unknown(size: u64, depth: usize) -> Self {
        Self {
            size: size.max(SLOT),
            len: size / SLOT,
            child: size.max(SLOT),
            depth,
            known: None,
        }
    }

    fn scalar() -> Self {
        Self {
            size: SLOT * 2,
            len: 0,
            child: SLOT,
            depth: 1,
            known: None,
        }
    }

    fn child(self) -> Self {
        Self::unknown(self.child, self.depth)
    }

    fn opaque(mut self) -> Self {
        self.known = None;
        self
    }
}

struct Budget {
    bytes: u64,
    work: u64,
    ast_bytes: u64,
}

impl Budget {
    fn charge(&mut self, bytes: u64, work: u64, times: u64) -> Result<(), QueryError> {
        self.bytes = self
            .bytes
            .checked_sub(bytes.saturating_mul(times))
            .ok_or_else(limit)?;
        self.work = self
            .work
            .checked_sub(work.saturating_mul(times))
            .ok_or_else(limit)?;
        Ok(())
    }

    fn measure<'a>(&mut self, value: &'a Value, depth: usize) -> Result<Bound<'a>, QueryError> {
        if depth > MAX_DEPTH {
            return Err(limit());
        }
        self.charge(0, 1, 1)?;
        let mut bound = Bound {
            size: SLOT,
            len: 0,
            child: SLOT,
            depth: 1,
            known: Some(value),
        };
        match value {
            Value::String(text) => {
                bound.size = bound
                    .size
                    .saturating_add((text.len() as u64).saturating_mul(6));
            }
            Value::Number(number) => {
                // Display writes arbitrary-precision lexemes without cloning
                // them, unlike Number::to_string().
                let mut counter = Counter(0);
                write!(counter, "{number}").map_err(|_| limit())?;
                bound.size = bound.size.saturating_add(counter.0);
            }
            Value::Array(values) => {
                bound.len = values.len() as u64;
                for value in values {
                    let child = self.measure(value, depth + 1)?;
                    bound.size = bound.size.saturating_add(child.size);
                    bound.child = bound.child.max(child.size);
                    bound.depth = bound.depth.max(child.depth + 1);
                    if bound.size > self.bytes {
                        return Err(limit());
                    }
                }
            }
            Value::Object(values) => {
                bound.len = values.len() as u64;
                for (key, value) in values {
                    let child = self.measure(value, depth + 1)?;
                    bound.size = bound
                        .size
                        .saturating_add(child.size)
                        .saturating_add((key.len() as u64).saturating_mul(6));
                    bound.child = bound.child.max(child.size);
                    bound.depth = bound.depth.max(child.depth + 1);
                    if bound.size > self.bytes {
                        return Err(limit());
                    }
                }
            }
            _ => {}
        }
        if bound.size > self.bytes {
            return Err(limit());
        }
        Ok(bound)
    }

    fn evaluate<'a>(
        &mut self,
        ast: &'a Ast,
        input: Bound<'a>,
        times: u64,
        depth: usize,
    ) -> Result<Bound<'a>, QueryError> {
        if depth > MAX_DEPTH {
            return Err(limit());
        }
        self.charge(0, 1, times)?;
        let result = match ast {
            Ast::Identity { .. } => input,
            Ast::Literal { value, .. } => self.measure(value, 1)?,
            Ast::Field { name, .. } => match input.known {
                Some(value) => self.measure(value.get(name).unwrap_or(&Value::Null), 1)?,
                None => input.child(),
            },
            Ast::Index { idx, .. } => match input.known {
                Some(Value::Array(values)) => {
                    let index = if *idx < 0 {
                        values.len().checked_sub(idx.unsigned_abs() as usize)
                    } else {
                        Some(*idx as usize)
                    };
                    self.measure(index.and_then(|i| values.get(i)).unwrap_or(&Value::Null), 1)?
                }
                Some(_) => Bound::scalar(),
                None => input.child(),
            },
            Ast::Subexpr { lhs, rhs, .. } => {
                let left = self.evaluate(lhs, input, times, depth + 1)?;
                self.evaluate(rhs, left, times, depth + 1)?
            }
            Ast::And { lhs, rhs, .. } | Ast::Or { lhs, rhs, .. } => {
                let left = self.evaluate(lhs, input, times, depth + 1)?;
                let right = self.evaluate(rhs, input, times, depth + 1)?;
                Bound::unknown(left.size.max(right.size), left.depth.max(right.depth))
            }
            Ast::Condition {
                predicate, then, ..
            } => {
                self.evaluate(predicate, input, times, depth + 1)?;
                self.evaluate(then, input, times, depth + 1)?.opaque()
            }
            Ast::Not { node, .. } => {
                self.evaluate(node, input, times, depth + 1)?;
                Bound::scalar()
            }
            Ast::Comparison { lhs, rhs, .. } => {
                self.evaluate(lhs, input, times, depth + 1)?;
                self.evaluate(rhs, input, times, depth + 1)?;
                Bound::scalar()
            }
            Ast::Projection { lhs, rhs, .. } => {
                let left = self.evaluate(lhs, input, times, depth + 1)?;
                self.project(rhs, left, times, depth + 1)?
            }
            Ast::Flatten { node, .. } => {
                let subject = self.evaluate(node, input, times, depth + 1)?;
                let known = subject.known.filter(|value| {
                    value
                        .as_array()
                        .is_some_and(|values| values.iter().all(|value| !value.is_array()))
                });
                // Flattening never adds payload, but may expose every nested
                // node as an immediate element.
                Bound {
                    size: subject.size.saturating_add(SLOT),
                    len: known
                        .and_then(Value::as_array)
                        .map_or(subject.size / SLOT, |values| values.len() as u64),
                    child: subject.child,
                    depth: subject.depth,
                    // Flattening non-array elements preserves the actual
                    // values, including their selected-field information.
                    known,
                }
            }
            Ast::ObjectValues { node, .. } => {
                let subject = self.evaluate(node, input, times, depth + 1)?;
                Bound {
                    size: subject.size.saturating_add(SLOT),
                    known: None,
                    ..subject
                }
            }
            Ast::Slice { .. } => input.opaque(),
            Ast::MultiList { elements, .. } => {
                let mut result = Bound {
                    size: SLOT,
                    len: elements.len() as u64,
                    child: SLOT,
                    depth: 1,
                    known: None,
                };
                for element in elements {
                    let child = self.evaluate(element, input, times, depth + 1)?;
                    result.size = result.size.saturating_add(child.size);
                    result.child = result.child.max(child.size);
                    result.depth = result.depth.max(child.depth + 1);
                }
                result
            }
            Ast::MultiHash { elements, .. } => {
                let mut result = Bound::unknown(SLOT, 1);
                result.len = elements.len() as u64;
                for element in elements {
                    let child = self.evaluate(&element.value, input, times, depth + 1)?;
                    result.size = result
                        .size
                        .saturating_add(child.size)
                        .saturating_add((element.key.len() as u64).saturating_mul(6));
                    result.child = result.child.max(child.size);
                    result.depth = result.depth.max(child.depth + 1);
                }
                result
            }
            Ast::Expref { .. } => {
                // jpx stores/clones ASTs for expression references; include
                // literal payloads and native map/sort_by's additional clone.
                self.charge(self.ast_bytes.saturating_mul(2), 1, times)?;
                Bound::scalar()
            }
            Ast::Function { name, args, .. } => {
                let mut arguments = Vec::with_capacity(args.len());
                for arg in args {
                    arguments.push(self.evaluate(arg, input, times, depth + 1)?);
                }
                self.function(name, args, &arguments, times, depth + 1)?
            }
        };
        if result.depth > MAX_DEPTH {
            return Err(limit());
        }
        // Cumulative copy weight also bounds simultaneous retained results.
        // Reserve headroom for native temporary vectors/maps and validation.
        self.charge(result.size.saturating_mul(4), 1, times)?;
        Ok(result)
    }

    fn project<'a>(
        &mut self,
        rhs: &'a Ast,
        array: Bound<'a>,
        times: u64,
        depth: usize,
    ) -> Result<Bound<'a>, QueryError> {
        if let Some(Value::Array(values)) = array.known {
            let mut result = Bound {
                size: SLOT,
                len: values.len() as u64,
                child: SLOT,
                depth: 1,
                known: None,
            };
            for value in values {
                // Inspect every real element, not a sample. Selecting an id
                // must not multiply one row's unrelated large field by the
                // entire collection's length.
                let input = self.measure(value, 1)?;
                let child = self.evaluate(rhs, input, times, depth)?;
                result.size = result.size.saturating_add(child.size);
                result.child = result.child.max(child.size);
                result.depth = result.depth.max(child.depth + 1);
            }
            return Ok(result);
        }
        // Constructed/unknown collections can amplify data. Every element may
        // survive a filter, so retain the conservative largest-child bound.
        let child = self.evaluate(rhs, array.child(), times.saturating_mul(array.len), depth)?;
        Ok(Bound {
            size: SLOT.saturating_add(child.size.saturating_mul(array.len)),
            len: array.len,
            child: child.size,
            depth: child.depth + 1,
            known: None,
        })
    }

    fn function<'a>(
        &mut self,
        name: &str,
        ast_args: &'a [Ast],
        args: &[Bound<'a>],
        times: u64,
        depth: usize,
    ) -> Result<Bound<'a>, QueryError> {
        let total = args
            .iter()
            .fold(0_u64, |sum, arg| sum.saturating_add(arg.size));
        // Arguments already reserved their JSON copies. Scalar functions do
        // not indiscriminately clone them again: charge traversal work here,
        // and actual additional temporary storage in the cases below.
        self.charge(0, total / SLOT + 1, times)?;
        // Arity was checked using runtime signatures before this analysis.
        let first = args[0];
        Ok(match name {
            "map" | "sort_by" | "min_by" | "max_by" => {
                let (array, reference) = if name == "map" {
                    (args[1], &ast_args[0])
                } else {
                    (first, &ast_args[1])
                };
                let mapped = if let Ast::Expref { ast, .. } = reference {
                    self.project(ast, array, times, depth + 1)?
                } else {
                    // Invalid type: native signature validation stops before
                    // any expression reference can be invoked.
                    Bound::unknown(SLOT, 1)
                };
                if name == "map" {
                    mapped
                } else {
                    // The internal key validator runs once per evaluated key
                    // and passes accepted values through by cloning them.
                    self.charge(mapped.size, array.len, times)?;
                    self.sort_work(array, mapped.child, times)?;
                    if name == "sort_by" {
                        array.opaque()
                    } else {
                        // min_by/max_by return an element, whose own width
                        // can greatly exceed the source array's length.
                        array.child()
                    }
                }
            }
            "sort" => {
                self.sort_work(first, first.child, times)?;
                first.opaque()
            }
            "reverse" => first.opaque(),
            "min" | "max" => {
                // Native min/max folds clone candidate values as they scan.
                self.charge(first.size.saturating_mul(2), first.len, times)?;
                first.child()
            }
            "not_null" => Bound::unknown(
                args.iter().map(|arg| arg.size).max().unwrap_or(SLOT),
                args.iter().map(|arg| arg.depth).max().unwrap_or(1),
            ),
            "merge" => Bound::unknown(
                total.saturating_add(SLOT),
                args.iter().map(|arg| arg.depth).max().unwrap_or(1),
            ),
            "values" => Bound {
                size: first.size.saturating_add(SLOT),
                depth: first.depth + 1,
                known: None,
                ..first
            },
            "keys" => Bound {
                size: first.size.saturating_add(SLOT),
                child: first.size,
                depth: 2,
                known: None,
                ..first
            },
            "to_array" => Bound {
                size: first.size.saturating_add(SLOT),
                len: first.len.max(1),
                child: first.size,
                depth: first.depth + 1,
                known: None,
            },
            "to_string" => {
                let size = first.size.saturating_mul(6).saturating_add(SLOT);
                // Python-compatible recursive encoding builds intermediate
                // strings at each container level.
                self.charge(size.saturating_mul(first.depth as u64), 1, times)?;
                Bound::unknown(size, 1)
            }
            "join" => Bound::unknown(
                first
                    .size
                    .saturating_mul(args[1].len)
                    .saturating_add(args[1].size)
                    .saturating_add(SLOT),
                1,
            ),
            "contains" => {
                // Numeric guards use a traversal stack. Equality comparison
                // itself builds no JSON; count comparison work, not copies.
                self.charge(total, 0, times)?;
                self.charge(0, first.len.saturating_mul(args[1].size / SLOT + 1), times)?;
                Bound::scalar()
            }
            // to_number(number) can retain an arbitrary-width exact integer.
            "to_number" => Bound::unknown(first.size.max(SLOT * 2), 1),
            "avg" | "sum" => {
                // Numeric guards and typed-array validation traverse values,
                // but do not build another full JSON array.
                self.charge(total, total / SLOT + 1, times)?;
                Bound::scalar()
            }
            "abs" | "ceil" | "ends_with" | "floor" | "length" | "starts_with" | "type"
            | "__fab_eq" | "__fab_ne" | "__fab_lt" | "__fab_le" | "__fab_gt" | "__fab_ge" => {
                Bound::scalar()
            }
            // Fail closed if the pinned runtime gains any unmodelled function.
            _ => return Err(limit()),
        })
    }

    fn sort_work(&mut self, array: Bound<'_>, key: u64, times: u64) -> Result<(), QueryError> {
        let comparisons = array
            .len
            .saturating_mul(u64::from(array.len.max(1).ilog2()) + 1);
        self.charge(
            0,
            comparisons.saturating_mul(key / SLOT + 1).saturating_mul(8),
            times,
        )
    }
}

struct Counter(u64);

impl fmt::Write for Counter {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 = self.0.saturating_add(text.len() as u64);
        Ok(())
    }
}

pub(crate) fn admit(data: &Value, ast: &Ast, expression: &str) -> Result<(), QueryError> {
    admit_with_limits(data, ast, expression, COPY_BYTES, WORK)
}

fn admit_with_limits(
    data: &Value,
    ast: &Ast,
    expression: &str,
    bytes: u64,
    work: u64,
) -> Result<(), QueryError> {
    let mut budget = Budget {
        bytes,
        work,
        // An upper bound for AST storage per source byte, including decoded
        // literals, enum/box/vector overhead, and adapted comparison nodes.
        ast_bytes: (expression.len() as u64).saturating_mul(512),
    };
    budget.charge(budget.ast_bytes, 1, 1)?;
    let input = budget.measure(data, 1)?;
    budget.evaluate(ast, input, 1, 1)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn amplification_is_rejected_without_materializing_the_result() {
        let data = json!("x".repeat(1024));
        let expression = "[@,@] | [@,@] | [@,@] | length(@)";
        let ast = jpx_core::parse(expression).unwrap();
        assert_eq!(
            admit_with_limits(&data, &ast, expression, 128 * 1024, WORK)
                .unwrap_err()
                .kind(),
            QueryErrorKind::ResourceLimit
        );
        admit_with_limits(&data, &ast, expression, COPY_BYTES, WORK).unwrap();
    }

    #[test]
    fn expression_reference_amplification_and_work_are_accounted() {
        for expression in [
            "map(&[@,@], @)",
            "sort_by(@, &length([@,@]))",
            "map(&map(&[@,@], @), @)",
        ] {
            let ast = jpx_core::parse(expression).unwrap();
            let data = json!([["x".repeat(1024)], ["y".repeat(1024)]]);
            assert!(admit_with_limits(&data, &ast, expression, 64 * 1024, WORK).is_err());
        }
        let ast = jpx_core::parse("[*].id").unwrap();
        assert!(admit_with_limits(&json!([{}, {}, {}]), &ast, "[*].id", COPY_BYTES, 2).is_err());
    }

    #[test]
    fn bounds_dominate_safe_native_results_including_selected_element_width() {
        let data = json!({
            "rows": [
                {"id": "b", "rank": 2, "extra": ["é", "abc"]},
                {"id": "a", "rank": 1, "extra": ["longer", "x"]}
            ],
            "words": ["a", "é", "abcdef"],
            "numbers": [1, 2, 3]
        });
        for expression in [
            "@",
            "rows[0]",
            "rows[-1]",
            "rows[::-1]",
            "rows[*].id",
            "rows[].extra[]",
            "rows[?rank > `0`].{id: id, extra: extra}",
            "[rows, words, numbers]",
            "rows || words",
            "rows && words",
            "!rows",
            "rows[0].*",
            "keys(rows[0])",
            "values(rows[0])",
            "map(&[@,@], rows)",
            "sort_by(rows, &rank)",
            "min_by(rows, &rank)",
            "max_by(rows, &rank)",
            "min_by(rows, &rank) | values(@) | [*]",
            "max_by(rows, &rank) | keys(@)",
            "merge(rows[0], rows[1])",
            "not_null(missing, rows)",
            "reverse(words)",
            "sort(words)",
            "max(words)",
            "min(words)",
            "join('abc', words)",
            "to_array(rows[0])",
            "to_string(@)",
            "to_number('123')",
            "contains(words, 'a')",
            "type(rows)",
            "length(rows)",
            "abs(`-1`)",
            "ceil(`1.5`)",
            "floor(`1.5`)",
            "avg(numbers)",
            "sum(numbers)",
            "starts_with('abc', 'a')",
            "ends_with('abc', 'c')",
        ] {
            let ast = jpx_core::parse(expression).unwrap();
            let mut budget = Budget {
                bytes: COPY_BYTES,
                work: WORK,
                ast_bytes: (expression.len() as u64) * 512,
            };
            let input = budget.measure(&data, 1).unwrap();
            let bound = budget.evaluate(&ast, input, 1, 1).unwrap();
            let runtime = crate::functions::runtime(false);
            let result = jpx_core::Expression::new(expression, ast.clone(), &runtime)
                .search(&data)
                .unwrap();
            let actual = budget.measure(&result, 1).unwrap();
            assert!(bound.size >= actual.size, "size: {expression}");
            assert!(bound.len >= actual.len, "width: {expression}");
            assert!(bound.child >= actual.child, "child: {expression}");
            assert!(bound.depth >= actual.depth, "depth: {expression}");
        }
    }
}
