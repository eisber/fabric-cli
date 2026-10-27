// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde_json::{Value, json};

/// Explicit OneLake address, not a local OS path or an ambient context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TablePath {
    workspace: String,
    item: String,
    local_path: String,
}

impl TablePath {
    /// Validate opaque workspace/item IDs and a resolved relative OneLake path.
    ///
    /// Display-name, home, folder and existence resolution belong to the caller.
    pub fn new(
        workspace: impl Into<String>,
        item: impl Into<String>,
        local_path: impl Into<String>,
    ) -> Result<Self, crate::TableError> {
        let path = Self {
            workspace: workspace.into(),
            item: item.into(),
            local_path: local_path.into(),
        };
        if !safe_segment(&path.workspace)
            || !safe_segment(&path.item)
            || path.local_path.split('/').any(|part| !safe_segment(part))
        {
            return Err(crate::TableError::invalid_path());
        }
        Ok(path)
    }

    /// Resolved workspace ID.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Resolved item ID.
    pub fn item(&self) -> &str {
        &self.item
    }

    /// Exact path inside the item, including `Tables` or `Files`.
    pub fn local_path(&self) -> &str {
        &self.local_path
    }

    /// Final path component, as in Python `add_table_props_to_args`.
    pub fn table_name(&self) -> &str {
        self.local_path.rsplit('/').next().expect("validated path")
    }

    /// Python adds a schema only when there are exactly three path components.
    pub fn schema_name(&self) -> Option<&str> {
        let parts: Vec<_> = self.local_path.split('/').collect();
        (parts.len() == 3).then(|| parts[1])
    }

    pub(crate) fn storage_segments(&self) -> Vec<&str> {
        std::iter::once(self.workspace.as_str())
            .chain(std::iter::once(self.item.as_str()))
            .chain(self.local_path.split('/'))
            .collect()
    }
}

pub(crate) fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains(['/', '\\', '\0'])
        && !value.chars().any(char::is_control)
}

/// Load mode accepted by Python's parser.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoadMode {
    /// Python's default, including for an existing table.
    #[default]
    Overwrite,
    /// Append rows.
    Append,
}

/// Typed source classification from the OneLake properties response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    /// A single file.
    File,
    /// A folder; load is always recursive, as in the source handler.
    Folder,
}

/// Validated load options. CSV header and delimiter intentionally retain JSON types.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadOptions {
    /// Append or overwrite; the default is overwrite.
    pub mode: LoadMode,
    /// Wire format options; construct with [`Self::csv`] or [`Self::parquet`].
    format_options: Value,
    /// Optional file extension filter, with no automatic dot/case rewriting.
    pub extension: Option<String>,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self::csv(None, None)
    }
}

impl LoadOptions {
    /// CSV defaults do not add header/delimiter despite Python's help text.
    pub fn csv(header: Option<Value>, delimiter: Option<Value>) -> Self {
        let mut format = json!({"format": "Csv"});
        if let Some(header) = header {
            format["header"] = header;
        }
        if let Some(delimiter) = delimiter {
            format["delimiter"] = delimiter;
        }
        Self {
            mode: LoadMode::Overwrite,
            format_options: format,
            extension: None,
        }
    }

    /// Parquet ignores header/delimiter in the Python handler.
    pub fn parquet() -> Self {
        Self {
            format_options: json!({"format": "Parquet"}),
            ..Self::default()
        }
    }

    /// Exact Python request shape after the source properties lookup.
    pub fn payload(&self, source: &TablePath, kind: SourceKind) -> Value {
        let mut payload = json!({
            "pathType": match kind { SourceKind::File => "File", SourceKind::Folder => "Folder" },
            "relativePath": source.local_path(),
            "mode": match self.mode { LoadMode::Overwrite => "Overwrite", LoadMode::Append => "Append" },
            "formatOptions": self.format_options,
        });
        if let Some(extension) = &self.extension
            && !extension.is_empty()
        {
            payload["extension"] = extension.clone().into();
        }
        if kind == SourceKind::Folder {
            payload["recursive"] = true.into();
        }
        payload
    }
}

/// Table-specific maintenance configuration, consumed by the generic jobs owner.
#[derive(Clone, Debug, PartialEq)]
pub enum Maintenance {
    /// Python's `days:hh:00:00` retention, including zero and negative values.
    Vacuum {
        /// Already translated retention period.
        retention_period: String,
    },
    /// Compaction with optional V-Order and comma-split Z-Order columns.
    Optimize {
        /// Omitted unless true.
        vorder: bool,
        /// `None` omits the key; empty columns inside a supplied list are retained.
        zorder: Option<Vec<String>>,
    },
}

impl Maintenance {
    /// Execution data for generic `TableMaintenance` jobs, without temporary files.
    pub fn execution_data(&self, table: &TablePath) -> Value {
        let mut config = match self {
            Self::Vacuum { retention_period } => {
                json!({"vacuumSettings": {"retentionPeriod": retention_period}})
            }
            Self::Optimize { vorder, zorder } => {
                let mut settings = json!({});
                if *vorder {
                    settings["vOrder"] = true.into();
                }
                if let Some(zorder) = zorder {
                    settings["zOrderBy"] = json!(zorder);
                }
                json!({"optimizeSettings": settings})
            }
        };
        config["tableName"] = table.table_name().into();
        if let Some(schema) = table.schema_name() {
            config["schemaName"] = schema.into();
        }
        config
    }
}
