// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Item-definition import command orchestration and cache effects.

use super::*;

pub(super) async fn import<E: OperationExecutor + ?Sized>(
    host: &mut DefinitionCommandContext<'_, E>,
    command: &TransferCommand,
    context: TransferContext,
    cancelled: &mut Cancellation<'_>,
) -> Result<TransferResult, TransferError> {
    let item = &context.target;
    let (workspace, item_type) = item_parts(item)?;
    definitions::resolve_format(item_type, command.format.as_deref())
        .map_err(|kind| TransferError::from(DefinitionError::from(kind)))?;
    let input = process_tokens(
        command
            .input
            .as_deref()
            .ok_or_else(|| invalid("Input path is required"))?,
    );
    phase(host.metadata.validate_import_input(&input), cancelled).await?;
    if input.is_empty() {
        return Err(command_error("InvalidPath", "No such file or directory").into());
    }
    let root = phase(
        async {
            host.files
                .resolve_import_async(&input)
                .await
                .map_err(local_error)
        },
        cancelled,
    )
    .await?;
    if !command.force && !phase(host.ui.confirm("Are you sure?"), cancelled).await? {
        return Ok(TransferResult {
            declined: true,
            ..TransferResult::default()
        });
    }
    let folder_id = item
        .parent()
        .filter(|parent| parent.kind() == NodeKind::Folder)
        .map(|parent| require_id(parent))
        .transpose()?;
    let target = ImportTarget {
        workspace_id: require_id(&workspace)?,
        item_id: item.id(),
        item_type,
        display_name: item.short_name(),
        folder_id,
    };
    let preparation = if item_type == "Environment" {
        host.files.prepare_environment_import_async(
            &root,
            target,
            command.format.as_deref(),
            host.definition_limits,
        )
    } else {
        host.files.prepare_import_async(
            &root,
            target,
            command.format.as_deref(),
            host.definition_limits,
        )
    };
    let prepared = phase(preparation, cancelled)
        .await
        .map_err(environment::source_definition_error)?;
    if item.id().is_some() {
        host.ui.emit(TransferEvent::Warning(
            "An item with the same name exists".into(),
        ))?;
        if !command.force && !phase(host.ui.confirm("Overwrite?"), cancelled).await? {
            return Ok(TransferResult {
                effects: vec![ImportCacheEffect {
                    target: item.clone(),
                    item_id: require_id(item)?.into(),
                }],
                declined: true,
                ..TransferResult::default()
            });
        }
    }
    host.ui.emit(TransferEvent::Grey(format!(
        "Importing{} '{}' \u{2192} '{}'...",
        if item.id().is_some() { " (update)" } else { "" },
        root.display(),
        item.path(),
    )))?;
    let mut session = OperationSession::with_context(
        host.client.context(),
        BorrowedExecutor(host.executor),
        host.operation_limits,
        cancelled.as_mut(),
    );
    if item_type == "Environment"
        && let Some(id) = item.id()
    {
        environment::ready(
            &mut session,
            host.executor,
            require_id(&workspace)?,
            id,
            host.ui,
            host.operation_limits.max_poll_delay,
        )
        .await?;
    }
    let outcome = prepared.execute(&mut session).await?;
    drop(session);
    let effect = ImportCacheEffect {
        target: item.clone(),
        item_id: outcome.item_id,
    };
    if item_type == "Environment" {
        let mut session = OperationSession::with_context(
            host.client.context(),
            BorrowedExecutor(host.executor),
            host.operation_limits,
            cancelled.as_mut(),
        );
        environment::publish(
            &mut session,
            host.executor,
            require_id(&workspace)?,
            &effect.item_id,
            host.ui,
            host.operation_limits.max_poll_delay,
        )
        .await
        .map_err(|mut error| {
            error.effects.push(effect.clone());
            error
        })?;
    }
    Ok(TransferResult {
        outputs: vec![message(format!("'{}' imported", item.name()))],
        effects: vec![effect],
        excluded_paths: outcome.excluded_paths,
        ..TransferResult::default()
    })
}
