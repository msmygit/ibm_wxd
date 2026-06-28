//! Configuration collection — interactive and non-interactive (AC8).
//!
//! Sources, in increasing precedence:
//!   1. An optional answers file (`KEY=VALUE` lines, `#` comments).
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

/// Parse an answers file body into a map. Each non-blank, non-comment line must
/// be `KEY=VALUE`. Surrounding whitespace around KEY is trimmed; the VALUE is
/// taken verbatim after the first `=` (so values may contain `=`). A single pair
/// of matching surrounding quotes on the value is stripped, to be friendly to
/// files written by hand or by a previous `cpd_vars.sh`-style export.
///
/// Lines beginning with `export ` are tolerated (the leading keyword is dropped)
/// so a prior `cpd_vars.sh` can be re-used as an answers file.
pub fn parse_answers(body: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
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
        map.insert(key, strip_one_quote_pair(value));
    }
    Ok(map)
}

/// Strip a single matching pair of surrounding single or double quotes, if
/// present. Leaves the value untouched otherwise.
fn strip_one_quote_pair(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// Resolve every variable's value from file + env, then (only in
/// [`Mode::Interactive`]) prompt for whatever is still missing.
///
/// Returns the assembled [`Config`] containing whatever was found. Required-ness
/// and value validity are NOT enforced here — that is the validator's job, run by
/// the caller after collection — so that a single, consistent error path reports
/// all problems (AC3).
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
        let resolved = from_env.or(from_file);

        match resolved {
            Some(value) => {
                config.insert(spec.name.to_string(), value);
            }
            None => {
                if mode == Mode::Interactive {
                    let value = prompter.prompt(spec.name, spec.description, spec.secret)?;
                    config.insert(spec.name.to_string(), value);
                }
                // NonInteractive: leave it out; validator will flag it missing.
            }
        }
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
        fn prompt(&mut self, name: &str, _description: &str, _secret: bool) -> std::io::Result<String> {
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
        let body = "OCP_URL=https://x\nVERSION=5.3.x\n";
        let m = parse_answers(body).unwrap();
        assert_eq!(m["OCP_URL"], "https://x");
        assert_eq!(m["VERSION"], "5.3.x");
    }

    #[test]
    fn skips_comments_and_blanks() {
        let body = "# a comment\n\nVERSION=5.3.x\n   # indented comment\n";
        let m = parse_answers(body).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m["VERSION"], "5.3.x");
    }

    #[test]
    fn tolerates_export_prefix() {
        let m = parse_answers("export VERSION=5.3.x\n").unwrap();
        assert_eq!(m["VERSION"], "5.3.x");
    }

    #[test]
    fn value_may_contain_equals() {
        let m = parse_answers("COMPONENTS=a=1,b=2\n").unwrap();
        assert_eq!(m["COMPONENTS"], "a=1,b=2");
    }

    #[test]
    fn strips_one_quote_pair() {
        let m = parse_answers("OCP_PASSWORD='p@ss w$rd'\n").unwrap();
        assert_eq!(m["OCP_PASSWORD"], "p@ss w$rd");
        let m2 = parse_answers("VERSION=\"5.3.x\"\n").unwrap();
        assert_eq!(m2["VERSION"], "5.3.x");
    }

    #[test]
    fn malformed_line_errors() {
        let err = parse_answers("VERSION=5.3.x\nnonsense\n").unwrap_err();
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
        let config =
            collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        assert_eq!(config["VERSION"], "from-file");
    }

    // ---- collect: non-interactive never prompts (AC8) ----

    #[test]
    fn non_interactive_never_prompts() {
        let answers = BTreeMap::new();
        // NeverPrompter panics if called; reaching the end proves no prompt.
        let config =
            collect(Mode::NonInteractive, &answers, &no_env, &mut NeverPrompter).unwrap();
        // Nothing supplied, nothing prompted -> empty config (validator flags it).
        assert!(config.is_empty());
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
                a.insert("VERSION".to_string(), "5.3.x".to_string());
                a
            },
            asked: Vec::new(),
        };
        let config = collect(Mode::Interactive, &answers, &no_env, &mut prompter).unwrap();
        assert_eq!(prompter.asked, vec!["VERSION".to_string()]);
        assert_eq!(config["VERSION"], "5.3.x");
    }
}
