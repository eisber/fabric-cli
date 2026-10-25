// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Finite metadata routes from Python cf67fd52 `fab_types.format_mapping`.

macro_rules! collections {
    ($($variant:ident => ($item_type:literal, $segment:literal)),+ $(,)?) => {
        /// A source-known item metadata collection, never an arbitrary URL or path.
        ///
        /// This models the metadata helper's routes, not CLI command support.
        /// Some collections are rejected by Python's GET command policy before
        /// the helper is called. Definitions and augmentation are not requested.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum ItemCollection {
            $(
                #[doc = concat!("The `", $item_type, "` metadata collection.")]
                $variant,
            )+
        }

        impl ItemCollection {
            /// Maps a source item type case-insensitively, without pluralization.
            ///
            /// Unknown/future types return `None`; they never select generic GET.
            pub fn from_item_type(item_type: &str) -> Option<Self> {
                $(if item_type.eq_ignore_ascii_case($item_type) {
                    return Some(Self::$variant);
                })+
                None
            }

            /// Returns the exact pinned source collection segment.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $segment,)+
                }
            }
        }
    };
}

collections! {
    AiSkill => ("AISkill", "aiskills"),
    ApacheAirflowJob => ("ApacheAirflowJob", "apacheairflowprojects"),
    Exploration => ("Exploration", "explorations"),
    RetailDataManager => ("RetailDataManager", "retaildatamanagers"),
    HealthcareDataSolution => ("Healthcaredatasolution", "healthcaredatasolutions"),
    SustainabilityDataSolution => ("SustainabilityDataSolution", "sustainabilitydatasolutions"),
    MetricSet => ("MetricSet", "metricsets"),
    OrgApp => ("OrgApp", "orgapps"),
    CosmosDbDatabase => ("CosmosDBDatabase", "cosmosDbDatabases"),
    Dashboard => ("Dashboard", "dashboards"),
    DataPipeline => ("DataPipeline", "dataPipelines"),
    Datamart => ("Datamart", "datamarts"),
    DigitalTwinBuilder => ("DigitalTwinBuilder", "digitalTwinBuilders"),
    DigitalTwinBuilderFlow => ("DigitalTwinBuilderFlow", "digitalTwinBuilderFlows"),
    Environment => ("Environment", "environments"),
    Eventhouse => ("Eventhouse", "eventhouses"),
    Eventstream => ("Eventstream", "eventstreams"),
    GraphModel => ("GraphModel", "graphModels"),
    KqlDashboard => ("KQLDashboard", "kqlDashboards"),
    KqlDatabase => ("KQLDatabase", "kqlDatabases"),
    KqlQueryset => ("KQLQueryset", "kqlQuerysets"),
    Lakehouse => ("Lakehouse", "lakehouses"),
    Map => ("Map", "maps"),
    MlExperiment => ("MLExperiment", "mlExperiments"),
    MlModel => ("MLModel", "mlModels"),
    MirroredWarehouse => ("MirroredWarehouse", "mirroredWarehouses"),
    MirroredDatabase => ("MirroredDatabase", "mirroredDatabases"),
    Notebook => ("Notebook", "notebooks"),
    Ontology => ("Ontology", "ontologies"),
    PaginatedReport => ("PaginatedReport", "paginatedReports"),
    Reflex => ("Reflex", "reflexes"),
    Report => ("Report", "reports"),
    SqlDatabase => ("SQLDatabase", "sqlDatabases"),
    SqlEndpoint => ("SQLEndpoint", "sqlEndpoints"),
    SemanticModel => ("SemanticModel", "semanticModels"),
    SparkJobDefinition => ("SparkJobDefinition", "sparkJobDefinitions"),
    Warehouse => ("Warehouse", "warehouses"),
    CopyJob => ("CopyJob", "copyJobs"),
    GraphQlApi => ("GraphQLApi", "graphqlapis"),
    GraphQuerySet => ("GraphQuerySet", "GraphQuerySets"),
    UserDataFunction => ("UserDataFunction", "userdatafunctions"),
    MountedDataFactory => ("MountedDataFactory", "mounteddatafactories"),
    Dataflow => ("Dataflow", "dataflows"),
    VariableLibrary => ("VariableLibrary", "variablelibraries"),
}
