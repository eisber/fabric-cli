// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::SqlInputError;
use serde::{Deserialize, Deserializer, de::Error};
use std::{fmt, str::FromStr};

/// A native GUID with canonical lowercase rendering and redacted Debug.
///
/// Parsing accepts exactly the 8-4-4-4-12 hexadecimal form, without whitespace,
/// braces, percent encoding or a UUID-version restriction.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Guid([u8; 16]);

impl FromStr for Guid {
    type Err = SqlInputError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 36 {
            return Err(SqlInputError::InvalidGuid);
        }
        let mut digits = [0_u8; 32];
        let mut next = 0;
        for (index, byte) in value.bytes().enumerate() {
            if matches!(index, 8 | 13 | 18 | 23) {
                if byte != b'-' {
                    return Err(SqlInputError::InvalidGuid);
                }
            } else {
                digits[next] = match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    b'A'..=b'F' => byte - b'A' + 10,
                    _ => return Err(SqlInputError::InvalidGuid),
                };
                next += 1;
            }
        }
        let mut bytes = [0_u8; 16];
        for (index, pair) in digits.as_chunks::<2>().0.iter().enumerate() {
            bytes[index] = (pair[0] << 4) | pair[1];
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Guid(<redacted>)")
    }
}

impl<'de> Deserialize<'de> for Guid {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

macro_rules! semantic_id {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(Clone, Copy, Eq, Hash, PartialEq, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Guid);

        impl FromStr for $name {
            type Err = SqlInputError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}

semantic_id!(
    WorkspaceId,
    "A concrete Fabric workspace GUID, not a folder or item ID."
);
semantic_id!(
    SqlDatabaseId,
    "A concrete SQLDatabase item GUID, not a logical ID."
);
semantic_id!(FolderId, "A concrete Fabric folder GUID.");

/// An explicitly SQLDatabase-owned concrete metadata target.
///
/// The type check validates caller-supplied identity metadata, not remote
/// authorization or proof that the service currently contains the item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqlDatabaseTarget {
    workspace_id: WorkspaceId,
    database_id: SqlDatabaseId,
}

impl SqlDatabaseTarget {
    /// Binds native IDs to the exact SQLDatabase item discriminator.
    ///
    /// # Errors
    /// Rejects another owner/type before a request can be constructed.
    ///
    /// ```compile_fail
    /// use fabric_sdk_sql_database::{FolderId, SqlDatabaseTarget, WorkspaceId};
    /// let workspace: WorkspaceId = "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
    /// let folder: FolderId = "01234567-89ab-cdef-0123-456789abcdef".parse().unwrap();
    /// SqlDatabaseTarget::new(workspace, folder, "SQLDatabase").unwrap();
    /// ```
    pub fn new(
        workspace_id: WorkspaceId,
        database_id: SqlDatabaseId,
        item_type: &str,
    ) -> Result<Self, SqlInputError> {
        if item_type != "SQLDatabase" {
            return Err(SqlInputError::WrongItemType);
        }
        Ok(Self {
            workspace_id,
            database_id,
        })
    }

    /// Returns the explicitly selected workspace.
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// Returns the concrete item, never a logical identifier.
    pub const fn database_id(&self) -> SqlDatabaseId {
        self.database_id
    }
}
