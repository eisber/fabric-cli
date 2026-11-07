// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bounded native shortcut creation, separate from the released v1 handlers.
//!
//! The contracts are the Fabric API v1 Core OneLake Shortcuts / Create Shortcut
//! and KQLDatabase Table Shortcuts / Create Shortcut definitions, documentation
//! commit `b4d803a1e7f495553fcb7ef509391016e676d91d`.
//! Only existing item/connection references are accepted. This module does not
//! acquire credentials, probe storage URLs, grant access, or change delegation.
//! The shared caller owns transport, request/body limits and LRO reconciliation.
//!
//! Lakehouse `Files` supports any safe folder depth. The only supported `Tables`
//! destination is the root `Tables`, with a OneLake source `Tables/<table>` and
//! whitespace-free table names. Schema destinations, schema shortcuts, external
//! table discovery and verification of Delta contents are not implemented.
//! This conservative placement rule is not a claim of schema-enabled support.
//!
//! HTTPS endpoint property definitions are used even where KQL examples show
//! `s3://` or `gs://`. OneLake source paths must be relative even where a KQL
//! example shows a leading slash. Values are validated, never normalized.
//! External endpoint locations have no path beyond an optional root slash and
//! use documented public-cloud host shapes; S3-compatible hosts are caller
//! supplied. External subpaths may retain one leading slash; only bucket-based
//! targets accept `/` as an explicit root. These are storage metadata, never
//! origins to which this SDK sends a credential or a permission probe.

use azure_core::http::Url;
use fabric_sdk_workspaces::mutations::{
    CompletionPolicy, ResourceError, ResourceMethod, ResourceOperation, ResourceOperations,
    ResourceOutcome,
};
use serde_json::{Map, Value};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShortcutOwner {
    Lakehouse,
    KqlDatabase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetKind {
    OneLake,
    AdlsGen2,
    AmazonS3,
    GoogleCloudStorage,
    S3Compatible,
    AzureBlobStorage,
    Dataverse,
    OneDriveSharePoint,
}

/// Validated, single native shortcut creation with an immutable request body.
///
/// The request never creates a connection or selects an identity. Core creation
/// always uses `Abort`; KQL creation has no conflict-policy or destination-path
/// field. Debug output excludes identifiers, names, locations and request data.
#[derive(Clone)]
pub struct ShortcutLinkRequest {
    owner: ShortcutOwner,
    workspace: String,
    item: String,
    body: Value,
    update_sensitivity: bool,
}

impl ShortcutLinkRequest {
    /// Validate an exact native Core or KQL shortcut request without doing I/O.
    ///
    /// `owner_type` must be exactly `Lakehouse` or `KQLDatabase`; workspace,
    /// item and connection references must have UUID shape. Lakehouse accepts
    /// exactly `{path,name,target}`; KQL accepts exactly
    /// `{name,enableQueryAcceleration,target}`, with a required boolean.
    /// Targets contain exactly one creatable native discriminator and only its
    /// typed, allowlisted fields. Response-only `target.type` is invalid.
    ///
    /// An explicit SharePoint `updateFabricItemSensitivity: true` requests an
    /// additional sensitivity-label effect. A caller must separately authorize
    /// that effect before execution; inspect
    /// [`Self::updates_fabric_item_sensitivity`]. No default is inserted.
    ///
    /// # Errors
    /// Returns sanitized `InvalidInput` for malformed data and
    /// `UnsupportedCapability` for unsupported owners, targets or placement.
    /// Local failures always have `mutation_may_have_occurred == false`.
    pub fn from_value(
        owner_type: &str,
        workspace: &str,
        item: &str,
        input: &Value,
    ) -> Result<Self, ResourceError> {
        let owner = match owner_type {
            "Lakehouse" => ShortcutOwner::Lakehouse,
            "KQLDatabase" => ShortcutOwner::KqlDatabase,
            _ => return Err(unsupported()),
        };
        uuid(workspace)?;
        uuid(item)?;
        let fields = match owner {
            ShortcutOwner::Lakehouse => &["path", "name", "target"][..],
            ShortcutOwner::KqlDatabase => &["name", "enableQueryAcceleration", "target"][..],
        };
        let body = object(input, fields)?;
        let name = string(body, "name")?;
        segment(name)?;
        if owner == ShortcutOwner::KqlDatabase {
            if name.contains(['[', ']']) {
                return Err(ResourceError::input());
            }
            boolean(body, "enableQueryAcceleration")?;
        }
        let target = body.get("target").ok_or_else(ResourceError::input)?;
        let (kind, details) = validate_target(owner, target)?;
        if owner == ShortcutOwner::Lakehouse {
            validate_placement(string(body, "path")?, name, kind, details)?;
        }
        let update_sensitivity = kind == TargetKind::OneDriveSharePoint
            && details
                .get("updateFabricItemSensitivity")
                .and_then(Value::as_bool)
                == Some(true);
        Ok(Self {
            owner,
            workspace: workspace.into(),
            item: item.into(),
            body: input.clone(),
            update_sensitivity,
        })
    }

    /// Whether this request explicitly asks to update the item's sensitivity.
    ///
    /// This is an additional SharePoint effect, not implied by shortcut creation.
    /// Caller UI/policy must require explicit authorization before dispatch.
    pub const fn updates_fabric_item_sensitivity(&self) -> bool {
        self.update_sensitivity
    }

    /// Whether KQL query acceleration was explicitly enabled.
    ///
    /// Enabling acceleration can incur cache, indexing and compute costs. A
    /// Lakehouse request always returns false; no KQL default is synthesized.
    pub fn enables_query_acceleration(&self) -> bool {
        self.body
            .get("enableQueryAcceleration")
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// Describe one fixed-origin native POST with caller-selected completion.
    ///
    /// Core uses `/v1/workspaces/{ws}/items/{item}/shortcuts` and explicit
    /// `shortcutConflictPolicy=Abort` (documented responses: 200/201).
    /// KQL uses `/v1/workspaces/{ws}/kqlDatabases/{item}/shortcuts`, without a
    /// conflict query (documented responses: 201/202). Acceptance is not
    /// completion; the shared executor owns all LRO/header handling.
    ///
    /// # Errors
    /// Propagates shared operation-construction validation without dispatch.
    pub fn operation(
        &self,
        completion: CompletionPolicy,
    ) -> Result<ResourceOperation, ResourceError> {
        let family = match self.owner {
            ShortcutOwner::Lakehouse => "items",
            ShortcutOwner::KqlDatabase => "kqlDatabases",
        };
        let operation = ResourceOperation::new(
            ResourceMethod::Post,
            &[
                "workspaces",
                &self.workspace,
                family,
                &self.item,
                "shortcuts",
            ],
            Some(self.body.clone()),
            completion,
        )?;
        match self.owner {
            ShortcutOwner::Lakehouse => {
                operation.with_query(&[("shortcutConflictPolicy", "Abort")])
            }
            ShortcutOwner::KqlDatabase => Ok(operation),
        }
    }

    /// Execute once through the caller's existing shared operation policy.
    ///
    /// No retry, storage probe, polling or cancellation mechanism is installed.
    /// The caller must authorize any declared sensitivity-label effect first.
    ///
    /// # Errors
    /// Preserves the executor's exact service and mutation-uncertainty evidence.
    pub async fn execute(
        &self,
        executor: &mut dyn ResourceOperations,
        completion: CompletionPolicy,
    ) -> Result<ResourceOutcome, ResourceError> {
        executor.execute(self.operation(completion)?).await
    }
}

impl fmt::Debug for ShortcutLinkRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShortcutLinkRequest")
            .field("owner", &self.owner)
            .field("body", &"<shortcut metadata>")
            .field("update_sensitivity", &self.update_sensitivity)
            .finish()
    }
}

