// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Native, reference-only link requests over the caller's shared executor.
//!
//! DataAgent requests affect staging only; they never select, publish or execute
//! a datasource. SemanticModel requests bind one existing connection and require
//! model ownership and permission to use that connection. Source capability,
//! identity and permissions must be resolved by the caller, not inferred here.
//! Ontology definition replacement and Power BI gateway binding are not supported.

use azure_core::http::Url;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::fmt;

/// Parse one JSON document, rejecting duplicate object members at every depth.
///
/// This parser is schema-independent and can also validate shortcut input before
/// its separate typed validation. Decoded object keys are compared exactly, so
/// escaped spellings of the same key are duplicates. JSON values and object
/// order retain `serde_json` semantics, including its default recursion limit.
/// The caller must decode bytes as UTF-8 before calling this function.
///
/// # Errors
/// Returns sanitized local `InvalidInput` for duplicate keys, invalid JSON,
/// excessive nesting or non-whitespace content after the first document.
pub fn parse_link_json(input: &str) -> Result<Value, ResourceError> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let value = StrictValue::deserialize(&mut deserializer).map_err(|_| ResourceError::input())?;
    deserializer.end().map_err(|_| ResourceError::input())?;
    Ok(value.0)
}

/// A finite native relationship, distinct from shortcuts or definition updates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeLinkKind {
    /// Create a ById datasource in an existing DataAgent's staging environment.
    DataAgentDatasource,
    /// Bind one semantic-model datasource reference to an existing connection.
    SemanticModelConnection,
}

/// An immutable, validated native link request with no transport or credentials.
#[derive(Clone)]
pub struct NativeLinkRequest {
    kind: NativeLinkKind,
    workspace: String,
    item: String,
    body: Value,
}

impl NativeLinkRequest {
    /// Validate the exact native POST schema and destination UUIDs without I/O.
    ///
    /// The caller must independently resolve a supported DataAgent source item.
    /// Connection metadata must explicitly supply its connectivity type; no
    /// default, gateway fallback, SSO inference or unbind is performed.
    /// UUID spelling and connection-detail strings are retained unchanged.
    ///
    /// This accepts an already parsed value. Use [`parse_link_json`] or another
    /// duplicate-rejecting parser before converting raw input into a `Value`.
    ///
    /// # Errors
    /// Returns sanitized `InvalidInput` for malformed or unsafe schemas and
    /// `UnsupportedCapability` for unsupported connectivity modes. All such
    /// errors are local and indicate that no mutation may have occurred.
    pub fn from_value(
        kind: NativeLinkKind,
        workspace: &str,
        item: &str,
        input: &Value,
    ) -> Result<Self, ResourceError> {
        validate_link_id(workspace)?;
        validate_link_id(item)?;
        match kind {
            NativeLinkKind::DataAgentDatasource => {
                let request: DatasourceRequest =
                    serde_json::from_value(input.clone()).map_err(|_| ResourceError::input())?;
                let reference = match request {
                    DatasourceRequest::LakehouseTables {
                        lakehouse_reference,
                    } => lakehouse_reference,
                    DatasourceRequest::FabricItem { item_reference } => item_reference,
                };
                if reference.reference_type != "ById" {
                    return Err(ResourceError::input());
                }
                validate_link_id(&reference.workspace_id)?;
                validate_link_id(&reference.item_id)?;
            }
            NativeLinkKind::SemanticModelConnection => {
                let request: BindRequest =
                    serde_json::from_value(input.clone()).map_err(|_| ResourceError::input())?;
                let binding = request.connection_binding;
                validate_link_id(&binding.id)?;
                validate_reference_text(&binding.connection_details.kind)?;
                validate_reference_text(&binding.connection_details.path)?;
                if !matches!(
                    binding.connectivity_type.as_str(),
                    "ShareableCloud"
                        | "PersonalCloud"
                        | "OnPremisesGateway"
                        | "OnPremisesGatewayPersonal"
                        | "VirtualNetworkGateway"
                        | "StreamingVirtualNetworkGateway"
                ) {
                    return Err(unsupported_connectivity());
                }
            }
        }
        Ok(Self {
            kind,
            workspace: workspace.into(),
            item: item.into(),
            body: input.clone(),
        })
    }

