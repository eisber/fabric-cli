// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::constraints::{Field, InputConstraint, ObjectConstraint, StringConstraint};
use crate::{DiscoveryError, Operation, TargetKind, TargetSelector};
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

/// Version of the local descriptor envelope, not a Fabric API or LRO version.
pub const CONTRACT_VERSION: &str = "1.0";

/// Dialect used by the local structural constraint projection.
pub const INPUT_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

const DISPLAY_NAME: Field = Field {
    name: "displayName",
    description: "Workspace display name. Tenant uniqueness and reserved names are not locally validated.",
    required: true,
    nullable: false,
    default: None,
    constraint: StringConstraint {
        max_length: Some(256),
        uuid: false,
        choices: &[],
    },
};
const DESCRIPTION: Field = Field {
    name: "description",
    description: "Workspace description; empty and absent are preserved distinctly.",
    required: false,
    nullable: false,
    default: None,
    constraint: StringConstraint {
        max_length: Some(4000),
        uuid: false,
        choices: &[],
    },
};
const CAPACITY_ID: Field = Field {
    name: "capacityId",
    description: "Capacity UUID. Capacity existence, permissions and assignment policy are not validated.",
    required: false,
    nullable: false,
    default: None,
    constraint: StringConstraint {
        max_length: None,
        uuid: true,
        choices: &[],
    },
};
const DOMAIN_ID: Field = Field {
    name: "domainId",
    description: "Domain UUID. Domain existence, permissions and assignment policy are not validated.",
    ..CAPACITY_ID
};
static WORKSPACE_CREATE: ObjectConstraint = ObjectConstraint {
    fields: &[DISPLAY_NAME, DESCRIPTION, CAPACITY_ID, DOMAIN_ID],
    exclusions: &[],
};
static WORKSPACE_UPDATE: ObjectConstraint = ObjectConstraint {
    fields: &[
        Field {
            required: false,
            ..DISPLAY_NAME
        },
        DESCRIPTION,
    ],
    exclusions: &[],
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    url: &'static str,
    revision: &'static str,
    scope: &'static str,
}

const WORKSPACE_CREATE_DOC: Source = Source {
    url: "https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/create-workspace",
    revision: "9feaea0cb4ae5c7942ca78399640061545c3df95",
    scope: "Fabric Core v1 create fields, requiredness and documented string maxima; not complete service validation",
};
const WORKSPACE_UPDATE_DOC: Source = Source {
    url: "https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/update-workspace",
    revision: "9feaea0cb4ae5c7942ca78399640061545c3df95",
    scope: "Fabric Core v1 optional displayName/description metadata PATCH; not a generic v2 update mode",
};
const WORKSPACE_DELETE_DOC: Source = Source {
    url: "https://learn.microsoft.com/en-us/rest/api/fabric/core/workspaces/delete-workspace",
    revision: "9feaea0cb4ae5c7942ca78399640061545c3df95",
    scope: "Fabric Core v1 DELETE with no body; deletes workspace and contained items; no recovery guarantee established",
};
const WORKSPACE_SDK: Source = Source {
    url: "https://github.com/eisber/fabric-cli/blob/5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b/rust/sdk/workspaces/src/mutations.rs",
    revision: "5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b",
    scope: "WorkspaceMutations create/update/delete routes only; accepting Value is not input-schema evidence",
};
const NOTEBOOK_SDK: Source = Source {
    url: "https://github.com/eisber/fabric-cli/blob/5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b/rust/sdk/jobs/src/lib.rs",
    revision: "5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b",
    scope: "JobType::Notebook is RunNotebook; submit(None) omits body; arbitrary Value is not an exact executionData signature",
};
const JOB_DOC: Source = Source {
    url: "https://learn.microsoft.com/en-us/rest/api/fabric/core/job-scheduler/run-on-demand-item-job",
    revision: "b4d803a1e7f495553fcb7ef509391016e676d91d",
    scope: "No-body job submission; executionData is specific to jobType and per-run parameters are not broadly supported",
};
const NOTEBOOK_DOC: Source = Source {
    url: "https://learn.microsoft.com/en-us/fabric/data-engineering/notebook-public-api",
    revision: "932298cc5ea37344bf2d10b403ac7a56ac073b8a",
    scope: "Notebook execution capability, not a complete per-notebook executionData or dynamic function signature",
};

