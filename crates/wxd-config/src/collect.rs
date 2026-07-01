//! Configuration collection — interactive and non-interactive (AC8).
//!
//! Sources, in increasing precedence:
//!   1. An optional answers file (`KEY=VALUE` lines, `#` comments). Values are
//!      shell-unquoted as the exact inverse of `generate::shell_quote`, so a
//!      previously-generated `cpd_vars.sh` can be fed back in and reproduced
//!      byte-for-byte (no credential corruption).
//!   2. Process environment variables.
//!   3. Interactive prompts (only for variables still missing, only when a TTY /
//!      interactive mode is allowed).
//!
//! Non-interactive mode (`--non-interactive`) never reads stdin: if a required
//! variable is still missing after the file + env sources, collection returns
//! the missing set so the caller can fail loudly (AC3) without prompting.
//!
//! The answers-file parser and the env/file merge are pure and unit-tested. The
//! interactive prompt path is isolated behind the [`Prompter`] trait so the core
//! collection logic stays hermetic and testable with a fake prompter (no real
//! stdin in tests — AC12).

use crate::generate::Config;
use crate::spec::SPEC;
use std::collections::BTreeMap;

/// Abstraction over "ask the user for one variable's value", so the collection
/// logic can be unit-tested with a fake instead of real stdin.
pub trait Prompter {
    /// Prompt for `name` (described by `description`); `secret` requests
    /// no-echo input. Returns the entered value (may be empty; validation is the
    /// caller's job).
    fn prompt(&mut self, name: &str, description: &str, secret: bool) -> std::io::Result<String>;
}

/// Where collected values may come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// May prompt on stdin for anything still missing.
    Interactive,
    /// Never prompts; missing values are reported, not asked for.
    NonInteractive,
}

/// The result of parsing an answers file: the resolved key/value map plus any
/// non-fatal advisories (e.g. an unrecognised key not in [`SPEC`]).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedAnswers {
    pub values: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

/// Parse an answers file body. Each non-blank, non-comment line must be
/// `KEY=VALUE`. Surrounding whitespace around KEY is trimmed; the VALUE is taken
/// after the first `=` (so values may contain `=`) and then **shell-unquoted**
/// (see [`unquote_shell_value`]) so that the parser is the exact inverse of
/// [`crate::generate::shell_quote`].
///
/// This guarantees the round-trip invariant the tool advertises:
/// `parse_answers(render(config)).values == config` for every value, including
/// ones containing single quotes — re-using a generated `cpd_vars.sh` as an
/// `--answers` file never corrupts a credential.
///
/// Lines beginning with `export ` are tolerated (the leading keyword is dropped)
/// so a prior `cpd_vars.sh` can be re-used directly.
///
/// Each key is classified three ways:
///   1. a [`SPEC`] input variable → kept and used as input;
///   2. a derived variable name (see [`crate::spec::is_derived`]) → silently
///      ignored (the tool recomputes it; re-feeding a generated `cpd_vars.sh`
///      must not warn) and NOT collected;
///   3. anything else → kept but reported as a warning, so a typo'd variable
///      name (e.g. `OCP_URI=`) surfaces as "unknown variable 'OCP_URI'" rather
///      than a misdirecting "OCP_URL is required" later (fail clearly, F3).
pub fn parse_answers(body: &str) -> Result<ParsedAnswers, String> {
    let mut out = ParsedAnswers::default();
    for (lineno, raw) in body.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected KEY=VALUE, got '{raw}'", lineno + 1))?;
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err(format!("line {}: empty key in '{raw}'", lineno + 1));
        }
        // A derived var the tool emits itself: recognized, recomputed, never
        // collected — skip it without parsing its value or warning.
        if crate::spec::is_derived(&key) {
            continue;
        }
        let value = unquote_shell_value(value)
            .map_err(|e| format!("line {}: {e} in '{raw}'", lineno + 1))?;
        if crate::spec::find(&key).is_none() {
            out.warnings
                .push(format!("ignoring unknown variable '{key}' in answers file"));
        }
        out.values.insert(key, value);
    }
    Ok(out)
}

