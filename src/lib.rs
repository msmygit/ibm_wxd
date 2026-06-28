//! wxd-config — collect & validate watsonx.data install configuration and
//! generate a deterministic, source-able `cpd_vars.sh`.
//!
//! This is the first increment of the watsonx.data Easy Installer (see
//! `.buildpilot/PRODUCT.md`). It is pure local logic: it NEVER contacts a
//! cluster, runs `oc`/`cpd-cli`, or installs anything. It collects inputs
//! (interactively or non-interactively), validates them, and writes the config
//! file the downstream install steps will consume.
//!
//! The library exposes the orchestration ([`run`]) and a [`StdinPrompter`] so the
//! binary stays a thin shell. All collection/validation/generation logic lives in
//! the submodules and is unit-tested without touching real stdin or a cluster.

pub mod cli;
pub mod collect;
pub mod generate;
pub mod mask;
pub mod spec;
pub mod validate;

use collect::{Mode, Prompter};
use std::io::{BufRead, Write};

/// Outcome of a [`run`] invocation, mapped to a process exit code by the binary.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Help or version was printed; exit 0 without doing work.
    Printed,
    /// A `cpd_vars.sh` was successfully written to this path.
    Generated(String),
    /// Validation or input failed; the message has been written to stderr.
    Failed,
}

/// Side-effect interfaces injected into [`run`] so the orchestration itself is
/// testable: a stdout sink, a stderr sink, an environment lookup, a prompter,
/// and a file writer.
pub struct Io<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub prompter: &'a mut dyn Prompter,
    /// Writes the generated file. Injected so tests assert content without
    /// touching the filesystem.
    pub write_file: &'a mut dyn FnMut(&str, &str) -> std::io::Result<()>,
    /// Reads an answers file's contents by path.
    pub read_file: &'a dyn Fn(&str) -> std::io::Result<String>,
}

/// Run the full collect -> validate -> generate flow.
///
/// Returns a [`RunOutcome`]; the binary maps `Generated`/`Printed` to exit 0 and
/// `Failed` to exit 1. No `cpd_vars.sh` is written on any failure (AC3).
pub fn run(args: &[String], io: &mut Io) -> std::io::Result<RunOutcome> {
    let opts = match cli::parse(args) {
        Ok(o) => o,
        Err(msg) => {
            writeln!(io.stderr, "error: {msg}")?;
            return Ok(RunOutcome::Failed);
        }
    };

    if opts.show_help {
        writeln!(io.stdout, "{}", cli::help_text())?;
        return Ok(RunOutcome::Printed);
    }
    if opts.show_version {
        writeln!(io.stdout, "{}", cli::version_text())?;
        return Ok(RunOutcome::Printed);
    }

    // 1. Load answers file (if any).
    let answers = match &opts.answers_file {
        Some(path) => {
            let body = match (io.read_file)(path) {
                Ok(b) => b,
                Err(e) => {
                    writeln!(io.stderr, "error: cannot read answers file '{path}': {e}")?;
                    return Ok(RunOutcome::Failed);
                }
            };
            match collect::parse_answers(&body) {
                Ok(m) => m,
                Err(msg) => {
                    writeln!(io.stderr, "error: invalid answers file '{path}': {msg}")?;
                    return Ok(RunOutcome::Failed);
                }
            }
        }
        None => std::collections::BTreeMap::new(),
    };

    let mode = if opts.non_interactive {
        Mode::NonInteractive
    } else {
        Mode::Interactive
    };

    // 2. Collect.
    let config = collect::collect(mode, &answers, io.env, io.prompter)?;

    // 3. Validate every required variable; accumulate ALL problems so the user
    //    sees the full picture, not just the first failure.
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for v in spec::SPEC {
        let value = config.get(v.name).map(String::as_str).unwrap_or("");
        let outcome = validate::validate_value(v, value);
        if let Some(e) = outcome.error {
            errors.push(e);
        }
        if let Some(w) = outcome.warning {
            warnings.push(w);
        }
    }

    for w in &warnings {
        writeln!(io.stderr, "warning: {w}")?;
    }

    if !errors.is_empty() {
        writeln!(
            io.stderr,
            "error: configuration is invalid; no file was written:"
        )?;
        for e in &errors {
            writeln!(io.stderr, "  - {e}")?;
        }
        return Ok(RunOutcome::Failed);
    }

    // 4. Generate and write. (config is fully validated here.)
    let contents = generate::render(&config);
    if let Err(e) = (io.write_file)(&opts.output_file, &contents) {
        writeln!(io.stderr, "error: failed to write '{}': {e}", opts.output_file)?;
        return Ok(RunOutcome::Failed);
    }

    // 5. Masked summary (AC10) — never echo secrets.
    writeln!(io.stdout, "Wrote {} with:", opts.output_file)?;
    for v in spec::SPEC {
        let value = config.get(v.name).map(String::as_str).unwrap_or("");
        writeln!(
            io.stdout,
            "  {} = {}",
            v.name,
            mask::display_value(v.secret, value)
        )?;
    }

    Ok(RunOutcome::Generated(opts.output_file))
}

