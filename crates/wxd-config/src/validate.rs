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
        // An optional variable may legitimately be empty/absent — its presence
        // is governed by cross-field rules (see [`validate_auth`]), not the
        // universal required check. Skip it here so it never errors as required.
        if spec.optional {
            return VarOutcome::default();
        }
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

/// Cross-field cluster-auth check (choose-one): the configuration is valid if
/// EITHER both `OCP_USERNAME` and `OCP_PASSWORD` are provided, OR `OCP_TOKEN` is
/// provided. If neither complete method is present, returns an actionable error.
///
/// A value is considered "provided" when present and non-empty after trimming.
/// `lookup` returns the collected value for a variable name (or `None`).
pub fn validate_auth(lookup: &dyn Fn(&str) -> Option<String>) -> Option<ValidationError> {
    let present = |name: &str| {
        lookup(name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };

    let has_userpass = present(crate::spec::AUTH_USERNAME) && present(crate::spec::AUTH_PASSWORD);
    let has_token = present(crate::spec::AUTH_TOKEN);

    if has_userpass || has_token {
        return None;
    }

    Some(ValidationError {
        var: "cluster authentication".to_string(),
        message: format!(
            "no complete cluster-auth method provided; supply both {} and {}, or {}",
            crate::spec::AUTH_USERNAME,
            crate::spec::AUTH_PASSWORD,
            crate::spec::AUTH_TOKEN
        ),
    })
}

/// Which cluster-auth variables to EMIT, given what the user provided. Returns
/// the set of auth variable names to write to `cpd_vars.sh`: only the chosen
/// method's variables, so a token-based config never emits empty username/password
/// lines (and vice-versa). If (unusually) both methods are complete, the
/// username+password pair is chosen deterministically.
pub fn auth_vars_to_emit(lookup: &dyn Fn(&str) -> Option<String>) -> Vec<&'static str> {
    let present = |name: &str| {
        lookup(name)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    let has_userpass = present(crate::spec::AUTH_USERNAME) && present(crate::spec::AUTH_PASSWORD);

    if has_userpass {
        vec![crate::spec::AUTH_USERNAME, crate::spec::AUTH_PASSWORD]
    } else if present(crate::spec::AUTH_TOKEN) {
        vec![crate::spec::AUTH_TOKEN]
    } else {
        Vec::new()
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

    /// Assert the outcome is an error whose message names the expected variable
    /// and contains the expected rule keyword (TQ1: "fail clearly" — messages
    /// must name the var AND the violated rule, not just be non-empty).
    fn assert_error(outcome: &VarOutcome, var: &str, rule_keyword: &str) {
        let e = outcome
            .error
            .as_ref()
            .unwrap_or_else(|| panic!("expected an error for {var}"));
        assert_eq!(e.var, var, "error should name the offending variable");
        let combined = format!("{e}").to_lowercase();
        assert!(
            combined.contains(&rule_keyword.to_lowercase()),
            "error for {var} should mention rule '{rule_keyword}', got: {e}"
        );
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
        assert_error(&outcome, "OCP_URL", "url");
    }

    #[test]
    fn ftp_scheme_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "ftp://api.example.com");
        assert_error(&outcome, "OCP_URL", "scheme");
    }

    #[test]
    fn url_without_host_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "https://");
        assert_error(&outcome, "OCP_URL", "host");
    }

    #[test]
    fn url_with_whitespace_errors() {
        let outcome = validate_value(spec_for("OCP_URL"), "https://api .example.com");
        assert_error(&outcome, "OCP_URL", "whitespace");
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
        assert_error(&outcome, "PROJECT_CPD_INST_OPERATORS", "namespace");
        // The specific invalid character should be surfaced.
        assert!(outcome.error.unwrap().message.contains('C'));
    }

    #[test]
    fn leading_dash_namespace_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), "-cpd");
        assert_error(&outcome, "PROJECT_CPD_INST_OPERANDS", "-");
    }

    #[test]
    fn trailing_dash_namespace_errors() {
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), "cpd-");
        assert_error(&outcome, "PROJECT_CPD_INST_OPERANDS", "-");
    }

    #[test]
    fn too_long_namespace_errors() {
        let long = "a".repeat(64);
        let outcome = validate_value(spec_for("PROJECT_CPD_INST_OPERANDS"), &long);
        // The rule mentions the 63-char limit and the actual length.
        assert_error(&outcome, "PROJECT_CPD_INST_OPERANDS", "63");
        assert!(outcome.error.unwrap().message.contains("64"));
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
        assert_error(&outcome, "PROJECT_CPD_INST_OPERANDS", "namespace");
        assert!(outcome.error.unwrap().message.contains('_'));
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
        // Message lists the accepted set and flags the whitespace.
        assert_error(&outcome, "OPENSHIFT_TYPE", "whitespace");
        assert!(outcome.error.unwrap().message.contains("self-managed"));
    }

    #[test]
    fn empty_required_error_names_var_and_rule() {
        // TQ1: required-ness message must name the var and say "required".
        let outcome = validate_value(spec_for("IBM_ENTITLEMENT_KEY"), "");
        assert_error(&outcome, "IBM_ENTITLEMENT_KEY", "required");
    }

    // ---- optional vars (auth set) skip the required check ----

    #[test]
    fn empty_optional_auth_var_does_not_error() {
        let outcome = validate_value(spec_for("OCP_TOKEN"), "");
        assert!(outcome.error.is_none(), "optional var must not error when empty");
        let outcome = validate_value(spec_for("OCP_USERNAME"), "");
        assert!(outcome.error.is_none());
    }

    // ---- cross-field auth (choose-one) ----

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn auth_userpass_is_valid() {
        let l = lookup_from(&[("OCP_USERNAME", "admin"), ("OCP_PASSWORD", "pw")]);
        assert!(validate_auth(&l).is_none());
    }

    #[test]
    fn auth_token_is_valid() {
        let l = lookup_from(&[("OCP_TOKEN", "sha256~x")]);
        assert!(validate_auth(&l).is_none());
    }

    #[test]
    fn auth_none_errors_actionably() {
        let l = lookup_from(&[]);
        let e = validate_auth(&l).expect("expected an auth error");
        assert!(e.message.contains("OCP_USERNAME"));
        assert!(e.message.contains("OCP_PASSWORD"));
        assert!(e.message.contains("OCP_TOKEN"));
    }

    #[test]
    fn auth_username_only_is_incomplete() {
        let l = lookup_from(&[("OCP_USERNAME", "admin")]);
        assert!(validate_auth(&l).is_some());
    }

    #[test]
    fn auth_empty_values_count_as_absent() {
        let l = lookup_from(&[("OCP_USERNAME", ""), ("OCP_PASSWORD", ""), ("OCP_TOKEN", "  ")]);
        assert!(validate_auth(&l).is_some());
    }

    #[test]
    fn emit_set_picks_userpass() {
        let l = lookup_from(&[("OCP_USERNAME", "a"), ("OCP_PASSWORD", "b")]);
        assert_eq!(auth_vars_to_emit(&l), vec!["OCP_USERNAME", "OCP_PASSWORD"]);
    }

    #[test]
    fn emit_set_picks_token() {
        let l = lookup_from(&[("OCP_TOKEN", "t")]);
        assert_eq!(auth_vars_to_emit(&l), vec!["OCP_TOKEN"]);
    }

    #[test]
    fn emit_set_prefers_userpass_when_both_present() {
        let l = lookup_from(&[
            ("OCP_USERNAME", "a"),
            ("OCP_PASSWORD", "b"),
            ("OCP_TOKEN", "t"),
        ]);
        assert_eq!(auth_vars_to_emit(&l), vec!["OCP_USERNAME", "OCP_PASSWORD"]);
    }
}
