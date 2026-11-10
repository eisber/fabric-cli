// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Single-item and selected workspace definition export orchestration.

use super::*;

pub(super) async fn export<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    command: &TransferCommand,
    context: TransferContext,
    cancelled: &mut Cancellation<'_>,
) -> Result<TransferResult, TransferError> {
    let workspace = matches!(context.target.kind(), NodeKind::Workspace(_));
    if !workspace && !matches!(context.target.kind(), NodeKind::Item(_)) {
        return Err(command_error("NotSupported", "Export requires an item or workspace").into());
    }
    if workspace && host.command_line && !(command.force || command.all) {
        return Err(command_error("NotSupported", "Not supported in 'cli' mode").into());
    }
    if command.force {
        host.ui.emit(TransferEvent::Warning(
            "Item definition is exported without its sensitivity label and its data".into(),
        ))?;
    }
    let output = process_tokens(
        command
            .output
            .as_deref()
            .ok_or_else(|| invalid("Output path is required"))?,
    );
    let mut result = TransferResult::default();
    if !workspace {
        if command.force || phase(host.ui.confirm(SINGLE_CONFIRM), cancelled).await? {
            result.completed_paths =
                export_one(host, &context.target, command, None, &output, cancelled).await?;
            result
                .exported_ids
                .push(require_id(&context.target)?.into());
        } else {
            result.declined = true;
        }
        // Python emits this message even when its single-item prompt is declined.
        result
            .outputs
            .push(message(format!("'{}' exported", context.target.name())));
        return Ok(result);
    }
    let context = phase(
        host.metadata.complete_snapshot(command.kind, context),
        cancelled,
    )
    .await?;
    host.check_snapshot(&context)?;
    if context.items.is_empty() {
        return Err(command_error("InvalidOperation", "Your workspace is empty").into());
    }
    let supported: Vec<_> = context
        .items
        .iter()
        .filter(|item| item.support == CommandSupport::Supported)
        .collect();
    if supported.is_empty() {
        return Err(command_error(
            "NotSupported",
            "Export not possible. Existing items cannot be imported/exported",
        )
        .into());
    }
    let root = phase(export_directory(host, &output), cancelled).await?;
    let names = &context.selection_order;
    let mut expected = supported
        .iter()
        .map(|item| item.element.name())
        .collect::<Vec<_>>();
    let mut actual = names.clone();
    expected.sort();
    actual.sort();
    if actual != expected {
        return Err(invalid("Complete configuration-sorted export selection is required").into());
    }
    let selected = if command.all {
        names.clone()
    } else {
        phase(host.ui.select(names), cancelled).await?
    };
    if selected.iter().any(|name| !names.contains(name)) {
        return Err(invalid("Selection contains an unknown item").into());
    }
    if selected.is_empty() {
        result.outputs.push(message("No items selected"));
        return Ok(result);
    }
    host.ui.emit(TransferEvent::Grey(selected.join("\n")))?;
    host.ui
        .emit(TransferEvent::Grey("------------------------------".into()))?;
    if !command.force && !phase(host.ui.confirm(SINGLE_CONFIRM), cancelled).await? {
        result.declined = true;
        return Ok(result);
    }
    let filtered_items = supported
        .into_iter()
        .filter(|item| selected.contains(&item.element.name()))
        .collect::<Vec<_>>();
    let count = filtered_items.len();
    for item in filtered_items {
        match export_one(
            host,
            &item.element,
            command,
            Some(&root),
            &output,
            cancelled,
        )
        .await
        {
            Ok(paths) => {
                result.completed_paths.extend(paths);
                result.exported_ids.push(require_id(&item.element)?.into());
            }
            Err(mut error) => {
                let mut paths = result.completed_paths;
                paths.append(&mut error.completed_paths);
                error.completed_paths = paths;
                let mut exported_ids = result.exported_ids;
                exported_ids.append(&mut error.exported_ids);
                error.exported_ids = exported_ids;
                return Err(error);
            }
        }
    }
    result.outputs.push(message(format!(
        "{count} {} exported successfully",
        if count > 1 { "items" } else { "item" }
    )));
    Ok(result)
}

async fn export_one<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    item: &Arc<Element>,
    command: &TransferCommand,
    root: Option<&Path>,
    output: &str,
    cancelled: &mut Cancellation<'_>,
) -> Result<Vec<String>, TransferError> {
    let (workspace, item_type) = item_parts(item)?;
    let item_id = require_id(item)?;
    let mut session = OperationSession::with_context(
        host.client.context(),
        BorrowedExecutor(host.executor),
        host.operation_limits,
        cancelled.as_mut(),
    );
    let read = definitions::read_item_definition(
        &mut session,
        DefinitionTarget {
            workspace_id: require_id(&workspace)?,
            item_id,
            item_type,
        },
        command.format.as_deref(),
    )
    .await?;
    let root = match root {
        Some(root) => root.to_owned(),
        None => files::export_directory(&mut session, host.files, output).await?,
    };
    let progress = format!(
        "Exporting '{}' \u{2192} '{}/{}'...",
        item.path().trim_matches('/'),
        host.files.export_display_path(&root, output),
        item.name(),
    );
    let mut files = files::ExportFiles::default();
    let storage = host.files.export_storage(&root);
    definitions::export_read_item(
        &mut session,
        &mut files,
        read,
        item_type,
        &item.name(),
        &root,
        host.definition_limits,
        storage,
    )
    .await
    .map_err(files::ExportFiles::preparation_error)?;
    Ok(files
        .write(&mut session, host.files, &root, host.ui, progress)
        .await?)
}
