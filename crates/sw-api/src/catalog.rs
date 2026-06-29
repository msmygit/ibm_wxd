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

/// The hyperscaler catalog. AWS is enabled in v1; the rest are stubbed behind
/// the same `Provisioner` interface.
pub fn hyperscalers() -> Vec<Hyperscaler> {
    vec![
        Hyperscaler { id: "aws".into(), name: "Amazon Web Services".into(), enabled: true },
        Hyperscaler { id: "ibmcloud".into(), name: "IBM Cloud".into(), enabled: false },
        Hyperscaler { id: "azure".into(), name: "Microsoft Azure".into(), enabled: false },
        Hyperscaler { id: "gcp".into(), name: "Google Cloud".into(), enabled: false },
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
        ("watsonx.ai", "watsonx_ai", false),
        ("watsonx Assistant", "watson_assistant", false),
        ("watsonx Discovery (Watson Discovery)", "watson_discovery", false),
        ("Watson Studio", "ws", false),
        ("Watson Machine Learning", "wml", false),
        ("IBM Knowledge Catalog", "wkc", false),
        ("DataStage Enterprise", "datastage_ent", false),
        ("DataStage Enterprise Plus", "datastage_ent_plus", false),
        ("Db2", "db2oltp", false),
        ("Db2 Warehouse", "db2wh", false),
        ("Db2 Data Management Console", "dmc", false),
        ("Analytics Engine (Apache Spark)", "analyticsengine", false),
        ("Cognos Analytics", "cognos_analytics", false),
        ("Cognos Dashboards", "cognos_dashboards", false),
        ("Planning Analytics", "planning_analytics", false),
        ("Decision Optimization", "dods", false),
        ("SPSS Modeler", "spss_modeler", false),
        ("OpenPages", "openpages", false),
        ("Match 360 (MDM)", "match360", false),
        ("Data Product Hub", "data_product_hub", false),
        ("RStudio", "rstudio", false),
        ("Product Master", "product_master", false),
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
