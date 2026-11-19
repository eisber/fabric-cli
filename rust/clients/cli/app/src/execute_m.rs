// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Request-scoped M expression wrapping; this is not an M semantic validator.

pub(super) fn document(name: &str, expression: &str) -> Option<String> {
    if name.trim().is_empty()
        || name.chars().any(char::is_control)
        || expression.len() > super::MAX_INPUT_BYTES
        || expression.contains('\0')
        || !single_expression(expression)
    {
        return None;
    }
    let identifier = name.replace("#(", "#(#)(").replace('"', "\"\"");
    // Newlines prevent a trailing line comment from consuming the terminator.
    Some(format!(
        "section Section1;\nshared #\"{identifier}\" =\n{expression}\n;\n"
    ))
}

fn single_expression(expression: &str) -> bool {
    let bytes = expression.as_bytes();
    let mut index = 0;
    let mut delimiters = Vec::new();
    let mut first = true;
    while index < bytes.len() {
        let current = bytes[index];
        let character = expression[index..]
            .chars()
            .next()
            .expect("remaining character");
        if character.is_whitespace() {
            index += character.len_utf8();
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() {
                let character = expression[index..]
                    .chars()
                    .next()
                    .expect("comment character");
                if matches!(character, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}') {
                    break;
                }
                index += character.len_utf8();
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut depth = 1;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    return false;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if depth != 0 {
                return false;
            }
            continue;
        }
        if first {
            if bytes[index..].starts_with(b"section")
                && bytes.get(index + 7).is_none_or(|next| {
                    !next.is_ascii_alphanumeric() && !matches!(*next, b'_' | b'.')
                })
            {
                return false;
            }
            first = false;
        }
        if current == b'"' {
            index += 1;
            loop {
                match bytes.get(index) {
                    None => return false,
                    Some(b'"') if bytes.get(index + 1) == Some(&b'"') => index += 2,
                    Some(b'"') => {
                        index += 1;
                        break;
                    }
                    Some(_) => index += 1,
                }
            }
            continue;
        }
        match current {
            b';' => return false,
            b'(' | b'[' | b'{' => delimiters.push(current),
            b')' | b']' | b'}' => {
                let expected = match current {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                if delimiters.pop() != Some(expected) {
                    return false;
                }
            }
            _ => {
                index += character.len_utf8();
                continue;
            }
        }
        index += 1;
    }
    !first && delimiters.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_expressions_without_interpreting_strings_or_comments() {
        for expression in [
            "42",
            "let Source = #table({\"雪\"}, {{null}}) in Source",
            "/* comment */ \"section; shared Orders = \"\"x\"\";\" // end",
            "[Name = #\"section\", Value = \"#(lf)\"]",
        ] {
            let actual = document("Orders", expression).unwrap();
            assert!(actual.contains(&format!("=\n{expression}\n;\n")));
        }
    }

    #[test]
    fn rejects_documents_terminators_and_unclosed_lexical_constructs() {
        for expression in [
            "",
            " \n // no expression",
            "/* unclosed",
            "/* outer /* nested */ */ 1",
            "\"unclosed",
            "([)]",
            "section Section1; shared X = 1;",
            "/* prefix */ section Section1",
            "1; shared Injected = 2",
            "1\0",
        ] {
            assert!(document("Orders", expression).is_none());
        }
    }

    #[test]
    fn query_names_are_literal_m_quoted_identifiers() {
        let actual = document("A\"#(lf)雪", "1").unwrap();
        assert!(actual.starts_with("section Section1;\nshared #\"A\"\"#(#)(lf)雪\" =\n"));
        assert!(document("bad\nname", "1").is_none());
    }

    #[test]
    fn every_m_newline_ends_comments_before_section_syntax() {
        for newline in ["\r", "\n", "\u{85}", "\u{2028}", "\u{2029}"] {
            let injected = format!("1 // comment{newline}; shared Injected = 2");
            assert!(document("Orders", &injected).is_none());
            let valid = format!("let // comment{newline}Source = \"雪\" in Source");
            assert!(document("Orders", &valid).is_some());
            assert!(document("Orders", &format!("{newline} // comment")).is_none());
        }
        assert!(document("Orders", "\u{2009}\u{2028}").is_none());
    }
}
