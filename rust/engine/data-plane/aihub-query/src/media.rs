// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::QueryErrorKind;

pub(crate) fn essence(value: &str) -> Result<&str, QueryErrorKind> {
    let invalid = QueryErrorKind::Protocol;
    if value.is_empty()
        || value.len() > 256
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
    {
        return Err(invalid);
    }
    let value = value.trim_matches([' ', '\t']);
    let (media, mut parameters) = value.split_once(';').unwrap_or((value, ""));
    let media = media.trim_end_matches([' ', '\t']);
    let (kind, subtype) = media.split_once('/').ok_or(invalid)?;
    if !token(kind) || !token(subtype) || kind == "*" || subtype == "*" {
        return Err(invalid);
    }
    if value.ends_with(';') {
        return Err(invalid);
    }
    while !parameters.is_empty() {
        parameters = parameters.trim_start_matches([' ', '\t']);
        let (name, rest) = parameters.split_once('=').ok_or(invalid)?;
        if !token(name.trim_end_matches([' ', '\t'])) {
            return Err(invalid);
        }
        let rest = rest.trim_start_matches([' ', '\t']);
        let tail = if let Some(quoted) = rest.strip_prefix('"') {
            let mut escaped = false;
            let mut end = None;
            for (index, byte) in quoted.bytes().enumerate() {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    end = Some(index);
                    break;
                }
            }
            &quoted[end.ok_or(invalid)? + 1..]
        } else {
            let end = rest.find([';', ' ', '\t']).unwrap_or(rest.len());
            if !token(&rest[..end]) {
                return Err(invalid);
            }
            &rest[end..]
        };
        let tail = tail.trim_start_matches([' ', '\t']);
        if tail.is_empty() {
            break;
        }
        parameters = tail.strip_prefix(';').ok_or(invalid)?;
        if parameters.trim_matches([' ', '\t']).is_empty() {
            return Err(invalid);
        }
    }
    Ok(media)
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn validated_essence_ignores_parameter_syntax_and_casing_without_sniffing() {
        for value in [
            "TeXt/CsV; charset=utf-8",
            "TeXt/CsV; filename=\"part;one.csv\"; CHARSET=\"utf-8\"",
            " TeXt/CsV ; filename=\"quoted\\\"name.csv\" ",
        ] {
            assert!(essence(value).unwrap().eq_ignore_ascii_case("text/csv"));
        }
        assert_eq!(essence("application/json").unwrap(), "application/json");
    }

    #[test]
    fn malformed_or_unsafe_media_values_are_not_normalized_into_valid_resources() {
        for value in [
            "",
            "csv",
            "text/",
            "/csv",
            "*/*",
            "text /csv",
            "text/csv;",
            "text/csv; ",
            "text/csv; charset",
            "text/csv; charset=",
            "text/csv; charset=\"unterminated",
            "text/csv; charset=\"utf-8\"garbage",
            "text/csv\r\nx-header: y",
            "text/csv; charset=not valid",
        ] {
            assert!(essence(value).is_err(), "{value:?}");
        }
    }
}
