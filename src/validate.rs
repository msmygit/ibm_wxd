//! Input validation for the `cpd_vars.sh` contract.
//!
//! All checks here are pure and hermetic: no cluster, no `oc`, no `cpd-cli`.
//! They cover required-ness (AC3), URL well-formedness (AC4), Kubernetes
//! namespace rules (AC5), and allowed-value sets (AC6).
//!
//! The validation policy for [`ValidationKind::Enum`] follows
//! questions_answers.md Q2: documented values pass silently; an unknown but
//! plausibly-formatted value is *warned* about and allowed, never hard-failed.

use crate::spec::{ValidationKind, VarSpec};

/// A validation failure for a single variable. Carries the variable name and an
/// actionable message naming the rule that was violated (AC3–AC6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub var: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.var, self.message)
    }
}

/// A non-fatal advisory (e.g. an unrecognised but plausible enum value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationWarning {
    pub var: String,
    pub message: String,
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.var, self.message)
    }
}

/// Outcome of validating one variable's value.
#[derive(Debug, Default)]
pub struct VarOutcome {
    pub error: Option<ValidationError>,
    pub warning: Option<ValidationWarning>,
}

/// Validate a single value against its [`VarSpec`].
///
/// Every variable is first checked for required-ness (non-empty after trimming).
/// An empty value yields an error and short-circuits — the type-specific check
/// does not run, so the message is always the clearest single cause.
pub fn validate_value(spec: &VarSpec, value: &str) -> VarOutcome {
    if value.trim().is_empty() {
        return VarOutcome {
            error: Some(ValidationError {
                var: spec.name.to_string(),
                message: "is required but was not provided (value is empty)".to_string(),
            }),
            warning: None,
        };
    }

    match spec.validation {
        ValidationKind::NonEmpty => VarOutcome::default(),
        ValidationKind::Url => check_url(spec.name, value),
        ValidationKind::Namespace => check_namespace(spec.name, value),
        ValidationKind::Enum(allowed) => check_enum(spec.name, value, allowed),
    }
}

fn err(var: &str, message: String) -> VarOutcome {
    VarOutcome {
        error: Some(ValidationError {
            var: var.to_string(),
            message,
        }),
        warning: None,
    }
}

/// AC4: value must be a well-formed `http://` or `https://` URL with a host.
fn check_url(var: &str, value: &str) -> VarOutcome {
    let scheme_split = value.split_once("://");
    let (scheme, rest) = match scheme_split {
        Some(parts) => parts,
        None => {
            return err(
                var,
                format!(
                    "must be a well-formed URL with an https:// (or http://) scheme, got '{value}'"
                ),
            )
        }
    };

    if scheme != "https" && scheme != "http" {
        return err(
            var,
            format!("must use the https:// or http:// scheme, got '{scheme}://'"),
        );
    }

    // Host is everything before the first '/', '?' or '#'. It must be present
    // and free of whitespace.
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        // strip optional userinfo@ and :port for the emptiness check
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host_only = host.split(':').next().unwrap_or("");

    if host_only.is_empty() {
        return err(
            var,
            format!("must include a host after the scheme, got '{value}'"),
        );
    }
    if value.chars().any(char::is_whitespace) {
        return err(
            var,
            format!("must not contain whitespace, got '{value}'"),
        );
    }

    VarOutcome::default()
}

/// AC5: Kubernetes / RFC 1123 namespace rules — lowercase alphanumeric and `-`,
/// length 1..=63, must start and end with an alphanumeric character.
fn check_namespace(var: &str, value: &str) -> VarOutcome {
    const RULE: &str = "must be a valid Kubernetes namespace (lowercase alphanumeric and '-', \
                         1-63 chars, starting and ending with an alphanumeric character)";

    if value.len() > 63 {
        return err(
            var,
            format!("{RULE}; got {} characters", value.len()),
        );
    }

    let valid_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    if let Some(bad) = value.chars().find(|&c| !valid_char(c)) {
        return err(var, format!("{RULE}; invalid character '{bad}'"));
    }

    let is_alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let first = value.chars().next().unwrap();
    let last = value.chars().last().unwrap();
    if !is_alnum(first) || !is_alnum(last) {
        return err(
            var,
            format!("{RULE}; must not start or end with '-'"),
        );
    }

    VarOutcome::default()
}

