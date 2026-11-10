// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Item-definition bulk export command orchestration.

use super::*;

impl<E: OperationExecutor + ?Sized> TransferHost<'_, E> {
    pub(super) async fn bulk(
        &mut self,
        command: &TransferCommand,
        context: TransferContext,
        cancelled: &mut Cancellation<'_>,
    ) -> Result<TransferResult, TransferError> {
        let scope = match context.target.kind() {
            NodeKind::Workspace(_) => BulkScope::Workspace,
            NodeKind::Folder => BulkScope::Folder,
            _ => {
                return Err(command_error(
                    "InvalidOperation",
                    format!(
                        "'bulk-export' requires a workspace or folder path, but got '{}'",
                        context.target.name()
                    ),
                )
                .into());
            }
        };
        let output = process_tokens(
            command
                .output
                .as_deref()
                .ok_or_else(|| invalid("Output path is required"))?,
        );
        if output.is_empty() {
            return Err(command_error(
                "InvalidOperation",
                "Export path '' is not a valid directory",
            )
            .into());
        }
        let root = phase(self.export_directory(&output), cancelled).await?;
        let display = self.files.export_display_path(&root, &output);
        if self.files.export_storage(&root) != definitions::ExportStorage::Local {
            return Err(command_error(
                "InvalidOperation",
                format!("Export path '{display}' is not a valid directory"),
            )
            .into());
        }
        let nonempty = !phase(
            async { self.files.is_empty_async(&root).await.map_err(local_error) },
            cancelled,
        )
        .await?;
        if nonempty
            && !command.force
            && !phase(
                self.ui.confirm(&format!(
                    "Output folder '{}' is not empty. Do you want to proceed?",
                    display,
                )),
                cancelled,
            )
            .await?
        {
            return Ok(TransferResult {
                declined: true,
                ..TransferResult::default()
            });
        }
        if command.force {
            self.ui.emit(TransferEvent::Warning(
                "Item definitions are exported without their sensitivity labels".into(),
            ))?;
            if nonempty {
                self.ui.emit(TransferEvent::Warning(
                    "Exporting to a non-empty output folder".into(),
                ))?;
            }
        } else if !phase(
            self.ui.confirm(
                "Item definitions are exported without their sensitivity labels. Are you sure?",
            ),
            cancelled,
        )
        .await?
        {
            return Ok(TransferResult {
                declined: true,
                ..TransferResult::default()
            });
        }
        let context = phase(
            self.metadata.complete_snapshot(command.kind, context),
            cancelled,
        )
        .await?;
        self.check_snapshot(&context)?;
        let workspace = context
            .target
            .workspace()
            .ok_or_else(|| invalid("Workspace metadata is required"))?;
        let items: Vec<_> = context
            .items
            .iter()
            .map(|item| {
                let (_, item_type) = item_parts(&item.element)?;
                Ok(BulkItem {
                    id: require_id(&item.element)?.into(),
                    item_type: item_type.into(),
                    support: item.support,
                })
            })
            .collect::<Result<_, TransferError>>()?;
        if items.is_empty() {
            return Err(command_error(
                "InvalidOperation",
                format!("No items found in '{}'", context.target.name()),
            )
            .into());
        }
        if scope == BulkScope::Folder
            && !items
                .iter()
                .any(|item| item.support == CommandSupport::Supported)
        {
            let message = if items
                .iter()
                .any(|item| item.support == CommandSupport::Rejected)
            {
                "No exportable items found. All items have unsupported types".into()
            } else {
                format!("No items found in '{}'", context.target.name())
            };
            return Err(command_error("InvalidOperation", message).into());
        }
        let path = context.target.path();
        let snapshot = BulkExportSnapshot {
            workspace_id: require_id(&workspace)?,
            from_path: path.trim_matches('/'),
            scope,
            items: &items,
        };
        let mut session = OperationSession::with_context(
            self.client.context(),
            BorrowedExecutor(self.executor),
            self.operation_limits,
            cancelled.as_mut(),
        );
        let progress = format!(
            "Bulk-exporting '{}' \u{2192} '{}'...",
            snapshot.from_path, display
        );
        let mut files = files::ExportFiles::default();
        let mut outcome = definitions::bulk_export_source(
            &mut session,
            &mut files,
            snapshot,
            &root,
            true,
            true,
            self.definition_limits,
        )
        .await
        .map_err(|error| {
            if matches!(
                error.kind,
                DefinitionErrorKind::Unsupported(
                    "bulk target has no exportable items" | "bulk folder has no exportable items"
                )
            ) {
                command_error(
                    "InvalidOperation",
                    "No exportable items found. All items have unsupported types",
                )
                .into()
            } else {
                TransferError::from(files::ExportFiles::preparation_error(error))
            }
        })?;
        outcome.completed_paths = files
            .write(&mut session, self.files, &root, self.ui, progress)
            .await?;
        let (exported_items, skipped_items) = bulk_summary_items(
            scope,
            &items,
            &outcome.supported_ids,
            &outcome.skipped_ids,
            &outcome.indexed_ids,
        )?;
        let exported = type_counts(exported_items)?;
        let skipped = type_counts(skipped_items)?;
        let mut summary = format!(
            "Exported {} items to '{output}'",
            outcome.supported_ids.len()
        );
        if !outcome.skipped_ids.is_empty() {
            let types = skipped
                .iter()
                .map(|(kind, count)| format!("{kind} ({count})"))
                .collect::<Vec<_>>()
                .join(", ");
            summary.push_str(&format!(
                ". Skipped {} items due to unsupported item types: {types}",
                outcome.skipped_ids.len()
            ));
        }
        Ok(TransferResult {
            outputs: vec![OutputInput {
                data: Some(json!([{
                    "exported": outcome.supported_ids.len(), "exported_types": exported,
                    "skipped": outcome.skipped_ids.len(), "skipped_types": skipped, "output": output,
                }])),
                message: Some(summary),
                ..OutputInput::default()
            }],
            key_value: true,
            completed_paths: outcome.completed_paths,
            exported_ids: outcome.supported_ids,
            skipped_ids: outcome.skipped_ids,
            ..TransferResult::default()
        })
    }
}

