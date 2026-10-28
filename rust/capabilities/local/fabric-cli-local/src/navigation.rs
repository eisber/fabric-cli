// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use fabric_cli_context::{
    HierarchyLookup, HomeContext, PathElement, PathInput, PathResolution, PathResolver,
    ResolveOptions,
};
use fabric_cli_output::json_dumps;
use serde_json::json;

use crate::{BrowserIdentity, Capacity, ConfigStore, LocalEffects, LocalError};

/// Explicit navigation state for one caller-selected shell/session.
///
/// The caller supplies its actual OS working directory and shell ID. No process
/// ancestry probing, HOME lookup, global cwd mutation, or stale-file sweep occurs.
pub struct NavigationState {
    current: Arc<PathElement>,
    home: HomeContext,
    local_directory: String,
    session_id: u32,
    command_line: bool,
}

impl NavigationState {
    /// Bind existing context metadata, home, OS directory, session and runtime mode.
    pub fn new(
        current: Arc<PathElement>,
        home: HomeContext,
        local_directory: String,
        session_id: u32,
        command_line: bool,
    ) -> Result<Self, LocalError> {
        current.root()?;
        Ok(Self {
            current,
            home,
            local_directory,
            session_id,
            command_line,
        })
    }

    /// Borrow the actual current immutable metadata.
    pub fn current(&self) -> &Arc<PathElement> {
        &self.current
    }

    /// Resolve using only the existing public context algorithms and injected lookup.
    pub fn resolve(
        &self,
        lookup: &(impl HierarchyLookup + ?Sized),
        path: &[String],
    ) -> Result<PathResolution, LocalError> {
        let tokens = path.iter().map(String::as_str).collect::<Vec<_>>();
        let resolver =
            PathResolver::new(lookup, Arc::clone(&self.current), self.home.clone(), "ls")?;
        resolver
            .resolve(PathInput::Tokens(&tokens), ResolveOptions::default())
            .map_err(LocalError::Path)
    }

    /// Exact session-scoped source file under the injected non-secret config root.
    pub fn context_file(&self, store: &ConfigStore) -> PathBuf {
        store
            .root()
            .join(format!("context-{}.json", self.session_id))
    }

    /// Change state after optional successful persistence, preserving old state on I/O failure.
    pub fn change(
        &mut self,
        store: &ConfigStore,
        context: Arc<PathElement>,
        persistence: bool,
    ) -> Result<(), LocalError> {
        context.root()?;
        if persistence && self.command_line {
            // Config initialization owns creation/restriction of this directory.
            store.initialize()?;
            let path = self.context_file(store);
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                match fs::metadata(&path) {
                    Ok(_) => fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                options.mode(0o600);
            }
            options
                .open(path)?
                .write_all(json_dumps(&json!({"path": context.path()}))?.as_bytes())?;
        }
        self.current = context;
        Ok(())
    }

    /// Restore one session file through existing resolution; leave other files untouched.
    ///
    /// Returns whether a file was loaded. The caller renders the source
    /// `Command context path` warning when true. Corrupt files fail explicitly
    /// without deleting evidence or resetting the context.
    pub fn restore(
        &mut self,
        store: &ConfigStore,
        lookup: &(impl HierarchyLookup + ?Sized),
    ) -> Result<bool, LocalError> {
        if !self.command_line
            || store
                .initialize()?
                .get("context_persistence_enabled")
                .and_then(serde_json::Value::as_str)
                != Some("true")
        {
            return Ok(false);
        }
        let content = match fs::read_to_string(self.context_file(store)) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        let value: serde_json::Value = serde_json::from_str(&content).map_err(|_| {
            LocalError::command("ContextLoadFailed", "Failed to load command context")
        })?;
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                LocalError::command("ContextLoadFailed", "Failed to load command context")
            })?;
        if path.is_empty() {
            return Ok(false);
        }
        self.current = self.resolve(lookup, &[path.to_owned()])?.element;
        Ok(true)
    }
}

/// Ready-to-call family adapter over existing context resolution and host effects.
///
/// Central code supplies current/home/catalog/cache/platform inputs, not a second
/// implementation of navigation, persistence, browser URLs or command semantics.
pub struct NavigationHost<'a, L: HierarchyLookup + ?Sized, E: LocalEffects + ?Sized> {
    /// Caller-owned mutable session state.
    pub state: &'a mut NavigationState,
    /// Shared context metadata lookup, already populated by the host.
    pub lookup: &'a L,
    /// Explicit non-secret file store.
    pub store: &'a ConfigStore,
    /// Platform and cache adapter; tests supply offline captures.
    pub effects: &'a mut E,
}

impl<L: HierarchyLookup + ?Sized, E: LocalEffects + ?Sized> LocalEffects
    for NavigationHost<'_, L, E>
{
    fn clear_cli_cache(&mut self) -> Result<(), LocalError> {
        self.effects.clear_cli_cache()
    }
    fn capacities(&mut self) -> Result<Vec<Capacity>, LocalError> {
        self.effects.capacities()
    }
    fn current_context(&self) -> Result<Arc<PathElement>, LocalError> {
        Ok(Arc::clone(self.state.current()))
    }
    fn local_directory(&self) -> Result<String, LocalError> {
        Ok(self.state.local_directory.clone())
    }
    fn resolve_path(&mut self, path: &[String]) -> Result<PathResolution, LocalError> {
        self.state.resolve(self.lookup, path)
    }
    fn change_context(
        &mut self,
        context: Arc<PathElement>,
        persistence: bool,
    ) -> Result<(), LocalError> {
        self.state.change(self.store, context, persistence)
    }
    fn browser_identity(&self) -> Result<BrowserIdentity, LocalError> {
        self.effects.browser_identity()
    }
    fn open_browser(&mut self, url: &str) -> Result<(), LocalError> {
        self.effects.open_browser(url)
    }
    fn clear_terminal(&mut self) -> Result<(), LocalError> {
        self.effects.clear_terminal()
    }
}