/// AC6 / Q2: known values pass; unknown-but-plausible values warn and are
/// allowed. A value with shell-hostile shape (whitespace) is still an error.
fn check_enum(var: &str, value: &str, allowed: &[&str]) -> VarOutcome {
    if allowed.contains(&value) {
        return VarOutcome::default();
    }

    if value.chars().any(char::is_whitespace) {
        return err(
            var,
            format!(
                "must be one of [{}], got '{value}' (contains whitespace)",
                allowed.join(", ")
            ),
        );
    }

    VarOutcome {
        error: None,
        warning: Some(ValidationWarning {
            var: var.to_string(),
            message: format!(
                "'{value}' is not a documented value [{}]; accepting it but verify against your cpd-cli version",
                allowed.join(", ")
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec;

    fn spec_for(name: &str) -> &'static VarSpec {
        spec::find(name).unwrap()
    }

    // ---- required-ness (AC3) ----

    #[test]
    fn empty_required_value_errors_and_names_var() {
        let outcome = validate_value(spec_for("IBM_ENTITLEMENT_KEY"), "");
        let e = outcome.error.expect("expected error");
        assert_eq!(e.var, "IBM_ENTITLEMENT_KEY");
        assert!(e.message.contains("required"));
    }

    #[test]
    fn whitespace_only_value_is_treated_as_empty() {
        let outcome = validate_value(spec_for("VERSION"), "   ");
        assert!(outcome.error.is_some());
    }

    // ---- URL (AC4) ----

    #[test]
    fn valid_https_url_passes() {
        let outcome = validate_value(spec_for("OCP_URL"), "https://api.cluster.example.com:6443");
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
    }

    #[test]
    fn http_url_passes() {
        let outcome = validate_value(spec_for("OCP_URL"), "http://api.example.com");
        assert!(outcome.error.is_none());
    }

    #[test]
    fn non_url_value_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "not-a-url");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn ftp_scheme_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "ftp://api.example.com");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn url_without_host_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "https://");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn url_with_whitespace_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "https://api .example.com");
        assert!(outcome.error.is_some());
    }

    // ---- namespace (AC5) ----

    #[test]
    fn valid_namespace_passes() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERATORS"), "cpd-operators");
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
    }

    #[test]
    fn uppercase_namespace_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERATORS"), "CPD-Operators");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn leading_dash_namespace_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), "-cpd");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn trailing_dash_namespace_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), "cpd-");
        assert!(outcome.error.is_some());
    }

    #[test]
    fn too_long_namespace_errors() {
        let long = "a".repeat(64);
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), &long);
        assert!(outcome.error.is_some());
    }

    #[test]
    fn max_length_namespace_passes() {
        let max = "a".repeat(63);
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), &max);
        assert!(outcome.error.is_none());
    }

    #[test]
    fn namespace_with_underscore_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), "cpd_operands");
        assert!(outcome.error.is_some());
    }

    // ---- enum (AC6 / Q2) ----

    #[test]
    fn known_openshift_type_passes_without_warning() {
        let outcome = validate_value(spec_for("OPENSHIFT_TYPE"), "self-managed");
        assert!(outcome.error.is_none());
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn known_image_arch_passes() {
        let outcome = validate_value(spec_for("IMAGE_ARCH"), "amd64");
        assert!(outcome.error.is_none());
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn unknown_enum_warns_but_allowed() {
        let outcome = validate_value(spec_for("IMAGE_ARCH"), "ppc64le");
        assert!(outcome.error.is_none(), "unknown enum must not hard-fail");
        let w = outcome.warning.expect("expected a warning");
        assert_eq!(w.var, "IMAGE_ARCH");
        assert!(w.message.contains("ppc64le"));
    }

    #[test]
    fn enum_with_whitespace_errors() {
        let outcome = validate_value(spec_for("OPENSHIFT_TYPE"), "self managed");
        assert!(outcome.error.is_some());
    }
}
