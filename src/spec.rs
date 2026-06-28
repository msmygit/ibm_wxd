//! The authoritative `cpd_vars.sh` variable contract.
//!
//! Every required variable, its validation kind, whether it is a secret, and a
//! short human-readable description live here in ONE place. The collector,
//! validator, generator and `--help` text are all driven from this single list
//! so they can never drift out of sync.
//!
//! The variable set and semantics are taken verbatim from WORKTREE.md
//! "Environment Variables" (the documented `cpd_vars.sh` contract). Do not
//! invent variable names here without a corresponding contract change.

/// How a variable's value is validated, beyond the universal "required &
/// non-empty" check that applies to every entry in [`SPEC`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationKind {
    /// Any non-empty string is accepted (e.g. credentials, storage classes,
    /// component lists, version strings). Required-ness is still enforced.
    NonEmpty,
    /// Must parse as a well-formed `http://` or `https://` URL with a host.
    Url,
    /// Must satisfy Kubernetes / RFC 1123 namespace naming rules.
    Namespace,
    /// Must be one of a documented allowed-value set. Unknown-but-plausible
    /// values are *warned* about, not rejected (see questions_answers.md Q2).
    Enum(&'static [&'static str]),
}

/// One required variable in the `cpd_vars.sh` contract.
#[derive(Debug, Clone, Copy)]
pub struct VarSpec {
    /// The shell variable name, e.g. `OCP_URL`.
    pub name: &'static str,
    /// One-line description shown in prompts and `--help`.
    pub description: &'static str,
    /// How the value is validated.
    pub validation: ValidationKind,
    /// Whether the value is sensitive and must be masked in console/log output
    /// (AC10). The generated file still carries the real value.
    pub secret: bool,
    /// An optional default applied during collection when the user supplies no
    /// value (no env var, no answers-file entry, no interactive input). When set,
    /// a missing value falls back to this default *before* validation, so the
    /// variable does not error as "required". When `None`, the variable remains
    /// strictly required. Only `VERSION` carries a default today (the target
    /// IBM Software Hub release); every other variable is `None`.
    pub default: Option<&'static str>,
}

/// Documented allowed values for `OPENSHIFT_TYPE` (WORKTREE.md examples).
pub const OPENSHIFT_TYPE_VALUES: &[&str] = &["self-managed", "roks"];

/// Documented allowed values for `IMAGE_ARCH` (WORKTREE.md examples).
pub const IMAGE_ARCH_VALUES: &[&str] = &["amd64", "s390x"];

/// Default target IBM Software Hub / Cloud Pak for Data release applied to
/// `VERSION` when the user supplies none. Currently the latest 5.4.x line
/// (patch 1 is the latest patch). The env-var contract is stable across 5.x, so
/// this is a version-reference/default change only — not a contract change.
/// Docs: https://www.ibm.com/docs/en/cloud-paks/cp-data
pub const VERSION_DEFAULT: &str = "5.4.0";

/// The complete, ordered list of required variables that must appear in a
/// generated `cpd_vars.sh`. Order here is the order they are emitted, which
/// guarantees deterministic output (AC9).
pub const SPEC: &[VarSpec] = &[
    VarSpec {
        name: "OCP_URL",
        description: "OpenShift API server URL the installer targets (https://...)",
        validation: ValidationKind::Url,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "OPENSHIFT_TYPE",
        description: "Cluster flavor (e.g. self-managed, roks)",
        validation: ValidationKind::Enum(OPENSHIFT_TYPE_VALUES),
        secret: false,
        default: None,
    },
    VarSpec {
        name: "IMAGE_ARCH",
        description: "Target image architecture (e.g. amd64, s390x)",
        validation: ValidationKind::Enum(IMAGE_ARCH_VALUES),
        secret: false,
        default: None,
    },
    VarSpec {
        name: "OCP_USERNAME",
        description: "OpenShift login username (cluster-admin for install)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "OCP_PASSWORD",
        description: "OpenShift login password",
        validation: ValidationKind::NonEmpty,
        secret: true,
        default: None,
    },
    VarSpec {
        name: "IBM_ENTITLEMENT_KEY",
        description: "Pull secret for the IBM Entitled Registry",
        validation: ValidationKind::NonEmpty,
        secret: true,
        default: None,
    },
    VarSpec {
        name: "PROJECT_CPD_INST_OPERATORS",
        description: "Namespace for CPD operators",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "PROJECT_CPD_INST_OPERANDS",
        description: "Namespace for CPD operands (watsonx.data instance)",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "STG_CLASS_BLOCK",
        description: "RWO (block) storage class for the cluster",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "STG_CLASS_FILE",
        description: "RWX (file) storage class for the cluster",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
    },
    VarSpec {
        name: "VERSION",
        description: "watsonx.data / IBM Software Hub release being installed (default 5.4.0; patch 1 is the latest)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: Some(VERSION_DEFAULT),
    },
    VarSpec {
        name: "COMPONENTS",
        description: "Component list passed to `cpd-cli manage apply-cr`",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
    },
];

/// Look up a [`VarSpec`] by its variable name.
pub fn find(name: &str) -> Option<&'static VarSpec> {
    SPEC.iter().find(|v| v.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for v in SPEC {
            assert!(seen.insert(v.name), "duplicate var in SPEC: {}", v.name);
        }
    }

    #[test]
    fn spec_covers_documented_contract() {
        // The exact required set from WORKTREE.md "Environment Variables".
        let expected = [
            "OCP_URL",
            "OPENSHIFT_TYPE",
            "IMAGE_ARCH",
            "OCP_USERNAME",
            "OCP_PASSWORD",
            "IBM_ENTITLEMENT_KEY",
            "PROJECT_CPD_INST_OPERATORS",
            "PROJECT_CPD_INST_OPERANDS",
            "STG_CLASS_BLOCK",
            "STG_CLASS_FILE",
            "VERSION",
            "COMPONENTS",
        ];
        let actual: Vec<&str> = SPEC.iter().map(|v| v.name).collect();
        assert_eq!(actual, expected, "SPEC drifted from documented contract");
    }

    #[test]
    fn secrets_are_exactly_the_sensitive_pair() {
        let secrets: Vec<&str> = SPEC.iter().filter(|v| v.secret).map(|v| v.name).collect();
        assert_eq!(secrets, ["OCP_PASSWORD", "IBM_ENTITLEMENT_KEY"]);
    }

    #[test]
    fn find_round_trips() {
        assert_eq!(find("OCP_URL").unwrap().name, "OCP_URL");
        assert!(find("NOPE").is_none());
    }

    #[test]
    fn only_version_has_a_default() {
        let with_default: Vec<&str> = SPEC
            .iter()
            .filter(|v| v.default.is_some())
            .map(|v| v.name)
            .collect();
        assert_eq!(
            with_default,
            ["VERSION"],
            "only VERSION should carry a default; everything else stays required"
        );
    }

    #[test]
    fn version_default_is_5_4_0() {
        assert_eq!(find("VERSION").unwrap().default, Some("5.4.0"));
        assert_eq!(VERSION_DEFAULT, "5.4.0");
    }
}
