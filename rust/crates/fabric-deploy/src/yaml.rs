// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded YAML event adapter with source-specific config and parameter policies.
//!
//! The parser is a syntax primitive, not a PyYAML-equivalence claim. Unsupported
//! scalar domains fail explicitly before conversion to the workspace JSON domain.

use crate::deployment_config::ConfigurationError;
use serde_json::{Map, Number, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};
use yaml_rust2::{
    parser::{Event, Parser, Tag},
    scanner::TScalarStyle,
};

/// Caller-owned finite YAML allocation and expansion policy.
#[derive(Clone, Copy, Debug)]
pub struct YamlLimits {
    /// Maximum UTF-8 input bytes and expanded scalar bytes.
    pub max_bytes: usize,
    /// Maximum parsed and expanded nodes, including aliases.
    pub max_nodes: usize,
    /// Maximum collection nesting and alias expansion depth.
    pub max_depth: usize,
}

impl Default for YamlLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024,
            max_nodes: 50_000,
            max_depth: 64,
        }
    }
}

/// Source config duplicate/merge policy is distinct from parameter YAML.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum YamlPolicy {
    /// Exact repeated string keys take the last value; safe merges are expanded.
    Config,
    /// Case-insensitive duplicates are rejected before map collapse; merges fail.
    Parameter,
}

enum Node {
    Scalar(String, TScalarStyle, Option<Tag>),
    Sequence(Vec<Arc<Node>>),
    Mapping(Vec<(Arc<Node>, Arc<Node>)>),
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    anchors: HashMap<usize, Arc<Node>>,
    limits: YamlLimits,
    nodes: usize,
}

fn invalid(message: &str) -> ConfigurationError {
    ConfigurationError::Invalid(vec![message.into()])
}

fn unsupported(branch: &str) -> ConfigurationError {
    ConfigurationError::Unsupported(branch.into())
}

impl Reader<'_> {
    fn event(&mut self) -> Result<Event, ConfigurationError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .filter(|count| *count <= self.limits.max_nodes)
            .ok_or(ConfigurationError::Limit)?;
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|_| invalid("Invalid deployment YAML syntax"))
    }

    fn node(&mut self, event: Event, depth: usize) -> Result<Arc<Node>, ConfigurationError> {
        if depth > self.limits.max_depth {
            return Err(ConfigurationError::Limit);
        }
        let (node, anchor) = match event {
            Event::Alias(anchor) => {
                return self
                    .anchors
                    .get(&anchor)
                    .cloned()
                    .ok_or_else(|| unsupported("recursive or unresolved YAML anchor"));
            }
            Event::Scalar(text, style, anchor, tag) => (Node::Scalar(text, style, tag), anchor),
            Event::SequenceStart(anchor, tag) => {
                collection_tag(tag.as_ref(), "seq")?;
                let mut values = Vec::new();
                loop {
                    let event = self.event()?;
                    if event == Event::SequenceEnd {
                        break;
                    }
                    values.push(self.node(event, depth + 1)?);
                }
                (Node::Sequence(values), anchor)
            }
            Event::MappingStart(anchor, tag) => {
                collection_tag(tag.as_ref(), "map")?;
                let mut values = Vec::new();
                loop {
                    let event = self.event()?;
                    if event == Event::MappingEnd {
                        break;
                    }
                    let key = self.node(event, depth + 1)?;
                    let event = self.event()?;
                    let value = self.node(event, depth + 1)?;
                    values.push((key, value));
                }
                (Node::Mapping(values), anchor)
            }
            _ => return Err(invalid("Invalid deployment YAML structure")),
        };
        let node = Arc::new(node);
        if anchor != 0 {
            self.anchors.insert(anchor, node.clone());
        }
        Ok(node)
    }
}

fn collection_tag(tag: Option<&Tag>, expected: &str) -> Result<(), ConfigurationError> {
    if tag.is_some_and(|tag| tag.handle != "tag:yaml.org,2002:" || tag.suffix != expected) {
        return Err(unsupported("YAML collection tag"));
    }
    Ok(())
}

struct Budget {
    limits: YamlLimits,
    nodes: usize,
    bytes: usize,
}

