// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Human exact-target help over the existing local operation registrations.
//!
//! This is not a machine protocol, docs index, identity resolver or executor.
//! Structured consumers use the retained operation contracts and schemas.

use crate::{
    CONTRACT_VERSION, DiscoveryError, Operation, OperationContract, TargetSelector, discover,
    registrations,
};

/// A finite human-help selection with its original structured contracts.
///
/// Construction performs no I/O, input validation or authorization. An absent
/// verb lists only registered exact pairs, never every verb for an item suffix.
#[derive(Clone, Debug)]
pub struct TargetHelp {
    target: TargetSelector,
    operations: Vec<OperationContract>,
}

impl TargetHelp {
    /// Select exact resource contracts from the existing immutable registry.
    ///
    /// The selector is already lexically validated, not a resolved identity.
    /// Facets without their own registration cannot fall back to a parent.
    ///
    /// # Errors
    /// Returns the existing capability failure for an unregistered exact pair.
    pub fn new(target: &TargetSelector, verb: Option<Operation>) -> Result<Self, DiscoveryError> {
        let mut operations = match verb {
            Some(operation) => vec![discover(operation, target)?],
            None => registrations()
                .iter()
                .filter(|entry| entry.target_kind() == target.kind())
                .map(|entry| discover(entry.operation(), target))
                .collect::<Result<Vec<_>, _>>()?,
        };
        if operations.is_empty() {
            return Err(DiscoveryError::UnsupportedCapability {
                reason: "the exact resource kind has no registered local help contracts",
            });
        }
        operations.sort_by_key(OperationContract::canonical_id);
        Ok(Self {
            target: target.clone(),
            operations,
        })
    }

    /// Original lexical selector; existence and authorization remain unverified.
    pub fn target(&self) -> &TargetSelector {
        &self.target
    }

    /// Exact shared contracts in locale-independent canonical-ID order.
    ///
    /// SDK/MCP consumers inspect these contracts, not the human text.
    pub fn operations(&self) -> &[OperationContract] {
        &self.operations
    }

    /// Render deterministic human text without fetching docs or reading inputs.
    ///
    /// Wording is not a stable parsing interface. Registration supplies input
    /// constraints, effects, prerequisites and examples without a second schema.
    pub fn render(&self) -> String {
        let mut text = format!(
            "Local help for {}\nTarget kind: {}; resource only.\nSelection: lexical only; resolution unverified; authorization not evaluated.\nDiscovery effects: none. V2 mutation execution: unconfigured.\nCommand corpus: local operation registry, contract version {CONTRACT_VERSION}; {} registered contracts, not complete Fabric coverage.\nDocs corpus: not indexed by this help projection; no documentation search or fetch performed.\n\n",
            self.target.path(),
            self.target.kind().as_str(),
            registrations().len(),
        );
        for contract in &self.operations {
            text.push_str(&contract.help());
            text.push_str(&format!(
                "\nInspect this exact machine contract: {} <path> --schema\nPass the target shown above as one argument, quoted for your shell.\n\n",
                contract.operation().as_str(),
            ));
        }
        text.push_str(
            "Coverage: exact registered resource contracts only. Native definitions, nested facets and dynamic signatures require their own registration; no parent fallback.\nHelp text is for people. Use the structured SDK/schema/MCP projections as machine contracts; none grants permission to execute.\n",
        );
        text
    }
}
