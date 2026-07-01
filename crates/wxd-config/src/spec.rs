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

/// One variable in the `cpd_vars.sh` contract.
#[derive(Debug, Clone, Copy)]
pub struct VarSpec {
    /// The shell variable name, e.g. `OCP_URL`.
    pub name: &'static str,
    /// One-line description shown in prompts and `--help`.
    pub description: &'static str,
    /// How the value is validated (when a value is present).
    pub validation: ValidationKind,
    /// Whether the value is sensitive and must be masked in console/log output
    /// (AC10). The generated file still carries the real value.
    pub secret: bool,
    /// An optional default applied during collection when the user supplies no
    /// value (no env var, no answers-file entry, no interactive input). When set,
    /// a missing value falls back to this default *before* validation, so the
    /// variable does not error as "required". Only `VERSION` (`5.4.0`) and
    /// `PATCH_ID` (`latest`) carry defaults; every other variable is `None`.
    pub default: Option<&'static str>,
    /// When `true`, this variable is NOT subject to the universal "required"
    /// check — its presence is governed by cross-field rules instead (e.g. the
    /// choose-one cluster-auth set: `OCP_USERNAME`/`OCP_PASSWORD`/`OCP_TOKEN`).
    /// A value, if present, is still format-validated. When `false`, the
    /// variable is strictly required (unless it carries a `default`).
    pub optional: bool,
}

/// The cluster-auth variables that participate in the choose-one rule:
/// either both username+password, or a token. None is individually required.
pub const AUTH_USERNAME: &str = "OCP_USERNAME";
pub const AUTH_PASSWORD: &str = "OCP_PASSWORD";
pub const AUTH_TOKEN: &str = "OCP_TOKEN";

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

/// Default patch level applied to `PATCH_ID` when the user supplies none. The
/// 5.4.0 template's patch mechanism; `latest` selects the newest patch (patch 1
/// at time of writing).
pub const PATCH_ID_DEFAULT: &str = "latest";

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
        optional: false,
    },
    VarSpec {
        name: "OPENSHIFT_TYPE",
        description: "Cluster flavor (e.g. self-managed, roks)",
        validation: ValidationKind::Enum(OPENSHIFT_TYPE_VALUES),
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "IMAGE_ARCH",
        description: "Target image architecture (e.g. amd64, s390x)",
        validation: ValidationKind::Enum(IMAGE_ARCH_VALUES),
        secret: false,
        default: None,
        optional: false,
    },
    // ---- cluster auth: choose-one (username+password OR token) ----
    VarSpec {
        name: AUTH_USERNAME,
        description: "OpenShift login username (with OCP_PASSWORD; or use OCP_TOKEN)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
        optional: true,
    },
    VarSpec {
        name: AUTH_PASSWORD,
        description: "OpenShift login password (with OCP_USERNAME; or use OCP_TOKEN)",
        validation: ValidationKind::NonEmpty,
        secret: true,
        default: None,
        optional: true,
    },
    VarSpec {
        name: AUTH_TOKEN,
        description: "OpenShift login token (alternative to OCP_USERNAME+OCP_PASSWORD)",
        validation: ValidationKind::NonEmpty,
        secret: true,
        default: None,
        optional: true,
    },
    VarSpec {
        name: "IBM_ENTITLEMENT_KEY",
        description: "Pull secret for the IBM Entitled Registry",
        validation: ValidationKind::NonEmpty,
        secret: true,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "IMAGE_PULL_SECRET",
        description: "Name of the image pull secret used for the install",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "PROJECT_CPD_INST_OPERATORS",
        description: "Namespace for CPD operators",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "PROJECT_CPD_INST_OPERANDS",
        description: "Namespace for CPD operands (watsonx.data instance)",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "PROJECT_LICENSE_SERVICE",
        description: "Namespace for the IBM License Service",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "PROJECT_SCHEDULING_SERVICE",
        description: "Namespace for the scheduling service",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "PROJECT_SCHEDULING_BR_SVC",
        description: "Namespace for the scheduling backup/restore service",
        validation: ValidationKind::Namespace,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "STG_CLASS_BLOCK",
        description: "RWO (block) storage class for the cluster",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "STG_CLASS_FILE",
        description: "RWX (file) storage class for the cluster",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
        optional: false,
    },
    VarSpec {
        name: "VERSION",
        description: "watsonx.data / IBM Software Hub release being installed (default 5.4.0; patch 1 is the latest)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: Some(VERSION_DEFAULT),
        optional: false,
    },
    VarSpec {
        name: "PATCH_ID",
        description: "Patch level for the release (default latest)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: Some(PATCH_ID_DEFAULT),
        optional: false,
    },
    VarSpec {
        name: "COMPONENTS",
        description: "Component list passed to `cpd-cli manage apply-cr` \
                      (must include watsonx_data; e.g. base set cpd_platform,...)",
        validation: ValidationKind::NonEmpty,
        secret: false,
        default: None,
        optional: false,
    },
];