/// Real interactive prompter reading from stdin and writing prompts to stderr.
///
/// Secret input is NOT echoed: on a Unix TTY terminal echo is disabled for the
/// duration of the read; if stdin is not a TTY (piped), the line is read without
/// echo control (no terminal echo happens anyway). The entered secret is never
/// printed back (AC10).
pub struct StdinPrompter;

impl Prompter for StdinPrompter {
    fn prompt(&mut self, name: &str, description: &str, secret: bool) -> std::io::Result<String> {
        let mut err = std::io::stderr();
        write!(err, "{name} ({description}): ")?;
        err.flush()?;

        if secret {
            read_secret_line()
        } else {
            read_plain_line()
        }
    }
}

fn read_plain_line() -> std::io::Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

/// Read a line with terminal echo suppressed when stdin is an interactive TTY.
/// Uses raw `termios` via libc-free FFI to avoid an external dependency; falls
/// back to a plain read when not a TTY.
#[cfg(unix)]
fn read_secret_line() -> std::io::Result<String> {
    use std::os::unix::io::AsRawFd;
    let fd = std::io::stdin().as_raw_fd();

    // SAFETY: isatty/tcgetattr/tcsetattr are standard POSIX calls; we only ever
    // toggle the ECHO flag and always restore the original termios.
    unsafe {
        if ffi::isatty(fd) != 1 {
            return read_plain_line();
        }
        let mut term: ffi::Termios = std::mem::zeroed();
        if ffi::tcgetattr(fd, &mut term) != 0 {
            return read_plain_line();
        }
        let original = term;
        term.c_lflag &= !ffi::ECHO;
        if ffi::tcsetattr(fd, ffi::TCSANOW, &term) != 0 {
            return read_plain_line();
        }

        let result = read_plain_line();

        // Always restore echo, even on read error.
        let _ = ffi::tcsetattr(fd, ffi::TCSANOW, &original);
        // Move to a fresh line since the user's Enter wasn't echoed.
        let _ = writeln!(std::io::stderr());
        result
    }
}

#[cfg(not(unix))]
fn read_secret_line() -> std::io::Result<String> {
    // No portable no-echo without a dependency; read plainly. Non-Unix dev boxes
    // are out of the supported target for this increment.
    read_plain_line()
}

/// Minimal POSIX termios FFI, just enough to toggle terminal echo for secret
/// prompts without pulling in the `libc` crate (keeps the build dependency-free).
#[cfg(unix)]
mod ffi {
    #[allow(non_camel_case_types)]
    pub type tcflag_t = std::os::raw::c_ulong;
    #[allow(non_camel_case_types)]
    pub type cc_t = std::os::raw::c_uchar;
    #[allow(non_camel_case_types)]
    pub type speed_t = std::os::raw::c_ulong;

    // termios layout is platform-specific; NCCS differs between macOS and Linux.
    #[cfg(target_os = "macos")]
    pub const NCCS: usize = 20;
    #[cfg(not(target_os = "macos"))]
    pub const NCCS: usize = 32;

    #[cfg(target_os = "macos")]
    pub const ECHO: tcflag_t = 0x0000_0008;
    #[cfg(not(target_os = "macos"))]
    pub const ECHO: tcflag_t = 0o0000010;