/// An immutable operation registration shared by all front-end projections.
///
/// Entries are obtained from [`registrations`], not supplied by an external
/// plugin or dynamically inferred from a JSON body. Private typed constraints
/// generate both validation and every schema/help projection.
#[derive(Debug)]
pub struct OperationRegistration {
    operation: Operation,
    kind: TargetKind,
    id: &'static str,
    summary: &'static str,
    input: InputConstraint,
    method: &'static str,
    representation: &'static str,
    native_semantics: &'static str,
    effects: &'static [&'static str],
    prerequisites: &'static [&'static str],
    execution_result: &'static str,
    unsupported: &'static [&'static str],
    example: &'static str,
    sources: &'static [Source],
}

static REGISTRATIONS: &[OperationRegistration] = &[
    OperationRegistration {
        operation: Operation::Create,
        kind: TargetKind::Workspace,
        id: "fabric.core.workspace.create.v1",
        summary: "Describe creation of a prospective Workspace using the documented JSON subset.",
        input: InputConstraint::Required(&WORKSPACE_CREATE),
        method: "POST",
        representation: "metadata",
        native_semantics: "create_workspace",
        effects: &[
            "would_create_workspace",
            "may_assign_capacity_or_domain_when_requested",
        ],
        prerequisites: &[
            "Execution needs an independently resolved destination context and caller authorization.",
            "Capacity/domain assignment needs independently verified service prerequisites.",
        ],
        execution_result: "Service-documented Workspace response; discovery returns only this descriptor, not a Workspace or acceptance handle.",
        unsupported: &[
            "native_definition",
            "revision_selection",
            "complete_service_policy_validation",
        ],
        example: "create /New.Workspace --schema",
        sources: &[WORKSPACE_CREATE_DOC, WORKSPACE_SDK],
    },
    OperationRegistration {
        operation: Operation::Update,
        kind: TargetKind::Workspace,
        id: "fabric.core.workspace.update-metadata.v1",
        summary: "Describe the native Workspace metadata PATCH subset, not a selected v2 update mode.",
        input: InputConstraint::Required(&WORKSPACE_UPDATE),
        method: "PATCH",
        representation: "metadata",
        native_semantics: "metadata_patch_only",
        effects: &["would_modify_workspace_metadata"],
        prerequisites: &[
            "Execution needs resolved Workspace identity, admin workspace role and applicable authorization.",
        ],
        execution_result: "Service-documented Workspace response; absent metadata properties remain unspecified by this local validator.",
        unsupported: &[
            "v2_update_mode_selection",
            "generic_replace",
            "native_definition",
            "spark_settings",
            "capacity_or_domain_reassignment",
            "revision_selection",
            "complete_service_policy_validation",
        ],
        example: "update /Sales.Workspace --schema",
        sources: &[WORKSPACE_UPDATE_DOC, WORKSPACE_SDK],
    },
    OperationRegistration {
        operation: Operation::Delete,
        kind: TargetKind::Workspace,
        id: "fabric.core.workspace.delete.v1",
        summary: "Describe native Workspace deletion without a request body or recovery guarantee.",
        input: InputConstraint::Absent {
            parameter_gap: false,
        },
        method: "DELETE",
        representation: "metadata",
        native_semantics: "delete_workspace_and_contained_items",
        effects: &["would_delete_workspace_and_contained_items", "destructive"],
        prerequisites: &[
            "Execution needs resolved Workspace identity, admin workspace role and applicable authorization.",
            "Any future host must apply its own explicit destructive-operation policy.",
        ],
        execution_result: "Service documents HTTP 200 without a response body; discovery neither deletes nor guarantees recoverability.",
        unsupported: &[
            "recursive_option",
            "permanent_option",
            "recoverable_delete_guarantee",
            "revision_selection",
        ],
        example: "delete /Sales.Workspace --schema",
        sources: &[WORKSPACE_DELETE_DOC, WORKSPACE_SDK],
    },
    OperationRegistration {
        operation: Operation::Run,
        kind: TargetKind::Notebook,
        id: "fabric.core.notebook.run-default.v1",
        summary: "Describe Notebook RunNotebook submission with absent executionData and parameters only.",
        input: InputConstraint::Absent {
            parameter_gap: true,
        },
        method: "POST",
        representation: "job_execution",
        native_semantics: "RunNotebook_no_body_default_execution",
        effects: &[
            "would_submit_notebook_job",
            "notebook_code_may_mutate_data_or_external_systems",
        ],
        prerequisites: &[
            "Execution needs resolved Workspace/Notebook IDs and independently verified execute authorization.",
            "Runtime, capacity and notebook-specific service prerequisites are not evaluated.",
        ],
        execution_result: "HTTP 202 is job acceptance, not completion; any job instance ID is distinct from this canonical contract ID.",
        unsupported: &[
            "executionData",
            "parameters",
            "dynamic_function_signature",
            "native_definition",
            "revision_selection",
        ],
        example: "run /Sales.Workspace/Analysis.Notebook --schema",
        sources: &[JOB_DOC, NOTEBOOK_SDK, NOTEBOOK_DOC],
    },
];