impl Budget {
    fn visit(&mut self, depth: usize) -> Result<(), ConfigurationError> {
        self.nodes = self.nodes.checked_add(1).ok_or(ConfigurationError::Limit)?;
        if depth > self.limits.max_depth || self.nodes > self.limits.max_nodes {
            return Err(ConfigurationError::Limit);
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), ConfigurationError> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or(ConfigurationError::Limit)?;
        if self.bytes > self.limits.max_bytes {
            return Err(ConfigurationError::Limit);
        }
        Ok(())
    }
}

/// Decode source-compatible representable YAML without ambient environment or I/O.
///
/// Dates/timestamps, non-finite/out-of-range numbers, complex/non-string keys and
/// custom tags remain typed unsupported, never coerced into successful JSON.
///
/// # Errors
/// Rejects invalid syntax, multiple documents, parameter duplicate/merge errors,
/// unsupported scalar domains, and bounded expansion/depth/allocation overflow.
pub fn decode(
    input: &str,
    policy: YamlPolicy,
    limits: YamlLimits,
) -> Result<Value, ConfigurationError> {
    if input.len() > limits.max_bytes {
        return Err(ConfigurationError::Limit);
    }
    let mut reader = Reader {
        parser: Parser::new_from_str(input),
        anchors: HashMap::new(),
        limits,
        nodes: 0,
    };
    if reader.event()? != Event::StreamStart {
        return Err(invalid("Invalid YAML stream"));
    }
    let first = reader.event()?;
    if first == Event::StreamEnd {
        return Ok(Value::Null);
    }
    if first != Event::DocumentStart {
        return Err(invalid("Invalid YAML document"));
    }
    let event = reader.event()?;
    let node = reader.node(event, 0)?;
    if reader.event()? != Event::DocumentEnd || reader.event()? != Event::StreamEnd {
        return Err(invalid("Expected exactly one YAML document"));
    }
    if policy == YamlPolicy::Parameter {
        parameter_duplicates(&node, limits)?;
    }
    convert(
        &node,
        policy,
        &mut Budget {
            limits,
            nodes: 0,
            bytes: 0,
        },
        0,
    )
}

