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

/// The service catalog. watsonx.data is preselected; others are entitlement-gated
/// at install time.
pub fn services() -> Vec<Service> {
    vec![
        Service {
            id: "watsonx-data".into(),
            name: "watsonx.data".into(),
            default_selected: true,
            component: "watsonx_data".into(),
        },
        Service {
            id: "watsonx-data-premium".into(),
            name: "watsonx.data premium".into(),
            default_selected: false,
            component: "watsonx_data_premium".into(),
        },
        Service {
            id: "watsonx-ai".into(),
            name: "watsonx.ai".into(),
            default_selected: false,
            component: "watsonx_ai".into(),
        },
        Service {
            id: "data-product-hub".into(),
            name: "Data Product Hub".into(),
            default_selected: false,
            component: "data_product_hub".into(),
        },
    ]
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
