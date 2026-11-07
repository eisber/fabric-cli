// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::parser::{Invocation, JobReadKind, ReadCommand};
use fabric_cli_context::NodeKind;
use fabric_cli_output::tabular::{TabularColumn, TabularTable, TabularType};
use fabric_cli_resources::frontdoor::AccessKind;

fn columns(fields: &[(&str, TabularType)]) -> Vec<TabularColumn> {
    fields
        .iter()
        .map(|(name, kind)| TabularColumn::new(*name, *kind, true))
        .collect()
}

fn strings(names: &[&str]) -> Vec<TabularColumn> {
    names
        .iter()
        .map(|name| TabularColumn::new(*name, TabularType::String, true))
        .collect()
}

fn table(columns: Vec<TabularColumn>) -> TabularTable {
    TabularTable::new(columns, Vec::new()).expect("statically declared empty schema is valid")
}

pub(super) fn listing(command: &ReadCommand, target: NodeKind) -> Option<TabularTable> {
    if target != NodeKind::Folder && projected(command.query.as_deref()) {
        return None;
    }
    Some(table(match target {
        NodeKind::Tenant if command.long => {
            strings(&["name", "id", "capacityName", "capacityId", "capacityRegion"])
        }
        NodeKind::Tenant => strings(&["name"]),
        NodeKind::Workspace(_) | NodeKind::Folder if command.long => strings(&["name", "id"]),
        NodeKind::Workspace(_) | NodeKind::Folder => strings(&["name"]),
        _ => return None,
    }))
}

pub(super) fn command(command: &Invocation) -> Option<TabularTable> {
    use TabularType::{Boolean, Json, String};
    match command {
        Invocation::Access(access) => match &access.kind {
            AccessKind::Find(options) if !projected(options.query.as_deref()) => {
                Some(table(if options.long {
                    strings(&["name", "id", "type", "workspace", "workspace_id"])
                } else {
                    strings(&["name", "type", "workspace"])
                }))
            }
            _ => None,
        },
        Invocation::Table(table)
            if table.operation == fabric_cli_tables::TableOperation::Schema =>
        {
            Some(self::table(columns(&[
                ("name", String),
                ("type", Json),
                ("nullable", Boolean),
                ("metadata", Json),
            ])))
        }
        Invocation::JobRead(job) if job.kind == JobReadKind::List => {
            // REST ItemJobInstance/ItemSchedule contracts, not first-row guesses:
            // learn.microsoft.com/rest/api/fabric/core/job-scheduler/list-item-job-instances
            // learn.microsoft.com/rest/api/fabric/core/job-scheduler/list-item-schedules
            Some(table(if job.schedule {
                columns(&[
                    ("id", String),
                    ("enabled", Boolean),
                    ("createdDateTime", String),
                    ("configuration", Json),
                    ("owner", Json),
                    ("executionData", Json),
                ])
            } else {
                columns(&[
                    ("id", String),
                    ("itemId", String),
                    ("jobType", String),
                    ("invokeType", String),
                    ("status", String),
                    ("rootActivityId", String),
                    ("startTimeUtc", String),
                    ("endTimeUtc", String),
                    ("failureReason", Json),
                ])
            }))
        }
        _ => None,
    }
}

#[cfg(feature = "native-host")]
pub(super) fn access(
    command: &fabric_cli_resources::frontdoor::AccessCommand,
    target: Option<NodeKind>,
) -> Option<TabularTable> {
    use TabularType::{Json, String};
    match &command.kind {
        AccessKind::AclList { long, query, .. } if !projected(query.as_deref()) => {
            Some(table(match target? {
                NodeKind::Workspace(_) => {
                    if *long {
                        strings(&["acl", "identity", "type", "objectId", "name"])
                    } else {
                        strings(&["acl", "identity", "type"])
                    }
                }
                NodeKind::Item(_) => {
                    if *long {
                        columns(&[
                            ("id", String),
                            ("name", String),
                            ("acl", Json),
                            ("identity", String),
                            ("type", String),
                        ])
                    } else {
                        columns(&[("acl", Json), ("identity", String), ("type", String)])
                    }
                }
                _ => return None,
            }))
        }
        AccessKind::AclGet {
            query: Some(query), ..
        } if query == "." => {
            // The mounted workspace and admin-item endpoints have declared DTOs:
            // learn.microsoft.com/rest/api/fabric/core/workspaces/list-workspace-role-assignments
            // learn.microsoft.com/rest/api/fabric/admin/items/list-item-access-details
            Some(table(match target? {
                NodeKind::Workspace(_) => {
                    columns(&[("id", String), ("principal", Json), ("role", String)])
                }
                NodeKind::Item(_) => columns(&[("principal", Json), ("itemAccessDetails", Json)]),
                _ => return None,
            }))
        }
        _ => None,
    }
}

fn projected(query: Option<&str>) -> bool {
    query.is_some_and(|query| !query.is_empty())
}
