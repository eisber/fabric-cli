// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{DecimalInteger, PythonString, PythonValue};
use serde_json::{Value, map};
use std::{slice, str};

#[derive(Clone, Copy)]
pub(crate) enum ValueRef<'a> {
    Json(&'a Value),
    Python(&'a PythonValue),
}

pub(crate) enum Node<'a> {
    None,
    Bool(bool),
    Number(Number<'a>),
    String(Text<'a>),
    List(ListIter<'a>),
    Dict(DictIter<'a>),
}

pub(crate) enum Number<'a> {
    Json(&'a serde_json::Number),
    Integer(&'a DecimalInteger),
    Float(f64),
}

pub(crate) enum Text<'a> {
    Scalar(&'a str),
    Python(&'a PythonString),
}

impl Text<'_> {
    pub(crate) fn points(&self) -> Points<'_> {
        match self {
            Self::Scalar(value) => Points::Scalar(value.chars()),
            Self::Python(value) => Points::Python(value.codepoints().iter()),
        }
    }
}

pub(crate) enum Points<'a> {
    Scalar(str::Chars<'a>),
    Python(slice::Iter<'a, u32>),
}

impl Iterator for Points<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        match self {
            Self::Scalar(values) => values.next().map(u32::from),
            Self::Python(values) => values.next().copied(),
        }
    }
}

pub(crate) enum ListIter<'a> {
    Json(slice::Iter<'a, Value>),
    Python(slice::Iter<'a, PythonValue>),
}

impl<'a> Iterator for ListIter<'a> {
    type Item = ValueRef<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Json(values) => values.next().map(ValueRef::Json),
            Self::Python(values) => values.next().map(ValueRef::Python),
        }
    }
}

pub(crate) enum DictIter<'a> {
    Json(map::Iter<'a>),
    Python(slice::Iter<'a, (PythonString, PythonValue)>),
}

impl<'a> Iterator for DictIter<'a> {
    type Item = (Text<'a>, ValueRef<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Json(values) => values
                .next()
                .map(|(k, v)| (Text::Scalar(k), ValueRef::Json(v))),
            Self::Python(values) => values
                .next()
                .map(|(k, v)| (Text::Python(k), ValueRef::Python(v))),
        }
    }
}

impl<'a> ValueRef<'a> {
    pub(crate) fn node(self) -> Node<'a> {
        match self {
            Self::Json(value) => match value {
                Value::Null => Node::None,
                Value::Bool(value) => Node::Bool(*value),
                Value::Number(value) => Node::Number(Number::Json(value)),
                Value::String(value) => Node::String(Text::Scalar(value)),
                Value::Array(values) => Node::List(ListIter::Json(values.iter())),
                Value::Object(values) => Node::Dict(DictIter::Json(values.iter())),
            },
            Self::Python(value) => match value {
                PythonValue::None => Node::None,
                PythonValue::Bool(value) => Node::Bool(*value),
                PythonValue::Integer(value) => Node::Number(Number::Integer(value)),
                PythonValue::Float(value) => Node::Number(Number::Float(*value)),
                PythonValue::String(value) => Node::String(Text::Python(value)),
                PythonValue::List(values) => Node::List(ListIter::Python(values.iter())),
                PythonValue::Dict(values) => Node::Dict(DictIter::Python(values.entries().iter())),
            },
        }
    }
}