fn bulk_summary_items<'a>(
    scope: BulkScope,
    items: &'a [BulkItem],
    supported: &[String],
    skipped: &[String],
    indexed: &[String],
) -> Result<(Vec<&'a BulkItem>, Vec<&'a BulkItem>), TransferError> {
    if scope == BulkScope::Folder {
        return Ok((
            items
                .iter()
                .filter(|item| item.support == CommandSupport::Supported)
                .collect(),
            items
                .iter()
                .filter(|item| item.support == CommandSupport::Rejected)
                .collect(),
        ));
    }
    let by_id: std::collections::HashMap<_, _> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let known = |id: &String| {
        by_id
            .get(id.as_str())
            .copied()
            .ok_or_else(|| TransferError::from(invalid("Incomplete bulk metadata snapshot")))
    };
    let exported = supported.iter().map(known).collect::<Result<Vec<_>, _>>()?;
    let mut skipped_items = skipped
        .iter()
        .filter(|id| indexed.contains(id))
        .map(known)
        .collect::<Result<Vec<_>, _>>()?;
    // Source's final missing-item pass appends the original cached objects,
    // not the last-by-ID lookup used for response-index entries.
    skipped_items.extend(items.iter().filter(|item| !indexed.contains(&item.id)));
    Ok((exported, skipped_items))
}

fn type_counts(items: Vec<&BulkItem>) -> Result<Map<String, Value>, TransferError> {
    let mut counts = Map::new();
    for item in items {
        let count = counts.entry(item.item_type.clone()).or_insert(json!(0));
        *count = json!(
            count
                .as_u64()
                .ok_or_else(|| invalid("Invalid summary count"))?
                + 1
        );
    }
    Ok(counts)
}
