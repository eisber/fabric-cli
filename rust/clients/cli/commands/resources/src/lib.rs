// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatibility facade for the capability-owned access, API and catalog packages.
//! Existing public imports retain the same types and behavior.

pub mod frontdoor;

pub use fabric_cli_access::{
    AclChange, AclExportSink, AclRead, LabelChange, MutationOptions, NamedResource, acl_resource,
    export_acls, labels_from_bytes, list_local_labels, mutate_acl, mutate_label, project_acls,
    read_acls, read_named_acls,
};
pub use fabric_cli_api::{ApiInput, ApiOptions, api, build_api_request, read_api_input};
pub use fabric_cli_catalog::{
    FindOptions, FindOutput, HiddenNameKind, HiddenRead, build_search_options, domain_name_by_id,
    find, find_hidden_name, find_interactive, project_search, read_hidden_metadata,
    resolve_hidden_name, service,
};
pub use fabric_cli_command_contracts::{
    CommandError, CommandValue, Confirmation, FamilyPresentation, FamilySurface,
    ResourceHttpExecutor, render_family_error, render_family_output, write_error,
};

/// Source registration supplied to the separately owned CLI front door.
#[derive(Clone, Copy, Debug)]
pub struct Registration {
    /// Canonical Python command path.
    pub path: &'static str,
    /// Full alias paths.
    pub aliases: &'static [&'static str],
}

/// Existing library registrations, not new binary or live-parity claims.
pub const REGISTRATIONS: &[Registration] = &[
    Registration {
        path: "acl ls",
        aliases: &["acl dir"],
    },
    Registration {
        path: "acl get",
        aliases: &[],
    },
    Registration {
        path: "label list-local",
        aliases: &[],
    },
    Registration {
        path: "acl set",
        aliases: &["acl grant"],
    },
    Registration {
        path: "acl rm",
        aliases: &["acl del", "acl revoke"],
    },
    Registration {
        path: "label set",
        aliases: &[],
    },
    Registration {
        path: "label rm",
        aliases: &["label del"],
    },
    Registration {
        path: "find",
        aliases: &[],
    },
    Registration {
        path: "api",
        aliases: &[],
    },
];
