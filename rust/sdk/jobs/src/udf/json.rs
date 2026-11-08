// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{UdfJsonLimits, UdfValidationError};
use serde_json::Value;
use std::collections::BTreeSet;

enum Frame {
    Object {
        keys: BTreeSet<String>,
        state: ObjectState,
    },
    Array(ArrayState),
}

#[derive(Clone, Copy)]
enum ObjectState {
    FirstKey,
    Key,
    Colon,
    Value,
    End,
}

#[derive(Clone, Copy)]
enum ArrayState {
    FirstValue,
    Value,
    End,
}

pub(super) fn parse(bytes: &[u8], limits: UdfJsonLimits) -> Result<Value, UdfValidationError> {
    if bytes.len() > limits.max_bytes.get() {
        return Err(UdfValidationError::JsonLimit);
    }
    let mut scan = Scan {
        bytes,
        at: 0,
        nodes: 0,
        limits,
    };
    let mut stack = Vec::new();
    scan.value(&mut stack)?;
    while let Some(frame) = stack.last_mut() {
        scan.space();
        match frame {
            Frame::Object { keys, state } => match *state {
                ObjectState::FirstKey if scan.take(b'}') => {
                    stack.pop();
                }
                ObjectState::FirstKey | ObjectState::Key => {
                    let span = scan.string()?;
                    let key: String = serde_json::from_slice(span)
                        .map_err(|_| UdfValidationError::InvalidJson)?;
                    scan.node()?;
                    if !keys.insert(key) {
                        return Err(UdfValidationError::DuplicateKey);
                    }
                    *state = ObjectState::Colon;
                }
                ObjectState::Colon => {
                    scan.expect(b':')?;
                    *state = ObjectState::Value;
                }
                ObjectState::Value => {
                    *state = ObjectState::End;
                    scan.value(&mut stack)?;
                }
                ObjectState::End if scan.take(b'}') => {
                    stack.pop();
                }
                ObjectState::End => {
                    scan.expect(b',')?;
                    *state = ObjectState::Key;
                }
            },
            Frame::Array(state) => match *state {
                ArrayState::FirstValue if scan.take(b']') => {
                    stack.pop();
                }
                ArrayState::FirstValue | ArrayState::Value => {
                    *state = ArrayState::End;
                    scan.value(&mut stack)?;
                }
                ArrayState::End if scan.take(b']') => {
                    stack.pop();
                }
                ArrayState::End => {
                    scan.expect(b',')?;
                    *state = ArrayState::Value;
                }
            },
        }
    }
    scan.space();
    if scan.at != bytes.len() {
        return Err(UdfValidationError::InvalidJson);
    }
    serde_json::from_slice(bytes).map_err(|_| UdfValidationError::InvalidJson)
}

struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
    nodes: usize,
    limits: UdfJsonLimits,
}

impl<'a> Scan<'a> {
    fn node(&mut self) -> Result<(), UdfValidationError> {
        if self.nodes == self.limits.max_nodes.get() {
            return Err(UdfValidationError::JsonLimit);
        }
        self.nodes += 1;
        Ok(())
    }

    fn space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), UdfValidationError> {
        self.take(byte)
            .then_some(())
            .ok_or(UdfValidationError::InvalidJson)
    }

    fn string(&mut self) -> Result<&'a [u8], UdfValidationError> {
        let start = self.at;
        self.expect(b'"')?;
        while let Some(byte) = self.bytes.get(self.at).copied() {
            self.at += 1;
            match byte {
                b'"' => return Ok(&self.bytes[start..self.at]),
                b'\\' if self.at < self.bytes.len() => self.at += 1,
                0..=31 => return Err(UdfValidationError::InvalidJson),
                _ => {}
            }
        }
        Err(UdfValidationError::InvalidJson)
    }

    fn value(&mut self, stack: &mut Vec<Frame>) -> Result<(), UdfValidationError> {
        self.space();
        self.node()?;
        let container = match self.bytes.get(self.at) {
            Some(b'{') => Some(Frame::Object {
                keys: BTreeSet::new(),
                state: ObjectState::FirstKey,
            }),
            Some(b'[') => Some(Frame::Array(ArrayState::FirstValue)),
            Some(b'"') => {
                self.string()?;
                None
            }
            _ => {
                let start = self.at;
                while self.bytes.get(self.at).is_some_and(|byte| {
                    !matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b',' | b']' | b'}')
                }) {
                    self.at += 1;
                }
                let token = &self.bytes[start..self.at];
                if token.is_empty() {
                    return Err(UdfValidationError::InvalidJson);
                }
                let digits = token.strip_prefix(b"-").unwrap_or(token);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    let text =
                        std::str::from_utf8(token).map_err(|_| UdfValidationError::InvalidJson)?;
                    if text.parse::<i64>().is_err() && text.parse::<u64>().is_err() {
                        return Err(UdfValidationError::IntegerOutOfRange);
                    }
                }
                None
            }
        };
        if let Some(container) = container {
            if stack.len() == self.limits.max_depth.get() {
                return Err(UdfValidationError::JsonLimit);
            }
            self.at += 1;
            stack.push(container);
        }
        Ok(())
    }
}