/// Iterate the finite, deterministic catalogue for command and help generation.
///
/// Presence means a local contract is known, never that a v2 execution host is
/// configured. There is one entry per exact operation/kind pair.
pub fn registrations() -> &'static [OperationRegistration] {
    REGISTRATIONS
}

impl OperationRegistration {
    /// Common operation used by root commands and type-narrowed facades.
    pub const fn operation(&self) -> Operation {
        self.operation
    }

    /// Exact supported resource kind; no parent-type fallback is permitted.
    pub const fn target_kind(&self) -> TargetKind {
        self.kind
    }

    /// Stable registration identity, never an LRO/job/service instance ID.
    pub const fn canonical_id(&self) -> &'static str {
        self.id
    }

    /// Source-backed description of the registered subset.
    pub const fn summary(&self) -> &'static str {
        self.summary
    }

    /// JSON Schema for a present body, or `false` when every body is unsupported.
    ///
    /// Body presence is separate from JSON nullability. Use the `input.required`
    /// and `input.presence` descriptor fields, or [`OperationContract::validate_input`],
    /// to model absence. No defaults are applied.
    pub fn input_schema(&self) -> Value {
        self.input.schema()
    }

    /// Generate human help from these same constraints and capability metadata.
    pub fn help(&self) -> String {
        format!(
            "{} <target> --schema\n{}\nCanonical ID: {}\nTarget: {} resource only; resolution unverified; authorization not evaluated.\n{}\nEffects of the described operation: {}.\nNative semantics: {}. V2 mutation execution unconfigured.\nPrerequisites: {}\nUnsupported: {}.\nValidation is a local structural subset, not complete service or native semantic validation.\nExample: {}",
            self.operation.as_str(),
            self.summary,
            self.id,
            self.kind.as_str(),
            self.input.help(),
            self.effects.join(", "),
            self.native_semantics,
            self.prerequisites.join(" "),
            self.unsupported.join(", "),
            self.example,
        )
    }

    /// Neutral CLI command-generation metadata; this contains no CLI types.
    ///
    /// Heads must not infer executable support from registration. The `input`
    /// object is generated from the same AST as SDK and MCP constraints.
    pub fn cli_descriptor(&self) -> Value {
        json!({
            "command": self.operation,
            "targetKind": self.kind,
            "operationId": self.id,
            "about": self.summary,
            "help": self.help(),
            "discoveryOnly": true,
            "input": self.input_descriptor()
        })
    }

    fn input_descriptor(&self) -> Value {
        json!({
            "required": self.input.required(),
            "presence": if self.input.required() { "required" } else { "absent_only" },
            "mediaType": if self.input.required() { Some("application/json") } else { None },
            "nativeFormat": null,
            "nativeVersion": null,
            "schemaDialect": INPUT_SCHEMA_DIALECT,
            "schema": self.input_schema(),
            "defaultsApplied": false,
            "validation": "registered_local_structural_subset_only"
        })
    }

    fn mcp_tool(&self) -> Value {
        json!({
            "name": self.id,
            "description": format!("Projection only, not an installed or executable MCP tool.\n{}", self.help()),
            "inputSchema": {
                "$schema": INPUT_SCHEMA_DIALECT,
                "type": "object",
                "properties": {"input": self.input_schema()},
                "required": if self.input.required() { vec!["input"] } else { vec![] },
                "additionalProperties": false
            },
            "_meta": {
                "fabricDiscovery": {
                    "contractVersion": CONTRACT_VERSION,
                    "operationId": self.id,
                    "projectionOnly": true,
                    "execution": "unconfigured",
                    "bodyMapping": "missing input property means no body; present input is the exact body"
                }
            }
        })
    }
}

/// A known contract bound only to an exact lexical target.
///
/// Serializing this type generates the descriptor and schema from its immutable
/// registration. Discovery never requires an operation's execution payload.
#[derive(Clone, Debug)]
pub struct OperationContract {
    registration: &'static OperationRegistration,
    target: TargetSelector,
}