/// A variable derived (computed) at generation time from other variables. These
/// are NOT collected or prompted for; they are emitted as literal shell that
/// references the variables they depend on, so the generated `cpd_vars.sh` stays
/// correct and sourceable.
#[derive(Debug, Clone, Copy)]
pub struct DerivedVar {
    /// The shell variable name.
    pub name: &'static str,
    /// The shell RHS, emitted verbatim (intentionally NOT shell-quoted — it
    /// references other variables, e.g. `"--server=${OCP_URL}"`).
    pub value_expr: &'static str,
}

/// Derived variables, emitted in this fixed order after the collected variables
/// so output stays deterministic (AC9). Each is sourceable shell referencing
/// already-emitted variables.
pub const DERIVED: &[DerivedVar] = &[
    DerivedVar {
        name: "SERVER_ARGUMENTS",
        value_expr: "\"--server=${OCP_URL}\"",
    },
    DerivedVar {
        name: "OLM_UTILS_IMAGE",
        value_expr: "icr.io/cpopen/cpd/olm-utils-v4:${VERSION}",
    },
    DerivedVar {
        name: "PROJECT_INST_BR_SVC",
        value_expr: "${PROJECT_CPD_INST_OPERATORS}-br-svc",
    },
];

/// Look up a [`VarSpec`] by its variable name.
pub fn find(name: &str) -> Option<&'static VarSpec> {
    SPEC.iter().find(|v| v.name == name)
}

/// Whether `name` is a derived/computed variable the tool emits itself (e.g.
/// `SERVER_ARGUMENTS`). Derived names are recognized-but-ignored when seen in an
/// answers file: they are never collected as input (they are recomputed from the
/// source variables), so re-feeding a generated `cpd_vars.sh` as `--answers` must
/// not raise an "unknown variable" warning for them.
pub fn is_derived(name: &str) -> bool {
    DERIVED.iter().any(|d| d.name == name)
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
        // The exact 5.4.0 baseline contract (WORKTREE.md "Environment Variables").
        let expected = [
            "OCP_URL",
            "OPENSHIFT_TYPE",
            "IMAGE_ARCH",
            "OCP_USERNAME",
            "OCP_PASSWORD",
            "OCP_TOKEN",
            "IBM_ENTITLEMENT_KEY",
            "IMAGE_PULL_SECRET",
            "PROJECT_CPD_INST_OPERATORS",
            "PROJECT_CPD_INST_OPERANDS",
            "PROJECT_LICENSE_SERVICE",
            "PROJECT_SCHEDULING_SERVICE",
            "PROJECT_SCHEDULING_BR_SVC",
            "STG_CLASS_BLOCK",
            "STG_CLASS_FILE",
            "VERSION",
            "PATCH_ID",
            "COMPONENTS",
        ];
        let actual: Vec<&str> = SPEC.iter().map(|v| v.name).collect();
        assert_eq!(actual, expected, "SPEC drifted from documented contract");
    }

    #[test]
    fn secrets_are_exactly_the_sensitive_set() {
        let secrets: Vec<&str> = SPEC.iter().filter(|v| v.secret).map(|v| v.name).collect();
        assert_eq!(
            secrets,
            ["OCP_PASSWORD", "OCP_TOKEN", "IBM_ENTITLEMENT_KEY"]
        );
    }

    #[test]
    fn optional_vars_are_exactly_the_auth_set() {
        let optional: Vec<&str> = SPEC.iter().filter(|v| v.optional).map(|v| v.name).collect();
        assert_eq!(
            optional,
            [AUTH_USERNAME, AUTH_PASSWORD, AUTH_TOKEN],
            "only the choose-one auth vars are optional"
        );
    }

    #[test]
    fn find_round_trips() {
        assert_eq!(find("OCP_URL").unwrap().name, "OCP_URL");
        assert!(find("NOPE").is_none());
    }

    #[test]
    fn only_version_and_patch_id_have_defaults() {
        let with_default: Vec<(&str, Option<&str>)> = SPEC
            .iter()
            .filter(|v| v.default.is_some())
            .map(|v| (v.name, v.default))
            .collect();
        assert_eq!(
            with_default,
            [("VERSION", Some("5.4.0")), ("PATCH_ID", Some("latest"))],
            "exactly VERSION=5.4.0 and PATCH_ID=latest carry defaults"
        );
    }

    #[test]
    fn version_and_patch_defaults_match_constants() {
        assert_eq!(find("VERSION").unwrap().default, Some(VERSION_DEFAULT));
        assert_eq!(find("PATCH_ID").unwrap().default, Some(PATCH_ID_DEFAULT));
        assert_eq!(VERSION_DEFAULT, "5.4.0");
        assert_eq!(PATCH_ID_DEFAULT, "latest");
    }

    #[test]
    fn derived_vars_are_the_expected_three() {
        let names: Vec<&str> = DERIVED.iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            ["SERVER_ARGUMENTS", "OLM_UTILS_IMAGE", "PROJECT_INST_BR_SVC"]
        );
        // Derived names must not collide with collected variable names.
        for d in DERIVED {
            assert!(
                find(d.name).is_none(),
                "derived {} shadows a SPEC var",
                d.name
            );
        }
    }
}
