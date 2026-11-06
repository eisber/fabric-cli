// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Entity-scoped backing-table reads and binding cleanup; never physical data deletion.

use crate::{client::*, models::*, workload::*};

impl OntologyClient<'_> {
    /// Generate a page read of tables backing this entity; unbound entities may return `[]`.
    pub fn backing_data_sources_request(
        &self,
        entity: &LineageTag,
        token: Option<&str>,
        refresh: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        let query = binding_query(token, refresh)?;
        self.request(
            &["entityTypes", entity.as_str(), "dataSources"],
            &query,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read one page of backing tables, including the valid empty ontology-first case.
    pub async fn backing_data_sources(
        &self,
        entity: &LineageTag,
        token: Option<&str>,
        refresh: bool,
    ) -> Result<Outcome<NativePage<DataSource>>, WorkloadError> {
        self.page_result(&self.backing_data_sources_request(entity, token, refresh)?)
            .await
    }

    /// Bounded backing-table traversal; cache refresh is an explicit caller choice.
    pub async fn list_backing_data_sources(
        &self,
        entity: &LineageTag,
        options: &ListOptions,
    ) -> Result<ListResult<DataSource>, WorkloadError> {
        validate_binding_options(options)?;
        let request =
            self.backing_data_sources_request(entity, None, options.refresh_binding_resolution)?;
        self.collect(request, options).await
    }

    /// Generate a read of one entity backing table, addressed by table NAME, not lineage tag.
    pub fn backing_data_source_request(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
        refresh: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        validate_segment(name.as_str(), 512)?;
        self.request(
            &["entityTypes", entity.as_str(), "dataSources", name.as_str()],
            &binding_query(None, refresh)?,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read the native TOM table backing this entity.
    pub async fn backing_data_source(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
        refresh: bool,
    ) -> Result<Outcome<NativeResource<DataSource>>, WorkloadError> {
        self.resource_result(
            &self.backing_data_source_request(entity, name, refresh)?,
            200,
        )
        .await
    }

    /// Generate a page read of named expressions referenced by this table's partitions.
    pub fn backing_expressions_request(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
        token: Option<&str>,
        refresh: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        validate_segment(name.as_str(), 512)?;
        self.request(
            &[
                "entityTypes",
                entity.as_str(),
                "dataSources",
                name.as_str(),
                "expressions",
            ],
            &binding_query(token, refresh)?,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read referenced expressions. Inline-only partition sources may produce a valid empty page.
    pub async fn backing_expressions(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
        token: Option<&str>,
        refresh: bool,
    ) -> Result<Outcome<NativePage<Expression>>, WorkloadError> {
        self.page_result(&self.backing_expressions_request(entity, name, token, refresh)?)
            .await
    }

    /// Bounded traversal of one backing table's referenced expressions.
    pub async fn list_backing_expressions(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
        options: &ListOptions,
    ) -> Result<ListResult<Expression>, WorkloadError> {
        validate_binding_options(options)?;
        let request = self.backing_expressions_request(
            entity,
            name,
            None,
            options.refresh_binding_resolution,
        )?;
        self.collect(request, options).await
    }

    /// Generate one binding detach. Backend clears affected property references; table survives.
    pub fn unbind_data_source_request(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
    ) -> Result<WorkloadRequest, WorkloadError> {
        validate_segment(name.as_str(), 512)?;
        self.request(
            &["entityTypes", entity.as_str(), "dataSources", name.as_str()],
            &[],
            WorkloadMethod::Delete,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Detach one primary/additional binding, not the table, data, ACL or Ontology item.
    pub async fn unbind_data_source(
        &self,
        entity: &LineageTag,
        name: &ObjectName,
    ) -> Result<Outcome<()>, WorkloadError> {
        self.empty_result(&self.unbind_data_source_request(entity, name)?)
            .await
    }

    /// Generate clearing all entity data-source bindings, leaving backing tables present.
    pub fn clear_data_source_bindings_request(
        &self,
        entity: &LineageTag,
    ) -> Result<WorkloadRequest, WorkloadError> {
        self.request(
            &["entityTypes", entity.as_str(), "dataSources"],
            &[],
            WorkloadMethod::Delete,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Clear all bindings and affected references; the entity becomes ontology-first.
    pub async fn clear_data_source_bindings(
        &self,
        entity: &LineageTag,
    ) -> Result<Outcome<()>, WorkloadError> {
        self.empty_result(&self.clear_data_source_bindings_request(entity)?)
            .await
    }

    /// Generate an idempotent native collection reset, retaining skeleton and item identity.
    pub fn reset_definition_request(&self) -> Result<WorkloadRequest, WorkloadError> {
        self.request(
            &["definition"],
            &[],
            WorkloadMethod::Delete,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Reset the definition's collections; never delete the Ontology item.
    ///
    /// The controller returns 204, despite the broader TypeSpec's document-returning
    /// description. No client-side item delete or reconstruction is performed.
    pub async fn reset_definition(&self) -> Result<Outcome<()>, WorkloadError> {
        self.empty_result(&self.reset_definition_request()?).await
    }

    /// Generate a native internal definition read, defaulting to projection-free.
    pub fn definition_request(
        &self,
        expanded: bool,
        refresh: bool,
    ) -> Result<WorkloadRequest, WorkloadError> {
        if expanded && !self.target.features().auto_projection {
            return Err(WorkloadError::gap(
                "expanded internal definition feature unavailable",
            ));
        }
        let mut query = binding_query(None, refresh)?;
        if expanded {
            query.push(("expand", "true"));
        }
        self.request(
            &["definition"],
            &query,
            WorkloadMethod::Get,
            None,
            Vec::new(),
            self.limits.response,
        )
    }

    /// Read the typed internal TMSL++ envelope, not the public Ontology model.
    pub async fn definition(
        &self,
        expanded: bool,
        refresh: bool,
    ) -> Result<Outcome<NativeResource<InternalDefinition>>, WorkloadError> {
        self.resource_result(&self.definition_request(expanded, refresh)?, 200)
            .await
    }
}

fn binding_query(token: Option<&str>, refresh: bool) -> Result<Vec<(&str, &str)>, WorkloadError> {
    let mut query = Vec::new();
    if let Some(token) = token {
        validate_token(token)?;
        query.push(("continuationToken", token));
    }
    if refresh {
        query.push(("refreshBindingResolution", "true"));
    }
    Ok(query)
}

fn validate_binding_options(options: &ListOptions) -> Result<(), WorkloadError> {
    ListOptions::new(options.max_pages, options.max_items, options.max_bytes)?;
    if !options.rule_entities.is_empty() {
        return Err(WorkloadError::invalid("rule filter on backing collection"));
    }
    Ok(())
}