/// Decode a shell value back to its literal string, inverting the quoting that
/// [`crate::generate::shell_quote`] produces and tolerating common hand-written
/// forms. Supports a sequence of adjacent tokens, each either:
///   - a single-quoted segment `'...'` (literal; the only special form inside is
///     the POSIX escape `'\''` for an embedded single quote),
///   - a double-quoted segment `"..."` (taken literally here — the generator
///     never emits these, and answers files are config, not scripts),
///   - an unquoted run of characters (taken verbatim).
///
/// Examples (round-tripping `shell_quote` output):
///   `'abc'`         -> `abc`
///   `'p@ss w$rd'`   -> `p@ss w$rd`
///   `'it'\''s'`     -> `it's`
///
/// Returns an error only for a malformed value (an unterminated quote), so a
/// corrupt answers file fails loudly instead of silently mangling a credential.
fn unquote_shell_value(value: &str) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '\'' => {
                chars.next(); // consume opening '
                loop {
                    match chars.next() {
                        Some('\'') => break, // closing quote ends this segment
                        Some(ch) => out.push(ch),
                        None => return Err("unterminated single quote".to_string()),
                    }
                }
                // A `'\''` escape appears as: closing ' , then \' (an unquoted
                // backslash-quote run), then a reopening ' . The unquoted branch
                // below decodes the `\'` to a literal '. No special-casing needed.
            }
            '"' => {
                chars.next(); // consume opening "
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            // Within double quotes, \" and \\ are the escapes we honor.
                            match chars.next() {
                                Some(escaped @ ('"' | '\\' | '$' | '`')) => out.push(escaped),
                                Some(other) => {
                                    out.push('\\');
                                    out.push(other);
                                }
                                None => return Err("unterminated double quote".to_string()),
                            }
                        }
                        Some(ch) => out.push(ch),
                        None => return Err("unterminated double quote".to_string()),
                    }
                }
            }
            '\\' => {
                chars.next(); // consume backslash
                match chars.next() {
                    Some(ch) => out.push(ch), // \x -> x (covers the \' in '\'')
                    None => out.push('\\'),   // trailing backslash, keep literal
                }
            }
            _ => {
                out.push(c);
                chars.next();
            }
        }
    }
    Ok(out)
}

