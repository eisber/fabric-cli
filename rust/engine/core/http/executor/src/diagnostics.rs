// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Parse raw metadata before it can enter any diagnostic event.

use fabric_core::{
    Audience, Endpoint,
    diagnostics::{
        ContentEncoding, ContentType, DiagnosticDuration, DiagnosticOrigin, DiagnosticRoute,
        Metadata, RequestId, SafeHeaders, SafeUrl,
    },
    resolve_operation_url_with_endpoint,
};
use std::{collections::BTreeMap, time::SystemTime};

#[cfg(test)]
pub(crate) fn safe_url(audience: Audience, value: &str) -> Option<SafeUrl> {
    safe_url_with_endpoint(&Endpoint::public(audience), value)
}

pub(crate) fn safe_url_with_endpoint(endpoint: &Endpoint, value: &str) -> Option<SafeUrl> {
    let audience = endpoint.audience();
    let url = resolve_operation_url_with_endpoint(endpoint, value, endpoint.base_url()).ok()?;
    let origin = if url.origin() == endpoint.base_url().origin() {
        match audience {
            Audience::Fabric => DiagnosticOrigin::Fabric,
            Audience::Storage => DiagnosticOrigin::Storage,
            Audience::Azure => DiagnosticOrigin::Azure,
            Audience::PowerBi => DiagnosticOrigin::PowerBi,
        }
    } else {
        DiagnosticOrigin::ServiceAdvertised
    };
    Some(SafeUrl::new(origin, route(audience, url.path())))
}

fn route(audience: Audience, path: &str) -> DiagnosticRoute {
    use DiagnosticRoute::*;
    // Encoded spellings never become fixed route labels. Variable segments are
    // discarded even when they look nothing like resource UUIDs.
    let segments: Vec<_> = path.split('/').collect();
    if segments.iter().skip(1).any(|segment| segment.is_empty()) && path != "/" {
        return Redacted;
    }
    let route = match segments.as_slice() {
        ["", ""] => Root,
        ["", "v1"] => FabricRoot,
        ["", "v1", "workspaces"] => Workspaces,
        ["", "v1", "workspaces", _] => Workspace,
        ["", "v1", "workspaces", _, "items"] => Items,
        ["", "v1", "workspaces", _, "items", _] => Item,
        ["", "v1", "workspaces", _, "items", _, "jobs", "instances"] => JobInstances,
        [
            "",
            "v1",
            "workspaces",
            _,
            "items",
            _,
            "jobs",
            "instances",
            _,
        ] => JobInstance,
        ["", "v1", "operations", _] => Operation,
        ["", "v1", "operations", _, "result"] => OperationResult,
        ["", "v1", "capacities"] => Capacities,
        ["", "v1", "capacities", _] => Capacity,
        ["", "subscriptions"] => Subscriptions,
        ["", "subscriptions", _] => Subscription,
        ["", "subscriptions", _, "resourceGroups"] => ResourceGroups,
        ["", "subscriptions", _, "resourceGroups", _] => ResourceGroup,
        ["", "v1.0", "myorg"] => PowerBiRoot,
        ["", "v1.0", "myorg", "groups"] => Groups,
        ["", "v1.0", "myorg", "groups", _] => Group,
        _ => Redacted,
    };
    match (audience, route) {
        (_, Root) => Root,
        (
            Audience::Fabric,
            FabricRoot | Workspaces | Workspace | Items | Item | JobInstances | JobInstance
            | Operation | OperationResult | Capacities | Capacity,
        ) => route,
        (Audience::Azure, Subscriptions | Subscription | ResourceGroups | ResourceGroup) => route,
        (Audience::PowerBi, PowerBiRoot | Groups | Group) => route,
        _ => Redacted,
    }
}

fn field<T>(
    headers: &BTreeMap<String, String>,
    name: &str,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Metadata<T> {
    let mut matching = headers
        .iter()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(name));
    let Some((_, value)) = matching.next() else {
        return Metadata::Absent;
    };
    if matching.next().is_some() || value.len() > 16 * 1024 || value.chars().any(char::is_control) {
        return Metadata::Omitted;
    }
    parse(value).map_or(Metadata::Omitted, Metadata::Value)
}