    /// Describe exactly one native POST using the caller's completion policy.
    ///
    /// DataAgent creation documents HTTP 201/202. SemanticModel binding documents
    /// an empty HTTP 200 response, not an operation-specific LRO. The shared
    /// executor alone interprets actual service acceptance and completion.
    ///
    /// # Errors
    /// Preserves a shared relative-operation validation failure before dispatch.
    pub fn operation(
        &self,
        completion: CompletionPolicy,
    ) -> Result<ResourceOperation, ResourceError> {
        let segments = match self.kind {
            NativeLinkKind::DataAgentDatasource => vec![
                "workspaces",
                self.workspace.as_str(),
                "dataAgents",
                self.item.as_str(),
                "staging",
                "datasources",
            ],
            NativeLinkKind::SemanticModelConnection => vec![
                "workspaces",
                self.workspace.as_str(),
                "semanticModels",
                self.item.as_str(),
                "bindConnection",
            ],
        };
        ResourceOperation::new(
            ResourceMethod::Post,
            &segments,
            Some(self.body.clone()),
            completion,
        )
    }

    /// Delegate exactly once, preserving the original outcome or error.
    ///
    /// No response normalization, retry, polling, result synthesis or fallback
    /// occurs here. Empty completion, acceptance and mutation uncertainty retain
    /// the shared executor's exact meaning.
    ///
    /// # Errors
    /// Preserves local operation-validation errors and unchanged executor errors.
    pub async fn execute(
        &self,
        executor: &mut dyn ResourceOperations,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor.execute(self.operation(completion)?).await
    }
}

impl fmt::Debug for NativeLinkRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeLinkRequest")
            .field("kind", &self.kind)
            .field("destination", &"<resource>")
            .field("body", &"<reference metadata>")
            .finish()
    }
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictValueVisitor).map(Self)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("Non-finite JSON number"))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom("Duplicate JSON object member"));
            }
            let value = object.next_value::<StrictValue>()?;
            values.insert(key, value.0);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum DatasourceRequest {
    LakehouseTables {
        #[serde(rename = "lakehouseReference")]
        lakehouse_reference: ByIdReference,
    },
    FabricItem {
        #[serde(rename = "itemReference")]
        item_reference: ByIdReference,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ByIdReference {
    reference_type: String,
    workspace_id: String,
    item_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindRequest {
    connection_binding: ConnectionBinding,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectionBinding {
    id: String,
    connectivity_type: String,
    connection_details: ConnectionDetails,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionDetails {
    #[serde(rename = "type")]
    kind: String,
    path: String,
}

/// Validate hyphenated UUID syntax for a native link request or response ID.
///
/// Accepts exactly 32 ASCII hexadecimal digits and four canonical hyphens.
/// Hexadecimal letters may use either case; no normalization, service lookup,
/// UUID version restriction or proof of resource existence is performed.
///
/// # Errors
/// Returns sanitized local `InvalidInput` for any other spelling, including
/// nonempty names, surrounding whitespace and URL or encoded selectors.
pub fn validate_link_id(value: &str) -> Result<(), ResourceError> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(ResourceError::input());
    }
    Ok(())
}

fn validate_reference_text(value: &str) -> Result<(), ResourceError> {
    let lower = value.to_ascii_lowercase();
    if value.trim().is_empty()
        || value.chars().any(char::is_control)
        || lower.trim_start().starts_with("bearer ")
        || lower.contains("-----begin private key-----")
        || lower.contains("-----begin rsa private key-----")
        || lower.contains("-----begin ec private key-----")
        || value.split([';', '?', '&', '#']).any(|part| {
            part.split_once('=')
                .is_some_and(|(key, _)| credential_key(key))
        })
    {
        return Err(ResourceError::input());
    }
    // SQL and other native paths are not URLs. Only inspect URLs when parsable;
    // do not normalize or impose a URI grammar on arbitrary datasource selectors.
    if let Ok(url) = Url::parse(value) {
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query_pairs().any(|(key, _)| credential_key(&key))
        {
            return Err(ResourceError::input());
        }
    }
    Ok(())
}

fn credential_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|&character| !matches!(character, '_' | '-' | ' '))
        .collect();
    matches!(
        normalized.trim().to_ascii_lowercase().as_str(),
        "password"
            | "pwd"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "clientsecret"
            | "accountkey"
            | "sharedaccesskey"
            | "sharedaccesssignature"
            | "sig"
            | "apikey"
            | "authorization"
            | "credential"
            | "credentials"
    )
}

fn unsupported_connectivity() -> ResourceError {
    ResourceError {
        code: "UnsupportedCapability".into(),
        message: "Native link requires an explicitly supported connection connectivity type".into(),
        ..ResourceError::input()
    }
}