fn validate_target(
    owner: ShortcutOwner,
    value: &Value,
) -> Result<(TargetKind, &Map<String, Value>), ResourceError> {
    let target = value.as_object().ok_or_else(ResourceError::input)?;
    if target.len() != 1 || target.contains_key("type") {
        return Err(ResourceError::input());
    }
    let (key, value) = target.iter().next().ok_or_else(ResourceError::input)?;
    let kind = match key.as_str() {
        "oneLake" => TargetKind::OneLake,
        "adlsGen2" => TargetKind::AdlsGen2,
        "amazonS3" => TargetKind::AmazonS3,
        "googleCloudStorage" => TargetKind::GoogleCloudStorage,
        "s3Compatible" => TargetKind::S3Compatible,
        "azureBlobStorage" => TargetKind::AzureBlobStorage,
        "dataverse" => TargetKind::Dataverse,
        "oneDriveSharePoint" => TargetKind::OneDriveSharePoint,
        _ => return Err(unsupported()),
    };
    if owner == ShortcutOwner::KqlDatabase
        && matches!(kind, TargetKind::Dataverse | TargetKind::OneDriveSharePoint)
    {
        return Err(unsupported());
    }
    let allowed = match kind {
        TargetKind::OneLake => &["workspaceId", "itemId", "path", "connectionId"][..],
        TargetKind::Dataverse => &[
            "connectionId",
            "environmentDomain",
            "tableName",
            "deltaLakeFolder",
        ][..],
        TargetKind::S3Compatible => &["connectionId", "location", "subpath", "bucket"][..],
        TargetKind::OneDriveSharePoint => &[
            "connectionId",
            "location",
            "subpath",
            "updateFabricItemSensitivity",
        ][..],
        _ => &["connectionId", "location", "subpath"][..],
    };
    let details = object(value, allowed)?;
    if kind == TargetKind::OneLake {
        uuid(string(details, "workspaceId")?)?;
        uuid(string(details, "itemId")?)?;
        relative(string(details, "path")?)?;
        if details.contains_key("connectionId") {
            uuid(string(details, "connectionId")?)?;
        }
    } else {
        uuid(string(details, "connectionId")?)?;
        if kind == TargetKind::Dataverse {
            endpoint(string(details, "environmentDomain")?, kind)?;
            segment(string(details, "tableName")?)?;
            subpath(string(details, "deltaLakeFolder")?, false)?;
        } else {
            endpoint(string(details, "location")?, kind)?;
            subpath(
                string(details, "subpath")?,
                matches!(
                    kind,
                    TargetKind::AmazonS3
                        | TargetKind::GoogleCloudStorage
                        | TargetKind::S3Compatible
                ),
            )?;
            if kind == TargetKind::S3Compatible {
                segment(string(details, "bucket")?)?;
            }
            if details.contains_key("updateFabricItemSensitivity") {
                boolean(details, "updateFabricItemSensitivity")?;
            }
        }
    }
    Ok((kind, details))
}