pub(crate) fn safe_request_headers(headers: &BTreeMap<String, String>) -> SafeHeaders {
    SafeHeaders::new(
        field(headers, "content-type", |value| {
            // Native ingress joins repeated non-control fields with a comma.
            // A duplicate may otherwise hide after the first media parameter.
            if value.contains(',') {
                return None;
            }
            match value
                .split(';')
                .next()?
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "application/json" => Some(ContentType::Json),
                "application/problem+json" => Some(ContentType::ProblemJson),
                "text/plain" => Some(ContentType::Text),
                "application/octet-stream" => Some(ContentType::Binary),
                "multipart/form-data" => Some(ContentType::Multipart),
                _ => None,
            }
        }),
        field(headers, "content-length", |value| {
            (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| value.parse().ok())
                .flatten()
        }),
        field(headers, "content-encoding", |value| {
            match value.to_ascii_lowercase().as_str() {
                "identity" => Some(ContentEncoding::Identity),
                "gzip" => Some(ContentEncoding::Gzip),
                "deflate" => Some(ContentEncoding::Deflate),
                _ => None,
            }
        }),
        Metadata::Absent,
        Metadata::Absent,
        Metadata::Absent,
        Metadata::Absent,
    )
}

#[cfg(test)]
pub(crate) fn safe_response_headers(
    audience: Audience,
    headers: &BTreeMap<String, String>,
    base: &str,
    now: SystemTime,
) -> SafeHeaders {
    safe_response_headers_with_endpoint(&Endpoint::public(audience), headers, base, now)
}