    pub const TCSANOW: std::os::raw::c_int = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Termios {
        pub c_iflag: tcflag_t,
        pub c_oflag: tcflag_t,
        pub c_cflag: tcflag_t,
        pub c_lflag: tcflag_t,
        pub c_cc: [cc_t; NCCS],
        pub c_ispeed: speed_t,
        pub c_ospeed: speed_t,
    }

    extern "C" {
        pub fn isatty(fd: std::os::raw::c_int) -> std::os::raw::c_int;
        pub fn tcgetattr(fd: std::os::raw::c_int, termios: *mut Termios) -> std::os::raw::c_int;
        pub fn tcsetattr(
            fd: std::os::raw::c_int,
            optional_actions: std::os::raw::c_int,
            termios: *const Termios,
        ) -> std::os::raw::c_int;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A prompter that always errors if called (proves the non-interactive path
    /// never prompts).
    struct NeverPrompter;
    impl Prompter for NeverPrompter {
        fn prompt(&mut self, name: &str, _d: &str, _s: bool) -> std::io::Result<String> {
            panic!("unexpected prompt for {name}");
        }
    }

    struct Harness {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        env: BTreeMap<String, String>,
        files: BTreeMap<String, String>,
        written: std::rc::Rc<std::cell::RefCell<BTreeMap<String, String>>>,
    }

    impl Harness {
        fn new() -> Self {
            Harness {
                stdout: Vec::new(),
                stderr: Vec::new(),
                env: BTreeMap::new(),
                files: BTreeMap::new(),
                written: std::rc::Rc::new(std::cell::RefCell::new(BTreeMap::new())),
            }
        }

        fn run(&mut self, args: &[&str]) -> RunOutcome {
            let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            let env = self.env.clone();
            let files = self.files.clone();
            let written = self.written.clone();
            let mut prompter = NeverPrompter;
            let mut write_file = move |path: &str, contents: &str| {
                written
                    .borrow_mut()
                    .insert(path.to_string(), contents.to_string());
                Ok(())
            };
            let env_fn = move |k: &str| env.get(k).cloned();
            let read_fn = move |p: &str| {
                files
                    .get(p)
                    .cloned()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no file"))
            };
            let mut io = Io {
                stdout: &mut self.stdout,
                stderr: &mut self.stderr,
                env: &env_fn,
                prompter: &mut prompter,
                write_file: &mut write_file,
                read_file: &read_fn,
            };
            run(&argv, &mut io).unwrap()
        }

        fn stdout_str(&self) -> String {
            String::from_utf8_lossy(&self.stdout).to_string()
        }
        fn stderr_str(&self) -> String {
            String::from_utf8_lossy(&self.stderr).to_string()
        }
        fn written_file(&self, path: &str) -> Option<String> {
            self.written.borrow().get(path).cloned()
        }
    }

    fn complete_env() -> BTreeMap<String, String> {
        let mut e = BTreeMap::new();
        e.insert("OCP_URL".into(), "https://api.c.example.com:6443".into());
        e.insert("OPENSHIFT_TYPE".into(), "self-managed".into());
        e.insert("IMAGE_ARCH".into(), "amd64".into());
        e.insert("OCP_USERNAME".into(), "kubeadmin".into());
        e.insert("OCP_PASSWORD".into(), "p@ss w$rd\"x".into());
        e.insert("IBM_ENTITLEMENT_KEY".into(), "ey-secret-key".into());
        e.insert("PROJECT_CPD_INST_OPERATORS".into(), "cpd-operators".into());
        e.insert("PROJECT_CPD_INST_OPERANDS".into(), "cpd-instance".into());
        e.insert("STG_CLASS_BLOCK".into(), "ocs-block".into());
        e.insert("STG_CLASS_FILE".into(), "ocs-file".into());
        e.insert("VERSION".into(), "5.3.x".into());
        e.insert("COMPONENTS".into(), "wxd".into());
        e
    }

    #[test]
    fn help_exits_printed_and_lists_vars() {
        let mut h = Harness::new();
        let outcome = h.run(&["--help"]);
        assert_eq!(outcome, RunOutcome::Printed);
        assert!(h.stdout_str().contains("IBM_ENTITLEMENT_KEY"));
    }

    #[test]
    fn version_exits_printed() {
        let mut h = Harness::new();
        assert_eq!(h.run(&["--version"]), RunOutcome::Printed);
    }

    #[test]
    fn non_interactive_full_env_generates(/* AC8 */) {
        let mut h = Harness::new();
        h.env = complete_env();
        let outcome = h.run(&["--non-interactive"]);
        assert_eq!(outcome, RunOutcome::Generated("cpd_vars.sh".into()));
        let file = h.written_file("cpd_vars.sh").expect("file written");
        for v in spec::SPEC {
            assert!(file.contains(&format!("export {}=", v.name)));
        }
    }

    #[test]
    fn missing_required_fails_names_var_and_writes_nothing(/* AC3 */) {
        let mut h = Harness::new();
        let mut env = complete_env();
        env.remove("IBM_ENTITLEMENT_KEY");
        h.env = env;
        let outcome = h.run(&["--non-interactive"]);
        assert_eq!(outcome, RunOutcome::Failed);
        assert!(h.stderr_str().contains("IBM_ENTITLEMENT_KEY"));
        assert!(h.written_file("cpd_vars.sh").is_none());
    }

    #[test]
    fn invalid_url_fails(/* AC4 */) {
        let mut h = Harness::new();
        let mut env = complete_env();
        env.insert("OCP_URL".into(), "not-a-url".into());
        h.env = env;
        assert_eq!(h.run(&["--non-interactive"]), RunOutcome::Failed);
        assert!(h.stderr_str().contains("OCP_URL"));
    }

    #[test]
    fn secret_not_echoed_in_summary(/* AC10 */) {
        let mut h = Harness::new();
        h.env = complete_env();
        h.run(&["--non-interactive"]);
        let out = h.stdout_str();
        assert!(out.contains("IBM_ENTITLEMENT_KEY = ********"));
        assert!(!out.contains("ey-secret-key"));
        assert!(!out.contains("p@ss w$rd"));
        // stderr also must not leak the secret.
        assert!(!h.stderr_str().contains("ey-secret-key"));
    }

    #[test]
    fn deterministic_two_runs_identical(/* AC9 */) {
        let mut a = Harness::new();
        a.env = complete_env();
        a.run(&["--non-interactive"]);
        let mut b = Harness::new();
        b.env = complete_env();
        b.run(&["--non-interactive"]);
        assert_eq!(a.written_file("cpd_vars.sh"), b.written_file("cpd_vars.sh"));
    }

    #[test]
    fn shell_significant_value_is_quoted_in_file(/* AC7 */) {
        let mut h = Harness::new();
        h.env = complete_env(); // OCP_PASSWORD = p@ss w$rd"x
        h.run(&["--non-interactive"]);
        let file = h.written_file("cpd_vars.sh").unwrap();
        assert!(file.contains("export OCP_PASSWORD='p@ss w$rd\"x'"));
    }

    #[test]
    fn unknown_enum_warns_but_still_generates(/* AC6 / Q2 */) {
        let mut h = Harness::new();
        let mut env = complete_env();
        env.insert("IMAGE_ARCH".into(), "ppc64le".into());
        h.env = env;
        let outcome = h.run(&["--non-interactive"]);
        assert_eq!(outcome, RunOutcome::Generated("cpd_vars.sh".into()));
        assert!(h.stderr_str().contains("ppc64le"));
    }

    #[test]
    fn answers_file_supplies_values() {
        let mut h = Harness::new();
        let mut body = String::new();
        for (k, v) in complete_env() {
            body.push_str(&format!("{k}={v}\n"));
        }
        h.files.insert("answers.txt".into(), body);
        let outcome = h.run(&["--non-interactive", "--answers", "answers.txt"]);
        assert_eq!(outcome, RunOutcome::Generated("cpd_vars.sh".into()));
    }

    #[test]
    fn missing_answers_file_fails_cleanly() {
        let mut h = Harness::new();
        let outcome = h.run(&["--non-interactive", "--answers", "nope.txt"]);
        assert_eq!(outcome, RunOutcome::Failed);
        assert!(h.stderr_str().contains("answers file"));
    }

    #[test]
    fn custom_output_path_respected() {
        let mut h = Harness::new();
        h.env = complete_env();
        let outcome = h.run(&["--non-interactive", "--output", "custom.sh"]);
        assert_eq!(outcome, RunOutcome::Generated("custom.sh".into()));
        assert!(h.written_file("custom.sh").is_some());
    }
}