/// Resolve every variable's value from file + env, then (only in
/// [`Mode::Interactive`]) prompt for whatever is still missing, and finally
/// apply any [`VarSpec::default`] for a value still absent or left empty.
///
/// Precedence (highest first): environment variable, answers file, interactive
/// input, spec default. The default is applied to a still-missing value (and to
/// an interactive value the user left blank), so a variable that carries a
/// default — only `VERSION` today — never errors as "required". Variables with
/// no default remain strictly required: if absent here, the validator flags them
/// (AC3). Required-ness and value validity are NOT enforced in this function —
/// that is the validator's job, run by the caller — so a single, consistent
/// error path reports all problems.
///
/// `env_lookup` is injected (rather than calling `std::env` directly) so tests
/// can supply a fake environment with no global state.
pub fn collect(
    mode: Mode,
    answers: &BTreeMap<String, String>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
    prompter: &mut dyn Prompter,
) -> std::io::Result<Config> {
    let mut config = Config::new();

    for spec in SPEC {
        // Precedence: env var overrides answers file (env is the explicit,
        // per-run override; the file is the saved baseline).
        let from_env = env_lookup(spec.name);
        let from_file = answers.get(spec.name).cloned();
        let mut resolved = from_env.or(from_file);

        // Interactive: prompt for anything still missing. The prompt advertises
        // the default (if any) so the user can press Enter to accept it.
        if resolved.is_none() && mode == Mode::Interactive {
            let label = match spec.default {
                Some(d) => format!("{} [default: {d}]", spec.description),
                None => spec.description.to_string(),
            };
            resolved = Some(prompter.prompt(spec.name, &label, spec.secret)?);
        }

        // Apply the spec default when the value is absent, or present-but-empty
        // (e.g. the user pressed Enter at an interactive prompt that offered one).
        let value = match (resolved, spec.default) {
            (Some(v), Some(default)) if v.trim().is_empty() => default.to_string(),
            (Some(v), _) => v,
            (None, Some(default)) => default.to_string(),
            (None, None) => continue, // leave out; validator flags it missing.
        };
        config.insert(spec.name.to_string(), value);
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted prompter that returns canned answers and records what it was
    /// asked. Lets us assert the interactive path without touching stdin.
    struct FakePrompter {
        answers: BTreeMap<String, String>,
        asked: Vec<String>,
    }

    impl Prompter for FakePrompter {
        fn prompt(
            &mut self,
            name: &str,
            _description: &str,
            _secret: bool,
        ) -> std::io::Result<String> {
            self.asked.push(name.to_string());
            Ok(self.answers.get(name).cloned().unwrap_or_default())
        }
    }

    /// A prompter that fails if ever called — proves non-interactive mode never
    /// reads input (AC8).
    struct NeverPrompter;
    impl Prompter for NeverPrompter {
        fn prompt(&mut self, name: &str, _d: &str, _s: bool) -> std::io::Result<String> {
            panic!("prompt() must not be called in non-interactive mode (asked for {name})");
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    // ---- parse_answers ----

    #[test]
    fn parses_basic_pairs() {
        let body = "OCP_URL=https://x\nVERSION=5.4.0\n";
        let m = parse_answers(body).unwrap().values;
        assert_eq!(m["OCP_URL"], "https://x");
        assert_eq!(m["VERSION"], "5.4.0");
    }

    #[test]
    fn skips_comments_and_blanks() {
        let body = "# a comment\n\nVERSION=5.4.0\n   # indented comment\n";
        let m = parse_answers(body).unwrap().values;
        assert_eq!(m.len(), 1);
        assert_eq!(m["VERSION"], "5.4.0");
    }

    #[test]
    fn tolerates_export_prefix() {
        let m = parse_answers("export VERSION=5.4.0\n").unwrap().values;
        assert_eq!(m["VERSION"], "5.4.0");
    }

    #[test]
    fn value_may_contain_equals() {
        let m = parse_answers("COMPONENTS=a=1,b=2\n").unwrap().values;
        assert_eq!(m["COMPONENTS"], "a=1,b=2");
    }

    #[test]
    fn unquotes_single_and_double_quoted_values() {
        let m = parse_answers("OCP_PASSWORD='p@ss w$rd'\n").unwrap().values;
        assert_eq!(m["OCP_PASSWORD"], "p@ss w$rd");
        let m2 = parse_answers("VERSION=\"5.4.0\"\n").unwrap().values;
        assert_eq!(m2["VERSION"], "5.4.0");
    }

    #[test]
    fn unquotes_embedded_single_quote_escape() {
        // The exact form shell_quote emits for `it's`.
        let m = parse_answers("OCP_PASSWORD='it'\\''s'\n").unwrap().values;
        assert_eq!(m["OCP_PASSWORD"], "it's");
    }

    #[test]
    fn round_trips_generate_then_parse_for_single_quote_value() {
        // G3 regression: feeding a generated cpd_vars.sh back as --answers must
        // reproduce every COLLECTED value exactly, including ones with single
        // quotes. The generated file also carries DERIVED vars (SERVER_ARGUMENTS
        // etc.); those reparse as their literal expr text and are not part of the
        // collected config — assert the collected values match and the only
        // extra keys are the known derived ones.
        use crate::generate::{render, Config};
        use crate::spec::DERIVED;
        let tricky = [
            ("OCP_PASSWORD", "it's"),
            ("IBM_ENTITLEMENT_KEY", "pa'ss\"w$rd `x` end"),
            ("COMPONENTS", "a,b,c"),
        ];
        let mut config = Config::new();
        for spec in SPEC {
            config.insert(spec.name.to_string(), format!("v-{}", spec.name));
        }
        for (k, v) in tricky {
            config.insert(k.to_string(), v.to_string());
        }
        let rendered = render(&config);
        let reparsed = parse_answers(&rendered).unwrap();

        // Derived vars are recognized-but-ignored: they must NOT appear as
        // collected values and must NOT raise warnings.
        for d in DERIVED {
            assert!(
                !reparsed.values.contains_key(d.name),
                "derived {} must not be collected",
                d.name
            );
        }
        assert!(
            reparsed.warnings.is_empty(),
            "no warnings: {:?}",
            reparsed.warnings
        );

        // The reparsed collected set equals the original config exactly,
        // including single-quote-bearing values.
        assert_eq!(
            reparsed.values, config,
            "collected vars must round-trip exactly"
        );
    }

    #[test]
    fn duplicate_key_last_wins() {
        // A KEY appearing twice in an answers file resolves to the LAST value
        // (the parser inserts into a map line-by-line). This is the documented,
        // useful behavior for overriding an earlier baseline line further down
        // the same file. Asserts the real last-wins contract.
        let parsed = parse_answers("VERSION=5.4.0\nVERSION=5.4.1\n").unwrap();
        assert_eq!(parsed.values["VERSION"], "5.4.1");
        // The duplicate is a known SPEC key, so no unknown-key warning fires.
        assert!(
            parsed.warnings.is_empty(),
            "warnings: {:?}",
            parsed.warnings
        );
    }

    #[test]
    fn derived_keys_are_ignored_without_warning() {
        // Derived var lines (as a generated file carries them) are recognized,
        // not collected, and never warned about.
        let body = "SERVER_ARGUMENTS=\"--server=${OCP_URL}\"\n\
                    OLM_UTILS_IMAGE=icr.io/cpopen/cpd/olm-utils-v4:${VERSION}\n\
                    PROJECT_INST_BR_SVC=${PROJECT_CPD_INST_OPERATORS}-br-svc\n";
        let parsed = parse_answers(body).unwrap();
        assert!(
            parsed.values.is_empty(),
            "derived vars must not be collected"
        );
        assert!(parsed.warnings.is_empty(), "derived vars must not warn");
    }

    #[test]
    fn unknown_key_warns_but_is_kept() {
        let parsed = parse_answers("OCP_URI=https://typo\n").unwrap();
        assert_eq!(parsed.values["OCP_URI"], "https://typo");
        assert_eq!(parsed.warnings.len(), 1);
        assert!(parsed.warnings[0].contains("OCP_URI"));
        assert!(parsed.warnings[0].contains("unknown"));
    }

    #[test]
    fn known_keys_produce_no_warnings() {
        let parsed = parse_answers("OCP_URL=https://x\nVERSION=5.4.0\n").unwrap();
        assert!(parsed.warnings.is_empty());
    }

    #[test]
    fn unterminated_quote_errors_loudly() {
        let err = parse_answers("OCP_PASSWORD='unterminated\n").unwrap_err();
        assert!(err.contains("unterminated"));
        assert!(err.contains("line 1"));
    }

    #[test]
    fn malformed_line_errors() {
        let err = parse_answers("VERSION=5.4.0\nnonsense\n").unwrap_err();
        assert!(err.contains("line 2"));
    }

    #[test]
    fn empty_key_errors() {
        assert!(parse_answers("=value\n").is_err());
    }

    // ---- collect: env precedence ----

    #[test]
    fn env_overrides_file() {
        let mut answers = BTreeMap::new();
        answers.insert("VERSION".to_string(), "from-file".to_string());
        let env = |k: &str| {
            if k == "VERSION" {
                Some("from-env".to_string())
            } else {
                None
            }
        };
        let config = collect(Mode::NonInteractive, &answers, &env, &mut NeverPrompter).unwrap();
        assert_eq!(config["VERSION"], "from-env");
    }

    #[test]
    fn file_used_when_env_absent() {
        let mut answers = BTreeMap::new();
        answers.insert("VERSION".to_string(), "from-file".to_string());
        let config = collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        assert_eq!(config["VERSION"], "from-file");
    }

    // ---- collect: non-interactive never prompts (AC8) ----

    #[test]
    fn non_interactive_never_prompts() {
        let answers = BTreeMap::new();
        // NeverPrompter panics if called; reaching the end proves no prompt.
        let config = collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        // Nothing supplied, nothing prompted. The only entries present are the
        // two defaulted vars (VERSION, PATCH_ID); every other (defaultless) var
        // is absent so the validator will flag it missing.
        let mut keys: Vec<&String> = config.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["PATCH_ID", "VERSION"]);
        assert_eq!(config["VERSION"], "5.4.0");
        assert_eq!(config["PATCH_ID"], "latest");
    }

    // ---- collect: VERSION default ----

    #[test]
    fn version_defaults_when_absent() {
        let answers = BTreeMap::new();
        let config = collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        assert_eq!(config["VERSION"], "5.4.0");
    }

    #[test]
    fn supplied_version_overrides_default() {
        // A user-supplied value (distinct from the 5.4.0 default) must win.
        let mut answers = BTreeMap::new();
        answers.insert("VERSION".to_string(), "5.3.x".to_string());
        let config = collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        assert_eq!(config["VERSION"], "5.3.x");
    }

    #[test]
    fn defaultless_var_stays_absent_when_missing() {
        // A var with no default (e.g. OCP_URL) must NOT be filled — it has to
        // reach the validator as missing so it errors as required.
        let answers = BTreeMap::new();
        let config = collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        assert!(!config.contains_key("OCP_URL"));
        assert!(!config.contains_key("IBM_ENTITLEMENT_KEY"));
    }

    // ---- collect: interactive prompts only for missing ----

    #[test]
    fn interactive_prompts_only_for_missing() {
        let mut answers = BTreeMap::new();
        // Supply everything except VERSION via the file.
        for spec in SPEC {
            if spec.name != "VERSION" {
                answers.insert(spec.name.to_string(), "x".to_string());
            }
        }
        let mut prompter = FakePrompter {
            answers: {
                let mut a = BTreeMap::new();
                a.insert("VERSION".to_string(), "5.4.0".to_string());
                a
            },
            asked: Vec::new(),
        };
        let config = collect(Mode::Interactive, &answers, &no_env, &mut prompter).unwrap();
        assert_eq!(prompter.asked, vec!["VERSION".to_string()]);
        assert_eq!(config["VERSION"], "5.4.0");
    }

    #[test]
    fn interactive_blank_version_falls_back_to_default() {
        // User is prompted for VERSION and presses Enter (empty) -> default.
        let mut answers = BTreeMap::new();
        for spec in SPEC {
            if spec.name != "VERSION" {
                answers.insert(spec.name.to_string(), "x".to_string());
            }
        }
        let mut prompter = FakePrompter {
            answers: BTreeMap::new(), // returns "" for VERSION
            asked: Vec::new(),
        };
        let config = collect(Mode::Interactive, &answers, &no_env, &mut prompter).unwrap();
        assert_eq!(config["VERSION"], "5.4.0");
    }
}
