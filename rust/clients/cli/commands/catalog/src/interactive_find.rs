// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    CommandError, CommandValue, Confirmation, FindOptions, build_search_options, project_search,
};
use fabric_sdk_api::{ApiError, Limits, cancellable};
use fabric_sdk_catalog::CatalogClient;
use serde_json::Value;
use std::{collections::HashSet, future::Future, pin::pin};

/// A caller-owned interactive output sink. Emission occurs before the next prompt.
pub trait FindOutput {
    /// Writes/records one already-projected source output/progress event.
    fn emit(&mut self, value: CommandValue) -> Result<(), CommandError>;
}

/// Executes source interactive find without a second terminal/parser/runtime.
/// Empty pages advance without prompts; displayed rows use cumulative summaries.
/// Declining continuation is source EOF/Ctrl+C behavior, not a remote cancellation.
pub async fn find_interactive(
    client: &CatalogClient,
    options: &FindOptions,
    limits: Limits,
    prompt: &mut dyn Confirmation,
    output: &mut dyn FindOutput,
    cancelled: impl Future<Output = ()> + Send,
) -> Result<(), CommandError> {
    let search = build_search_options(options, true)?;
    output.emit(CommandValue {
        diagnostics: vec!["Searching...".into()],
        ..CommandValue::default()
    })?;
    let mut cancelled = pin!(cancelled);
    let (mut token, mut count, mut pages, mut received) = (None, 0usize, 0usize, 0usize);
    let mut seen = HashSet::new();
    let mut displayed = false;
    loop {
        if token.is_some() && displayed {
            output.emit(CommandValue {
                diagnostics: vec![String::new()],
                ..CommandValue::default()
            })?;
            if !cancellable(
                async {
                    prompt
                        .confirm("Press Enter to continue... (Ctrl+C to stop)")
                        .await
                },
                cancelled.as_mut(),
            )
            .await
            .map_err(CommandError::from)??
            {
                output.emit(CommandValue {
                    diagnostics: vec![String::new()],
                    ..CommandValue::default()
                })?;
                return Ok(());
            }
        }
        let page = client
            .search_page(
                &search,
                token.as_deref(),
                limits.max_response_bytes,
                cancelled.as_mut(),
            )
            .await
            .map_err(CommandError::from)?;
        if page.rows.len() > limits.max_items.get() - received {
            return Err(CommandError::from(ApiError::Limit));
        }
        received += page.rows.len();
        pages += 1;
        let rows = project_search(&page.rows, options)?;
        displayed = !rows.is_empty();
        if displayed {
            count += rows.len();
            output.emit(CommandValue {
                data: Some(Value::Array(rows)),
                diagnostics: vec![
                    String::new(),
                    format!(
                        "{count} {} found{}",
                        if count == 1 { "item" } else { "items" },
                        if page.next_token.is_some() {
                            " (more available)"
                        } else {
                            ""
                        }
                    ),
                    String::new(),
                    String::new(),
                ],
                ..CommandValue::default()
            })?;
        }
        token = page.next_token;
        let Some(next) = &token else {
            break;
        };
        if !seen.insert(next.clone()) {
            return Err(CommandError::from(ApiError::Continuation));
        }
        if pages >= limits.max_pages.get() {
            return Err(CommandError::from(ApiError::Limit));
        }
    }
    if count == 0 {
        output.emit(CommandValue {
            diagnostics: vec!["No items found.".into()],
            ..CommandValue::default()
        })?;
    }
    Ok(())
}