fn parameter_duplicates(root: &Arc<Node>, limits: YamlLimits) -> Result<(), ConfigurationError> {
    let mut budget = Budget {
        limits,
        nodes: 0,
        bytes: 0,
    };
    let mut stack = vec![(root.clone(), 0)];
    let mut errors = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        budget.visit(depth)?;
        let Node::Mapping(entries) = node.as_ref() else {
            continue;
        };
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        let mut positions = HashMap::new();
        for (key, value) in entries {
            if merge_key(key) {
                return Err(invalid(
                    "could not determine a constructor for the tag 'tag:yaml.org,2002:merge'",
                ));
            }
            let key = convert(key, YamlPolicy::Config, &mut budget, depth + 1)?;
            let (identity, spelling) = match key {
                Value::String(value) => (format!("s:{}", value.to_lowercase()), value),
                Value::Bool(value) => (
                    format!("n:{}", u8::from(value)),
                    if value { "True" } else { "False" }.into(),
                ),
                Value::Number(value) => {
                    let identity = value
                        .as_f64()
                        .filter(|_| value.is_f64())
                        .filter(|value| value.fract() == 0.0)
                        .map_or_else(|| value.to_string(), |value| format!("{value:.0}"));
                    (format!("n:{identity}"), value.to_string())
                }
                Value::Null => ("null".into(), "None".into()),
                _ => return Err(unsupported("complex YAML mapping key")),
            };
            if let Some(index) = positions.get(&identity).copied() {
                let (_, variants): &mut (String, Vec<String>) = &mut groups[index];
                variants.push(spelling);
            } else {
                positions.insert(identity.clone(), groups.len());
                groups.push((identity, vec![spelling]));
            }
            match value.as_ref() {
                Node::Mapping(_) => stack.push((value.clone(), depth + 1)),
                Node::Sequence(children) => {
                    for child in children {
                        if matches!(child.as_ref(), Node::Mapping(_)) {
                            stack.push((child.clone(), depth + 2));
                        }
                    }
                }
                _ => {}
            }
        }
        for (_, variants) in groups {
            if variants.len() > 1 {
                let unique: BTreeSet<_> = variants.iter().collect();
                if unique.len() == 1 {
                    errors.push(format!(
                        "'{}' ({})",
                        unique.first().expect("nonempty variants"),
                        variants.len()
                    ));
                } else {
                    let quoted = unique
                        .into_iter()
                        .map(|value| {
                            let quote = if value.contains('\'') && !value.contains('"') {
                                '"'
                            } else {
                                '\''
                            };
                            let escaped = value
                                .replace('\\', "\\\\")
                                .replace('\n', "\\n")
                                .replace('\r', "\\r")
                                .replace('\t', "\\t")
                                .replace(quote, &format!("\\{quote}"));
                            format!("{quote}{escaped}{quote}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    errors.push(format!("[{quoted}] ({})", variants.len()));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(invalid(&format!(
            "duplicate key(s) found: {}",
            errors.join(", ")
        )))
    }
}

fn merge_key(node: &Node) -> bool {
    matches!(node, Node::Scalar(value, TScalarStyle::Plain, None) if value == "<<")
        || matches!(node, Node::Scalar(_, _, Some(tag)) if tag.handle == "tag:yaml.org,2002:" && tag.suffix == "merge")
}

fn convert(
    node: &Node,
    policy: YamlPolicy,
    budget: &mut Budget,
    depth: usize,
) -> Result<Value, ConfigurationError> {
    budget.visit(depth)?;
    match node {
        Node::Scalar(text, style, tag) => {
            budget.text(text)?;
            scalar(text, *style, tag.as_ref())
        }
        Node::Sequence(values) => values
            .iter()
            .map(|value| convert(value, policy, budget, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Node::Mapping(entries) => {
            let mut merged = Map::new();
            let mut direct = Vec::new();
            for (key, value) in entries {
                if merge_key(key) {
                    if policy == YamlPolicy::Parameter {
                        return Err(invalid(
                            "could not determine a constructor for the tag 'tag:yaml.org,2002:merge'",
                        ));
                    }
                    let value = convert(value, policy, budget, depth + 1)?;
                    let sources = match value {
                        Value::Object(mapping) => vec![mapping],
                        Value::Array(values) => values
                            .into_iter()
                            .rev()
                            .map(|value| {
                                value
                                    .as_object()
                                    .cloned()
                                    .ok_or_else(|| invalid("YAML merge requires a mapping"))
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        _ => {
                            return Err(invalid(
                                "YAML merge requires a mapping or mapping sequence",
                            ));
                        }
                    };
                    for source in sources {
                        merged.extend(source);
                    }
                    continue;
                }
                let key = convert(key, policy, budget, depth + 1)?;
                direct.push((key, value));
            }
            for (key, value) in direct {
                let key = key
                    .as_str()
                    .ok_or_else(|| unsupported("non-string YAML mapping key"))?;
                merged.insert(key.into(), convert(value, policy, budget, depth + 1)?);
            }
            Ok(Value::Object(merged))
        }
    }
}

fn scalar(text: &str, style: TScalarStyle, tag: Option<&Tag>) -> Result<Value, ConfigurationError> {
    if let Some(tag) = tag {
        if tag.handle != "tag:yaml.org,2002:" {
            return Err(unsupported("custom YAML scalar tag"));
        }
        if tag.suffix == "str" {
            return Ok(Value::String(text.into()));
        }
        if !matches!(tag.suffix.as_str(), "bool" | "int" | "float" | "null") {
            return Err(unsupported("YAML scalar tag outside the JSON domain"));
        }
        return Err(unsupported("explicit non-string YAML scalar tags"));
    }
    if style != TScalarStyle::Plain {
        return Ok(Value::String(text.into()));
    }
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Ok(Value::Null),
        "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => {
            return Ok(Value::Bool(true));
        }
        "false" | "False" | "FALSE" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => {
            return Ok(Value::Bool(false));
        }
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" | "-.inf" | "-.Inf" | "-.INF"
        | ".nan" | ".NaN" | ".NAN" => return Err(unsupported("non-finite YAML float")),
        _ => {}
    }
    if text.len() >= 10
        && text.as_bytes()[4] == b'-'
        && text.as_bytes()[7] == b'-'
        && text[..4].bytes().all(|ch| ch.is_ascii_digit())
    {
        return Err(unsupported("YAML date or timestamp"));
    }
    let raw_unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if !raw_unsigned
        .as_bytes()
        .first()
        .is_some_and(|ch| ch.is_ascii_digit() || *ch == b'.')
    {
        return Ok(Value::String(text.into()));
    }
    let clean = text.replace('_', "");
    let unsigned = clean.strip_prefix(['+', '-']).unwrap_or(&clean);
    if unsigned.is_empty() {
        return Ok(Value::String(text.into()));
    }
    let integer = if let Some(digits) = unsigned
        .strip_prefix("0x")
        .filter(|_| raw_unsigned.starts_with("0x"))
    {
        (!digits.is_empty() && digits.bytes().all(|ch| ch.is_ascii_hexdigit()))
            .then_some((digits, 16))
    } else if let Some(digits) = unsigned
        .strip_prefix("0b")
        .filter(|_| raw_unsigned.starts_with("0b"))
    {
        (!digits.is_empty() && digits.bytes().all(|ch| matches!(ch, b'0' | b'1')))
            .then_some((digits, 2))
    } else if unsigned.starts_with('0') && unsigned.len() > 1 {
        unsigned
            .bytes()
            .all(|ch| matches!(ch, b'0'..=b'7'))
            .then_some((unsigned, 8))
    } else {
        unsigned
            .bytes()
            .all(|ch| ch.is_ascii_digit())
            .then_some((unsigned, 10))
    };
    if let Some((digits, radix)) = integer {
        let number = u64::from_str_radix(digits, radix)
            .map_err(|_| unsupported("YAML integer outside the JSON number domain"))?;
        return if clean.starts_with('-') {
            let signed = i128::from(number)
                .checked_neg()
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| unsupported("YAML integer outside the JSON number domain"))?;
            Ok(Value::Number(signed.into()))
        } else {
            Ok(Value::Number(number.into()))
        };
    }
    if unsigned.contains(':')
        && unsigned
            .as_bytes()
            .first()
            .is_some_and(|ch| matches!(ch, b'1'..=b'9'))
    {
        let components: Vec<_> = raw_unsigned.split(':').collect();
        if components[0]
            .bytes()
            .all(|ch| ch.is_ascii_digit() || ch == b'_')
            && components[1..].iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 2
                    && part.bytes().all(|ch| ch.is_ascii_digit())
                    && part.parse::<u8>().is_ok_and(|value| value < 60)
            })
        {
            let number = components
                .iter()
                .try_fold(0_u64, |sum, part| {
                    sum.checked_mul(60)?
                        .checked_add(part.replace('_', "").parse::<u64>().ok()?)
                })
                .ok_or_else(|| unsupported("YAML integer outside the JSON number domain"))?;
            return if clean.starts_with('-') {
                i64::try_from(number)
                    .ok()
                    .and_then(i64::checked_neg)
                    .map(|value| Value::Number(value.into()))
                    .ok_or_else(|| unsupported("YAML integer outside the JSON number domain"))
            } else {
                Ok(Value::Number(number.into()))
            };
        }
    }
    if unsigned.contains('.') {
        if unsigned.contains(':') {
            return Err(unsupported("sexagesimal YAML float"));
        }
        let mantissa = unsigned.split(['e', 'E']).next().expect("nonempty scalar");
        let exponent = unsigned
            .find(['e', 'E'])
            .map(|index| &unsigned[index + 1..]);
        let valid_exponent = exponent.is_none_or(|value| {
            value.strip_prefix(['+', '-']).is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|ch| ch.is_ascii_digit())
            })
        });
        if mantissa.matches('.').count() == 1
            && mantissa.bytes().any(|ch| ch.is_ascii_digit())
            && mantissa.bytes().all(|ch| ch.is_ascii_digit() || ch == b'.')
            && valid_exponent
        {
            let number = clean
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .ok_or_else(|| unsupported("YAML float outside the finite JSON number domain"))?;
            return Ok(Value::Number(number));
        }
    }
    Ok(Value::String(text.into()))
}