pub(crate) fn safe_response_headers_with_endpoint(
    endpoint: &Endpoint,
    headers: &BTreeMap<String, String>,
    base: &str,
    now: SystemTime,
) -> SafeHeaders {
    let content = safe_request_headers(headers);
    SafeHeaders::new(
        content.content_type(),
        content.content_length(),
        content.content_encoding(),
        field(headers, "retry-after", |value| {
            super::retry_after_value(value, now)
                .ok()
                .map(DiagnosticDuration::from_duration)
        }),
        field(headers, "requestId", RequestId::parse),
        field(headers, "x-ms-request-id", RequestId::parse),
        field(headers, "location", |value| {
            let base =
                resolve_operation_url_with_endpoint(endpoint, base, endpoint.base_url()).ok()?;
            let resolved = resolve_operation_url_with_endpoint(endpoint, value, &base).ok()?;
            safe_url_with_endpoint(endpoint, resolved.as_str())
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANARY: &str = "SECRET-canary";
    const UUID: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn outgoing_metadata_cannot_populate_response_only_header_slots() {
        let headers = BTreeMap::from([
            (
                "Content-Type".into(),
                "application/json; secret=SECRET-canary".into(),
            ),
            ("Content-Length".into(), "42".into()),
            ("Content-Encoding".into(), "gzip".into()),
            ("requestId".into(), UUID.into()),
            ("X-MS-Request-ID".into(), UUID.into()),
            ("Location".into(), "/v1/operations/SECRET-canary".into()),
            ("Retry-After".into(), "123".into()),
            ("Authorization".into(), "SECRET-canary".into()),
            ("Proxy-Authorization".into(), "SECRET-canary".into()),
        ]);
        let safe = safe_request_headers(&headers);
        assert_eq!(safe.content_type(), Metadata::Value(ContentType::Json));
        assert_eq!(safe.content_length(), Metadata::Value(42));
        assert_eq!(
            safe.content_encoding(),
            Metadata::Value(ContentEncoding::Gzip)
        );
        assert_eq!(safe.request_id(), Metadata::Absent);
        assert_eq!(safe.x_ms_request_id(), Metadata::Absent);
        assert_eq!(safe.location(), Metadata::Absent);
        assert_eq!(safe.retry_after(), Metadata::Absent);
        let rendered = format!("{safe:?}");
        assert!(!rendered.contains(UUID));
        assert!(!rendered.contains("SECRET-canary"));
        assert!(!rendered.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn urls_discard_all_dynamic_segments_queries_and_nonstandard_origins() {
        for (value, expected) in [
            (
                "https://api.fabric.microsoft.com/v1/workspaces/SECRET-canary/items/not-a-uuid?key=SECRET-canary",
                "https://api.fabric.microsoft.com/v1/workspaces/{workspaceId}/items/{itemId}",
            ),
            (
                "https://api.fabric.microsoft.com/v1/workspaces/%53ECRET-canary/items/item",
                "https://api.fabric.microsoft.com/v1/workspaces/{workspaceId}/items/{itemId}",
            ),
            (
                "https://api.fabric.microsoft.com/v1/unknown/SECRET-canary",
                "https://api.fabric.microsoft.com/<redacted>",
            ),
            (
                "https://secret-canary.example/v1/operations/SECRET-canary?sig=SECRET-canary",
                "service-advertised/v1/operations/{operationId}",
            ),
            (
                "https://api.fabric.microsoft.com/%76%31/workspaces/SECRET-canary",
                "https://api.fabric.microsoft.com/<redacted>",
            ),
        ] {
            let safe = safe_url(Audience::Fabric, value).unwrap();
            assert_eq!(safe.to_string(), expected);
            assert!(!format!("{safe:?} {safe}").contains(CANARY));
        }
        for value in [
            "https://SECRET-canary@api.fabric.microsoft.com/v1/workspaces",
            "https://api.fabric.microsoft.com/v1/workspaces#SECRET-canary",
            "https://api.fabric.microsoft.com/v1/workspaces/a%2fb",
            "https://api.fabric.microsoft.com/v1/../workspaces",
            "https://api.fabric.microsoft.com/v1/workspaces\nSECRET-canary",
            "https://secret-canary.example/private/SECRET-canary",
            "http://api.fabric.microsoft.com/v1/workspaces",
        ] {
            assert!(safe_url(Audience::Fabric, value).is_none());
        }
    }

    #[test]
    fn headers_use_fixed_labels_and_never_receive_protected_names_or_raw_values() {
        let headers = BTreeMap::from([
            ("Authorization".into(), CANARY.into()),
            ("pRoXy-AuThOrIzAtIoN".into(), CANARY.into()),
            ("Cookie".into(), CANARY.into()),
            (
                "Content-Type".into(),
                format!("application/json; boundary={CANARY}"),
            ),
            ("Content-Length".into(), "42".into()),
            ("Content-Encoding".into(), "gzip".into()),
            ("Retry-After".into(), "5".into()),
            ("requestId".into(), UUID.into()),
            ("X-MS-REQUEST-ID".into(), UUID.into()),
            (
                "Location".into(),
                format!("/v1/operations/{CANARY}?secret={CANARY}"),
            ),
        ]);
        let safe = safe_response_headers(
            Audience::Fabric,
            &headers,
            Audience::Fabric.base_url(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(safe.content_type(), Metadata::Value(ContentType::Json));
        assert_eq!(safe.content_length(), Metadata::Value(42));
        assert_eq!(
            safe.content_encoding(),
            Metadata::Value(ContentEncoding::Gzip)
        );
        assert_eq!(safe.retry_after().to_string(), "5000 ms");
        assert_eq!(safe.request_id().to_string(), UUID);
        assert_eq!(safe.x_ms_request_id().to_string(), UUID);
        assert_eq!(
            safe.location().to_string(),
            "https://api.fabric.microsoft.com/v1/operations/{operationId}"
        );
        let debug = format!("{safe:?}").to_ascii_lowercase();
        for forbidden in ["secret-canary", "authorization", "cookie", "boundary", "?"] {
            assert!(!debug.contains(forbidden));
        }
    }

    #[test]
    fn joined_content_type_duplicates_are_not_hidden_by_parameters() {
        let headers = BTreeMap::from([(
            "content-type".into(),
            "application/json; charset=utf-8, application/json; secret=SECRET-canary".into(),
        )]);
        let safe = safe_response_headers(
            Audience::Fabric,
            &headers,
            Audience::Fabric.base_url(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(safe.content_type(), Metadata::Omitted);
    }

    #[test]
    fn ambiguous_header_slots_are_omitted_even_when_duplicate_values_match() {
        let headers = [
            ("Content-Type", "application/json"),
            ("Content-Length", "42"),
            ("Content-Encoding", "gzip"),
            ("Retry-After", "5"),
            ("requestId", UUID),
            ("X-MS-Request-ID", UUID),
            ("Location", "/v1/operations/accepted"),
        ];
        let headers = headers
            .into_iter()
            .flat_map(|(name, value)| {
                [
                    (name.to_owned(), value.to_owned()),
                    (name.to_ascii_lowercase(), value.to_owned()),
                ]
            })
            .collect();
        let safe = safe_response_headers(
            Audience::Fabric,
            &headers,
            Audience::Fabric.base_url(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(safe.content_type(), Metadata::Omitted);
        assert_eq!(safe.content_length(), Metadata::Omitted);
        assert_eq!(safe.content_encoding(), Metadata::Omitted);
        assert_eq!(safe.retry_after(), Metadata::Omitted);
        assert_eq!(safe.request_id(), Metadata::Omitted);
        assert_eq!(safe.x_ms_request_id(), Metadata::Omitted);
        assert_eq!(safe.location(), Metadata::Omitted);
    }

    #[test]
    fn controls_invalid_values_and_unsupported_metadata_never_fall_back_to_text() {
        for value in [
            CANARY,
            "\r\nSECRET-canary",
            "\u{1b}[31m",
            "\t",
            "\u{85}",
            "",
        ] {
            let headers = [
                "Content-Type",
                "Content-Length",
                "Content-Encoding",
                "Retry-After",
                "requestId",
                "X-MS-Request-ID",
                "Location",
            ]
            .into_iter()
            .map(|name| (name.into(), value.into()))
            .collect();
            let safe = safe_response_headers(
                Audience::Fabric,
                &headers,
                Audience::Fabric.base_url(),
                SystemTime::UNIX_EPOCH,
            );
            assert_eq!(safe.content_type(), Metadata::Omitted);
            assert_eq!(safe.content_length(), Metadata::Omitted);
            assert_eq!(safe.content_encoding(), Metadata::Omitted);
            assert_eq!(safe.retry_after(), Metadata::Omitted);
            assert_eq!(safe.request_id(), Metadata::Omitted);
            assert_eq!(safe.x_ms_request_id(), Metadata::Omitted);
            assert_eq!(safe.location(), Metadata::Omitted);
            assert!(!format!("{safe:?}").contains(CANARY));
        }
        let headers = BTreeMap::from([
            ("Content-Type".into(), "application/SECRET-canary".into()),
            ("Content-Encoding".into(), "gzip, SECRET-canary".into()),
            ("Content-Length".into(), "18446744073709551616".into()),
        ]);
        let safe = safe_response_headers(
            Audience::Fabric,
            &headers,
            Audience::Fabric.base_url(),
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(safe.content_type(), Metadata::Omitted);
        assert_eq!(safe.content_length(), Metadata::Omitted);
        assert_eq!(safe.content_encoding(), Metadata::Omitted);
    }

    #[test]
    fn relative_location_keeps_the_approved_origin_but_not_its_hostname() {
        let headers = BTreeMap::from([(
            "Location".into(),
            "/v1/operations/SECRET-canary/result?token=SECRET-canary".into(),
        )]);
        let safe = safe_response_headers(
            Audience::Fabric,
            &headers,
            "https://secret-canary.example/v1/operations/accepted",
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(
            safe.location().to_string(),
            "service-advertised/v1/operations/{operationId}/result"
        );
    }
}
