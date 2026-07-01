//! Static capability + service catalog. Per the design decision, services are a
//! static list whose entitlement is validated at install time (not pre-filtered).

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Hyperscaler {
    pub id: String,
    pub name: String,
    /// `true` when a working `Provisioner` impl exists; others are "coming soon".
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Service {
    pub id: String,
    pub name: String,
    /// Preselected in the UI (watsonx.data is the product default).
    pub default_selected: bool,
    /// Component token written into `cpd_vars.sh` `COMPONENTS`.
    pub component: String,
}

/// The provider-specific cluster-spec fields for a new-cluster provision. Each
/// cloud declares its own (instance/VM types, regions/zones, node counts, …); the
/// UI renders the spec form from this, so adding a cloud is server-side only.
/// Empty for clouds without a working provisioner yet.
pub fn provider_spec(provider: &str) -> Vec<sw_core::InputField> {
    // Sourced from the provisioner registry: AWS today, empty (= "coming soon")
    // for clouds without a `Provisioner` impl yet.
    sw_mod_provision::ProvisionerRegistry::new().spec_fields(provider)
}

/// The hyperscaler catalog. AWS is enabled in v1; the rest are stubbed behind
/// the same `Provisioner` interface.
pub fn hyperscalers() -> Vec<Hyperscaler> {
    vec![
        Hyperscaler {
            id: "aws".into(),
            name: "Amazon Web Services".into(),
            enabled: true,
        },
        Hyperscaler {
            id: "ibmcloud".into(),
            name: "IBM Cloud".into(),
            enabled: false,
        },
        Hyperscaler {
            id: "azure".into(),
            name: "Microsoft Azure".into(),
            enabled: false,
        },
        Hyperscaler {
            id: "gcp".into(),
            name: "Google Cloud".into(),
            enabled: false,
        },
    ]
}

/// The service catalog for IBM Software Hub 5.4.x. watsonx.data is preselected,
/// watsonx.data premium next, then the other installable services. `component`
/// is the `cpd-cli manage apply-cr --components` token. Entitlement is validated
/// at install time, not pre-filtered.
pub fn services() -> Vec<Service> {
    // (display name, component id, default_selected)
    let rows: &[(&str, &str, bool)] = &[
        ("watsonx.data", "watsonx_data", true),
        ("watsonx.data premium", "watsonx_data_premium", false),
        ("AI Factsheets", "factsheet", false),
        (
            "Analytics Engine powered by Apache Spark",
            "analyticsengine",
            false,
        ),
        ("Cognos Analytics", "cognos_analytics", false),
        ("Cognos Dashboards", "cognos_dashboards", false),
        ("Data Gate", "datagate", false),
        ("Data Privacy", "dp", false),
        ("Data Product Hub", "dataproduct", false),
        ("Data Refinery", "datarefinery", false),
        ("Data Replication", "replication", false),
        ("DataStage Enterprise", "datastage_ent", false),
        ("DataStage Enterprise Plus", "", false),
        ("Data Virtualization", "dv", false),
        ("Db2", "db2oltp", false),
        ("Db2 Big SQL", "bigsql", false),
        ("Db2 Data Management Console", "dmc", false),
        ("Db2 Warehouse", "db2wh", false),
        ("Decision Optimization", "dods", false),
        ("EDB Postgres", "edb_cp4d,postgresql", false),
        ("Execution Engine for Apache Hadoop", "hee", false),
        ("IBM Knowledge Catalog", "ikc", false),
        ("IBM Knowledge Catalog Premium", "ikc_premium", false),
        ("IBM Knowledge Catalog Standard", "ikc_standard", false),
        ("IBM Manta Data Lineage", "datalineage", false),
        ("IBM Master Data Management", "match360", false),
        ("IBM StreamSets", "streamsets,ibm-streamsets-sdi", false),
        ("Informix", "informix_cp4d,informix", false),
        ("MANTA Automated Data Lineage", "mantaflow", false),
        ("OpenPages", "openpages", false),
        ("Orchestration Pipelines", "ws_pipelines", false),
        ("Planning Analytics", "planning_analytics", false),
        ("Product Master", "productmaster", false),
        ("RStudio® Server Runtimes", "rstudio", false),
        ("SPSS Modeler", "spss", false),
        ("Synthetic Data Generator", "syntheticdata", false),
        ("Unstructured Data Integration", "udp", false),
        ("Voice Gateway", "voice_gateway", false),
        ("Watsonx Discovery", "watson_discovery", false),
        ("Watson Machine Learning", "wml", false),
        ("Watson OpenScale", "openscale", false),
        ("Watson Speech services", "watson_speech", false),
        ("Watson Studio", "ws", false),
        ("Watson Studio Runtimes", "ws_runtimes", false),
        ("watsonx.ai", "watsonx_ai", false),
        ("watsonx.ai model gateway", "model_gateway", false),
        ("watsonx Assistant", "watson_assistant", false),
        ("watsonx BI", "watsonx_bi_assistant", false),
        ("watsonx Code Assistant", "wca", false),
        (
            "watsonx Code Assistant for Red Hat Ansible Lightspeed",
            "wca_ansible",
            false,
        ),
        (
            "watsonx Code Assistant for Z Agentic",
            "wca_z_agentic",
            false,
        ),
        (
            "watsonx Code Assistant for Z Understand",
            "wca_z_understand",
            false,
        ),
        ("watsonx.data integration", "watsonx_dataintegration", false),
        (
            "watsonx.data intelligence",
            "watsonx_dataintelligence",
            false,
        ),
        ("watsonx.governance", "watsonx_governance", false),
        ("watsonx Orchestrate", "watsonx_orchestrate", false),
    ];
    rows.iter()
        .map(|(name, component, default_selected)| Service {
            id: component.replace('_', "-"),
            name: name.to_string(),
            default_selected: *default_selected,
            component: component.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_is_the_only_enabled_hyperscaler_in_v1() {
        let enabled: Vec<_> = hyperscalers().into_iter().filter(|h| h.enabled).collect();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "aws");
    }

    #[test]
    fn watsonx_data_is_the_default_service() {
        let s = services();
        let def: Vec<_> = s.iter().filter(|x| x.default_selected).collect();
        assert_eq!(def.len(), 1);
        assert_eq!(def[0].id, "watsonx-data");
        assert_eq!(def[0].component, "watsonx_data");
    }
}