fn validate_placement(
    path: &str,
    name: &str,
    kind: TargetKind,
    details: &Map<String, Value>,
) -> Result<(), ResourceError> {
    relative(path)?;
    if path == "Files" || path.starts_with("Files/") {
        return Ok(());
    }
    if path == "Tables" && kind == TargetKind::OneLake {
        let source = string(details, "path")?;
        if let Some(table) = source.strip_prefix("Tables/")
            && !table.contains('/')
            && !table.chars().any(char::is_whitespace)
            && !name.chars().any(char::is_whitespace)
        {
            return Ok(());
        }
    }
    Err(unsupported())
}

fn object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>, ResourceError> {
    let object = value.as_object().ok_or_else(ResourceError::input)?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ResourceError::input());
    }
    Ok(object)
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, ResourceError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(ResourceError::input)
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, ResourceError> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(ResourceError::input)
}

fn uuid(value: &str) -> Result<(), ResourceError> {
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

fn segment(value: &str) -> Result<(), ResourceError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.trim() != value
        || !value.is_ascii()
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || matches!(
                    byte,
                    b'/' | b'\\'
                        | b'?'
                        | b'#'
                        | b'%'
                        | b'+'
                        | b':'
                        | b'"'
                        | b'<'
                        | b'>'
                        | b'|'
                        | b'*'
                )
        })
    {
        return Err(ResourceError::input());
    }
    Ok(())
}

fn relative(value: &str) -> Result<(), ResourceError> {
    value.split('/').try_for_each(segment)
}

fn subpath(value: &str, allow_root: bool) -> Result<(), ResourceError> {
    if value == "/" && allow_root {
        return Ok(());
    }
    relative(value.strip_prefix('/').unwrap_or(value))
}

fn endpoint(value: &str, kind: TargetKind) -> Result<(), ResourceError> {
    if !value.is_ascii()
        || !value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace()
                || byte.is_ascii_control()
                || matches!(byte, b'\\' | b'%' | b'+' | b'@')
        })
    {
        return Err(ResourceError::input());
    }
    // Reject raw path syntax before URL parsing can normalize dot segments.
    let authority = &value[8..];
    if authority
        .strip_suffix('/')
        .unwrap_or(authority)
        .contains('/')
    {
        return Err(ResourceError::input());
    }
    let url = Url::parse(value).map_err(|_| ResourceError::input())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(ResourceError::input());
    }
    let host = url.host_str().ok_or_else(ResourceError::input)?;
    let suffix = |suffix: &str| {
        host.strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.ends_with('.'))
    };
    let valid = match kind {
        TargetKind::AdlsGen2 => suffix(".dfs.core.windows.net"),
        TargetKind::AzureBlobStorage => suffix(".blob.core.windows.net"),
        TargetKind::AmazonS3 => host
            .strip_suffix(".amazonaws.com")
            .and_then(|host| host.rsplit_once(".s3."))
            .is_some_and(|(bucket, region)| {
                !bucket.is_empty() && !region.is_empty() && !region.contains('.')
            }),
        TargetKind::GoogleCloudStorage => suffix(".storage.googleapis.com"),
        TargetKind::OneDriveSharePoint => suffix(".sharepoint.com"),
        TargetKind::Dataverse => host
            .strip_suffix(".dynamics.com")
            .and_then(|host| host.rsplit_once('.'))
            .is_some_and(|(org, region)| {
                !org.is_empty()
                    && !org.contains('.')
                    && region
                        .strip_prefix("crm")
                        .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()))
            }),
        TargetKind::S3Compatible => !host.is_empty(),
        TargetKind::OneLake => false,
    };
    if !valid {
        return Err(ResourceError::input());
    }
    Ok(())
}

fn unsupported() -> ResourceError {
    let mut error = ResourceError::input();
    error.code = "UnsupportedCapability".into();
    error.message = "Shortcut owner, target or placement is not supported".into();
    error
}