impl OperationContract {
    /// Canonical operation identity, distinct from any future execution handle.
    pub const fn canonical_id(&self) -> &'static str {
        self.registration.canonical_id()
    }

    /// Exact operation selected by discovery.
    pub const fn operation(&self) -> Operation {
        self.registration.operation
    }

    /// Unresolved lexical target. Existence and permissions were not checked.
    pub fn target(&self) -> &TargetSelector {
        &self.target
    }

    /// The one registration also consumed by CLI command generation.
    pub const fn registration(&self) -> &'static OperationRegistration {
        self.registration
    }

    /// Generate the registered body schema without reading a body or source URL.
    pub fn input_schema(&self) -> Value {
        self.registration.input_schema()
    }

    /// Validate only the source-backed local subset; never execute or normalize.
    ///
    /// `None`, `Some(null)` and `Some({})` are intentionally different. Required
    /// bodies cannot be absent or null. No-body registrations accept only
    /// `None`; Notebook parameter bodies yield an explicit capability gap.
    ///
    /// # Errors
    /// Returns [`DiscoveryError::InvalidInput`] for unsupported body structure
    /// or [`DiscoveryError::UnsupportedCapability`] for an unestablished signature.
    pub fn validate_input(&self, input: Option<&Value>) -> Result<(), DiscoveryError> {
        self.registration.input.validate(input)
    }

    /// Project an MCP-compatible tool description, not a server or executable.
    ///
    /// `inputSchema` is an object envelope: an omitted `input` property maps to
    /// `None`; a present property maps to `Some(value)` without coercion.
    /// `inputSchema.properties.input` equals [`Self::input_schema`] exactly.
    /// This preserves absence versus null even for no-body operations, whose
    /// nested body schema is `false`. The target is fixed in `_meta`, not resolved.
    pub fn mcp_tool(&self) -> Value {
        let mut projection = self.registration.mcp_tool();
        projection["_meta"]["fabricDiscovery"]["target"] = self.target_descriptor();
        projection
    }

    /// Human help from the same registration as validation and machine schemas.
    pub fn help(&self) -> String {
        self.registration.help()
    }

    /// Versioned machine descriptor with explicit source and availability limits.
    pub fn descriptor(&self) -> Value {
        let entry = self.registration;
        json!({
            "contractVersion": CONTRACT_VERSION,
            "schemaVersion": "1.0",
            "schemaKind": "operation_input",
            "operationId": entry.id,
            "operationVersion": "1",
            "operation": entry.operation,
            "description": entry.summary,
            "target": self.target_descriptor(),
            "backend": {
                "id": "fabric.core",
                "apiVersion": "v1",
                "binding": "unverified",
                "nativeMethod": entry.method
            },
            "input": entry.input_descriptor(),
            "effects": {
                "discovery": "none",
                "describedOperation": entry.effects
            },
            "availability": {
                "contract": "known_local_subset",
                "targetResolution": "unverified",
                "authorization": "not_evaluated",
                "v2MutationExecution": "unconfigured"
            },
            "prerequisites": entry.prerequisites,
            "result": {
                "discovery": "this_descriptor_only",
                "execution": entry.execution_result,
                "operationIdIsExecutionHandle": false,
                "acceptedIsCompleted": false
            },
            "errors": {
                "discovery": ["invalid_operation", "invalid_target", "unsupported_capability", "invalid_input"],
                "execution": "not_attempted; service, authorization, policy and runtime errors are outside local validation"
            },
            "coverage": {
                "complete": false,
                "nativeSemantics": entry.native_semantics,
                "supportedV2UpdateModes": [],
                "v2UpdateModeSelection": "unconfigured",
                "updateModeBoundary": "Native metadata PATCH is not a selected v2 update mode; no generic replacement or mode parameter is registered.",
                "nativeSemanticValidation": "not_performed",
                "unsupported": entry.unsupported,
                "limits": [
                    "Only exact registered resource kinds and operations; no facet or parent-schema fallback.",
                    "Structural validation is not full service validation, tenant uniqueness, reserved-name, existence or permission validation.",
                    "No service defaults are inferred or materialized; null, empty and absent remain distinct.",
                    "No revision, recursive or permanent-delete option and no recovery guarantee is registered.",
                    "No native definition, dynamic function signature or opaque JSON parameter passthrough is registered."
                ]
            },
            "examples": [{
                "discovery": entry.example,
                "sourceUrl": format!("{}#examples", entry.sources[0].url)
            }],
            "sources": entry.sources
        })
    }

    fn target_descriptor(&self) -> Value {
        json!({
            "path": self.target.path(),
            "kind": self.target.kind(),
            "facet": "resource",
            "representation": self.registration.representation,
            "selection": "lexical_only",
            "resolution": "unverified",
            "authorization": "not_evaluated",
            "intent": if self.operation() == Operation::Create {
                "prospective_destination"
            } else {
                "existing_target_unverified"
            }
        })
    }
}

impl Serialize for OperationContract {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.descriptor().serialize(serializer)
    }
}

