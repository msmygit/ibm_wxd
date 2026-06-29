//! Command-line interface: argument parsing and `--help`/usage text (AC11).
//!
//! Hand-rolled parsing (no external crates) keeps the build hermetic and the
//! binary tiny. The accepted flags are intentionally small:
//!
//!   --non-interactive        Never prompt; take values from --answers + env.
//!   --answers <FILE>         Read KEY=VALUE answers from FILE.
//!   --output  <FILE>         Where to write cpd_vars.sh (default: ./cpd_vars.sh).
//!   --help, -h               Print usage and exit 0.
//!   --version, -V            Print version and exit 0.

use crate::spec::{ValidationKind, DERIVED, SPEC};

/// Parsed CLI options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub non_interactive: bool,
    pub answers_file: Option<String>,
    pub output_file: String,
    pub show_help: bool,
    pub show_version: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            non_interactive: false,
            answers_file: None,
            output_file: "cpd_vars.sh".to_string(),
            show_help: false,
            show_version: false,
        }
    }
}

/// Parse argv (excluding the program name). Returns an error string for unknown
/// flags or flags missing their required value.
pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => opts.show_help = true,
            "--version" | "-V" => opts.show_version = true,
            "--non-interactive" => opts.non_interactive = true,
            "--answers" => {
                opts.answers_file = Some(
                    it.next()
                        .ok_or("--answers requires a FILE argument")?
                        .clone(),
                );
            }
            "--output" => {
                opts.output_file = it
                    .next()
                    .ok_or("--output requires a FILE argument")?
                    .clone();
            }
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }
    Ok(opts)
}

/// The program name shown in usage.
pub const PROG: &str = "wxd-config";

/// Build the `--help`/usage text. Lists every required input (AC11) and both
/// interactive and non-interactive modes. Generated from [`SPEC`] so it can
/// never drift from the actual contract.
pub fn help_text() -> String {
    let mut s = String::new();
    s.push_str(PROG);
    s.push_str(" — collect watsonx.data install configuration and generate cpd_vars.sh\n\n");

    s.push_str("USAGE:\n");
    s.push_str(&format!("    {PROG} [OPTIONS]\n\n"));

    s.push_str("MODES:\n");
    s.push_str("    Interactive (default): prompts for any required value not already\n");
    s.push_str("        supplied via --answers or the environment. Secret values are read\n");
    s.push_str("        without echoing and are never printed back.\n");
    s.push_str("    Non-interactive (--non-interactive): never prompts. Every required\n");
    s.push_str("        value must come from --answers and/or environment variables, or the\n");
    s.push_str("        run fails listing what is missing.\n\n");

    s.push_str("OPTIONS:\n");
    s.push_str("    --non-interactive    Do not prompt; use --answers + environment only.\n");
    s.push_str("    --answers <FILE>     Read KEY=VALUE answers from FILE (# comments ok).\n");
    s.push_str("    --output <FILE>      Output path for cpd_vars.sh (default: ./cpd_vars.sh).\n");
    s.push_str("    -h, --help           Print this help and exit.\n");
    s.push_str("    -V, --version        Print version and exit.\n\n");

    s.push_str("INPUTS (each may be given via --answers, an env var, or a prompt):\n");
    for spec in SPEC {
        let kind = match spec.validation {
            ValidationKind::NonEmpty => "non-empty".to_string(),
            ValidationKind::Url => "https URL".to_string(),
            ValidationKind::Namespace => "k8s namespace".to_string(),
            ValidationKind::Enum(vals) => format!("one of [{}]", vals.join(", ")),
        };
        let mut tags = String::new();
        if spec.secret {
            tags.push_str(" (secret)");
        }
        if spec.optional {
            tags.push_str(" (auth, choose-one)");
        }
        if let Some(d) = spec.default {
            tags.push_str(&format!(" (default {d})"));
        }
        s.push_str(&format!(
            "    {:<28} {} [{}]{}\n",
            spec.name, spec.description, kind, tags
        ));
    }
    s.push('\n');

    s.push_str("CLUSTER AUTH (choose one):\n");
    s.push_str("    Provide BOTH OCP_USERNAME and OCP_PASSWORD, OR provide OCP_TOKEN.\n");
    s.push_str("    Only the chosen method's variables are written to the output file.\n\n");

    s.push_str("DERIVED (computed automatically, not prompted):\n");
    for d in DERIVED {
        s.push_str(&format!("    {:<28} = {}\n", d.name, d.value_expr));
    }
    s.push('\n');

    s.push_str("OUTPUT:\n");
    s.push_str("    A deterministic, source-able cpd_vars.sh for IBM Software Hub 5.4.x.\n");
    s.push_str("    Never commit it — it carries credentials and the IBM entitlement key.\n");

    s
}

/// Version string for `--version`.
pub fn version_text() -> String {
    format!("{PROG} {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_are_interactive_and_default_output() {
        let opts = parse(&[]).unwrap();
        assert!(!opts.non_interactive);
        assert_eq!(opts.output_file, "cpd_vars.sh");
        assert!(opts.answers_file.is_none());
    }

    #[test]
    fn parses_all_flags() {
        let opts = parse(&argv(&[
            "--non-interactive",
            "--answers",
            "a.txt",
            "--output",
            "out.sh",
        ]))
        .unwrap();
        assert!(opts.non_interactive);
        assert_eq!(opts.answers_file.as_deref(), Some("a.txt"));
        assert_eq!(opts.output_file, "out.sh");
    }

    #[test]
    fn help_flag_recognised() {
        assert!(parse(&argv(&["--help"])).unwrap().show_help);
        assert!(parse(&argv(&["-h"])).unwrap().show_help);
    }

    #[test]
    fn version_flag_recognised() {
        assert!(parse(&argv(&["-V"])).unwrap().show_version);
    }

    #[test]
    fn unknown_flag_errors() {
        assert!(parse(&argv(&["--nope"])).is_err());
    }

    #[test]
    fn answers_without_value_errors() {
        assert!(parse(&argv(&["--answers"])).is_err());
    }

    #[test]
    fn output_without_value_errors() {
        assert!(parse(&argv(&["--output"])).is_err());
    }

    // ---- help text (AC11) ----

    #[test]
    fn help_lists_every_required_var() {
        let help = help_text();
        for spec in SPEC {
            assert!(help.contains(spec.name), "help missing {}", spec.name);
        }
    }

    #[test]
    fn help_documents_auth_choose_one_and_derived() {
        let help = help_text();
        assert!(help.contains("OCP_TOKEN"));
        assert!(help.to_lowercase().contains("choose one"));
        for d in DERIVED {
            assert!(help.contains(d.name), "help missing derived {}", d.name);
        }
        assert!(help.contains("5.4"), "help should reference the 5.4.x target");
    }

    #[test]
    fn help_mentions_both_modes() {
        let help = help_text();
        assert!(help.to_lowercase().contains("interactive"));
        assert!(help.contains("--non-interactive"));
    }

    #[test]
    fn help_does_not_reference_cluster_tools() {
        // AC11: help contacts no cluster; it also shouldn't tell the user to.
        let help = help_text();
        assert!(!help.contains("oc login"));
    }
}
