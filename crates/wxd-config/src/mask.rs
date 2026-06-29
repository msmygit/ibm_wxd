//! Secret-handling hygiene for console/log output (AC10).
//!
//! The generated `cpd_vars.sh` must carry real secret values to be usable, but
//! nothing printed to stdout/stderr (prompts, progress, summary) may ever echo a
//! secret in plaintext. This module produces the masked representation used in
//! all console output.

/// The fixed mask shown in place of any secret value in console/log output.
/// Constant-width on purpose so it never leaks the secret's length.
pub const MASK: &str = "********";

/// Return the value to display for a variable in console output: the real value
/// for non-secrets, the fixed [`MASK`] for secrets.
///
/// A secret whose value is empty is shown as empty (so an "is required" error
/// still reads clearly) rather than as a mask over nothing.
pub fn display_value(is_secret: bool, value: &str) -> String {
    if is_secret && !value.is_empty() {
        MASK.to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_secret_shown_verbatim() {
        assert_eq!(display_value(false, "amd64"), "amd64");
    }

    #[test]
    fn secret_is_masked() {
        assert_eq!(display_value(true, "super-secret-key"), MASK);
    }

    #[test]
    fn mask_does_not_leak_length() {
        let short = display_value(true, "a");
        let long = display_value(true, "a".repeat(200).as_str());
        assert_eq!(short, long);
    }

    #[test]
    fn empty_secret_shown_empty() {
        assert_eq!(display_value(true, ""), "");
    }
}
