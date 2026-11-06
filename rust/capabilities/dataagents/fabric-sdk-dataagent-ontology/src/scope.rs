// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit caller, agent, stage, ontology and source identities.

use crate::ValidationError;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A nonzero GUID, normalized only for GUID identity comparison.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Guid(String);

impl Guid {
    /// Accepts the hyphenated GUID form and rejects the empty GUID.
    pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.len() != 36
            || !value.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
            || value.bytes().all(|byte| byte == b'0' || byte == b'-')
        {
            return Err(ValidationError::InvalidGuid);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    /// Returns the canonical route-safe spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Guid {
    type Error = ValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Guid> for String {
    fn from(value: Guid) -> Self {
        value.0
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The immutable published snapshot or mutable staging tree; never inferred.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Stage {
    /// Private authoring tree (service Sandbox stage).
    Staging,
    /// Last published configuration (service Production stage).
    Published,
}

/// Caller-specific cache/validator partition, not proof of provider claims.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CallerScope {
    account: String,
    tenant: String,
}

impl CallerScope {
    /// Creates explicit selection metadata without discovering credentials.
    pub fn new(
        account: impl Into<String>,
        tenant: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let account = account.into();
        let tenant = tenant.into();
        if account.trim().is_empty() || tenant.trim().is_empty() {
            return Err(ValidationError::InvalidSelector);
        }
        Ok(Self { account, tenant })
    }
}

impl fmt::Debug for CallerScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CallerScope(<explicit>)")
    }
}

/// The DataAgent route owner. This is not the referenced ontology workspace.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AgentScope {
    /// Workspace containing the DataAgent.
    pub workspace_id: Guid,
    /// DataAgent item ID.
    pub agent_id: Guid,
}

/// Stage-qualified attachment identity.
///
/// Attachment add idempotency is scoped to the agent and ontology item ID, not
/// display name, referenced workspace, or the physical bound datasource.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AttachmentScope {
    /// Bound DataAgent.
    pub agent: AgentScope,
    /// Explicit configuration stage.
    pub stage: Stage,
    /// Referenced ontology item ID.
    pub ontology_id: Guid,
}

/// An ontology-owned virtual source, not a standalone physical Fabric item.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourceScope {
    /// Owning agent, ontology and configuration stage.
    pub attachment: AttachmentScope,
    /// Returned source ID; Eventhouse sources can identify a child KQL database.
    pub source_id: Guid,
}

/// Authority scope of an ETag; navigation/cache representations never authorize writes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Resource {
    /// Whole DataAgent staging tree, used only by explicit publication.
    Agent(AgentScope, Stage),
    /// Stage-qualified attachment collection.
    Attachments(AgentScope, Stage),
    /// Attachment metadata and entity selection.
    Attachment(AttachmentScope),
    /// Ontology entity navigation, whose ETag hashes a served representation.
    OntologyElements(AttachmentScope),
    /// Generated context projection, never a validator for authored configuration.
    ContextMarkdown(AttachmentScope),
    /// Read-only derived source collection.
    Sources(AttachmentScope),
    /// Read-only cached source schema.
    SourceSchema(SourceScope),
    /// Read-only source element navigation.
    SourceElements(SourceScope),
    /// Authored source instructions/description.
    Instructions(SourceScope),
    /// Authored source examples collection.
    FewShots(SourceScope),
    /// A single authored example.
    FewShot(SourceScope, Guid),
    /// All retained custom setup on an unavailable source.
    CustomSetup(SourceScope),
}

impl Resource {
    /// The agent and explicit stage of this resource.
    pub fn agent_stage(&self) -> (&AgentScope, Stage) {
        match self {
            Self::Agent(agent, stage) | Self::Attachments(agent, stage) => (agent, *stage),
            Self::Attachment(scope)
            | Self::OntologyElements(scope)
            | Self::ContextMarkdown(scope)
            | Self::Sources(scope) => (&scope.agent, scope.stage),
            Self::SourceSchema(scope)
            | Self::SourceElements(scope)
            | Self::Instructions(scope)
            | Self::FewShots(scope)
            | Self::FewShot(scope, _)
            | Self::CustomSetup(scope) => (&scope.attachment.agent, scope.attachment.stage),
        }
    }
}

/// Exact caller/resource-scoped conditional validator.
#[derive(Clone, Eq, PartialEq)]
pub struct Validator {
    caller: CallerScope,
    resource: Resource,
    value: String,
}

impl Validator {
    /// Preserves the exact source ETag without normalization or wildcard semantics.
    pub fn new(
        caller: CallerScope,
        resource: Resource,
        value: impl Into<String>,
    ) -> Result<Self, ValidationError> {
        let value = value.into();
        if value.trim().is_empty()
            || value == "*"
            || value.len() > 8192
            || !value.is_ascii()
            || value.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidETag);
        }
        Ok(Self {
            caller,
            resource,
            value,
        })
    }

    /// Checks caller and full resource identity before sending an optional condition.
    pub fn for_resource(
        &self,
        caller: &CallerScope,
        resource: &Resource,
    ) -> Result<&str, ValidationError> {
        if &self.caller != caller || &self.resource != resource {
            Err(ValidationError::ValidatorScope)
        } else {
            Ok(&self.value)
        }
    }
}

impl fmt::Debug for Validator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Validator")
            .field("resource", &self.resource)
            .field("value", &"<opaque>")
            .finish()
    }
}