/// Discover an exact operation contract without a body, I/O or authorization.
///
/// # Errors
/// Returns [`DiscoveryError::UnsupportedCapability`] when this precise
/// operation/kind pair is unregistered. Parsing a recognized suffix is never
/// treated as existence, execution availability or authorization evidence.
pub fn discover(
    operation: Operation,
    target: &TargetSelector,
) -> Result<OperationContract, DiscoveryError> {
    let registration = registrations()
        .iter()
        .find(|entry| entry.operation == operation && entry.kind == target.kind())
        .ok_or(DiscoveryError::UnsupportedCapability {
            reason: "the exact operation and resource kind have no registered contract",
        })?;
    Ok(OperationContract {
        registration,
        target: target.clone(),
    })
}

/// Apply an artifact facade's exact type narrowing to the same registration.
///
/// # Errors
/// Returns [`DiscoveryError::UnsupportedCapability`] for a mismatched kind or
/// unregistered operation; it never creates a second facade schema catalogue.
pub fn discover_for_facade(
    operation: Operation,
    target: &TargetSelector,
    kind: TargetKind,
) -> Result<OperationContract, DiscoveryError> {
    if target.kind() != kind {
        return Err(DiscoveryError::UnsupportedCapability {
            reason: "the artifact facade kind does not match the exact target kind",
        });
    }
    discover(operation, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODE: Field = Field {
        name: "mode",
        description: "Synthetic fixture only; not a Fabric update mode.",
        required: true,
        nullable: false,
        default: Some("safe"),
        constraint: StringConstraint {
            max_length: Some(5),
            uuid: false,
            choices: &["safe", "quick"],
        },
    };
    const LEFT: Field = Field {
        name: "left",
        required: false,
        nullable: true,
        default: None,
        ..MODE
    };
    const RIGHT: Field = Field {
        name: "right",
        ..LEFT
    };
    static FIXTURE: ObjectConstraint = ObjectConstraint {
        fields: &[MODE, LEFT, RIGHT],
        exclusions: &[&["left", "right"]],
    };
    static REGISTRATION: OperationRegistration = OperationRegistration {
        operation: Operation::Update,
        kind: TargetKind::Workspace,
        id: "synthetic.fixture.v1",
        summary: "Test-only registration; not Fabric support.",
        input: InputConstraint::Required(&FIXTURE),
        method: "PATCH",
        representation: "test_only",
        native_semantics: "test_only",
        effects: &[],
        prerequisites: &[],
        execution_result: "test_only",
        unsupported: &["production"],
        example: "test-only fixture",
        sources: &[Source {
            url: "https://example.invalid/fixture",
            revision: "synthetic",
            scope: "test-only constraints; no external schema is fetched",
        }],
    };

    #[test]
    fn fixture_preserves_required_default_enum_nullable_and_exclusions_across_heads() {
        let contract = OperationContract {
            registration: &REGISTRATION,
            target: TargetSelector::parse("/Synthetic.Workspace").expect("fixture target"),
        };
        let schema = contract.input_schema();
        assert_eq!(schema["required"], json!(["mode"]));
        assert_eq!(schema["properties"]["mode"]["default"], "safe");
        assert_eq!(
            schema["properties"]["mode"]["enum"],
            json!(["safe", "quick"])
        );
        assert_eq!(
            schema["properties"]["left"]["enum"],
            json!(["safe", "quick", null])
        );
        assert_eq!(
            schema["allOf"],
            json!([{"not": {"required": ["left", "right"]}}])
        );
        for good in [
            json!({"mode": "safe"}),
            json!({"mode": "quick", "left": null}),
        ] {
            assert!(contract.validate_input(Some(&good)).is_ok());
        }
        for bad in [
            json!({}),
            json!({"mode": null}),
            json!({"mode": "other"}),
            json!({"mode": "safe", "left": "safe", "right": null}),
            json!({"mode": "safe", "extra": "quick"}),
        ] {
            assert!(contract.validate_input(Some(&bad)).is_err());
        }
        let value = json!({"left": "safe"});
        let before = value.clone();
        assert!(contract.validate_input(Some(&value)).is_err());
        assert_eq!(value, before, "defaults must never be materialized");
        assert_eq!(
            contract.mcp_tool()["inputSchema"]["properties"]["input"],
            schema
        );
        assert_eq!(contract.descriptor()["input"]["schema"], schema);
        assert_eq!(REGISTRATION.cli_descriptor()["input"]["schema"], schema);
        let help = contract.help();
        for expected in [
            "required",
            "default=",
            "enum=",
            "nullable",
            "At most one present",
        ] {
            assert!(help.contains(expected));
        }
    }
}
