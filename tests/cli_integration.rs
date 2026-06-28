//! End-to-end integration tests driving the built binary as a subprocess.
//!
//! These exercise the ACs that involve the real shell and real process exit
//! codes — none of them contact a cluster or require `oc`/`cpd-cli` (AC12):
//!
//!   AC1  the binary builds (implicit: these tests can't run otherwise)
//!   AC2  generated file is valid shell (`bash -n`) and has every var
//!   AC7  shell-significant values round-trip through `source`
//!   AC8  non-interactive run reads no stdin and exits 0
//!   AC9  two runs produce byte-identical output
//!   AC10 secrets are masked in stdout
//!   AC11 --help exits 0 and lists required vars
//!
//! `bash` is used when present (the macOS/Linux dev box has it); the shell tests
//! skip gracefully if no POSIX shell is found, so the suite never hard-fails on
//! an unusual box.

use std::collections::HashMap;
use std::process::Command;

/// Path to the compiled binary under test (set by Cargo for integration tests).
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wxd-config")
}

/// A complete, valid environment for a non-interactive run. Deliberately
/// includes a password full of shell-significant characters for AC7.
fn complete_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("OCP_URL", "https://api.cluster.example.com:6443"),
        ("OPENSHIFT_TYPE", "self-managed"),
        ("IMAGE_ARCH", "amd64"),
        ("OCP_USERNAME", "kubeadmin"),
        ("OCP_PASSWORD", "p@ss w$rd\"x`y'z"),
        ("IBM_ENTITLEMENT_KEY", "ey-super-secret-entitlement"),
        ("PROJECT_CPD_INST_OPERATORS", "cpd-operators"),
        ("PROJECT_CPD_INST_OPERANDS", "cpd-instance"),
        ("STG_CLASS_BLOCK", "ocs-storagecluster-ceph-rbd"),
        ("STG_CLASS_FILE", "ocs-storagecluster-cephfs"),
        ("VERSION", "5.3.x"),
        ("COMPONENTS", "watsonx_data"),
    ]
}

/// Run the binary with a clean environment (only the vars we pass), the given
/// args, and no stdin. Returns (exit_success, stdout, stderr).
fn run_clean(env: &[(&str, &str)], args: &[&str], out_path: &std::path::Path) -> (bool, String, String) {
    let mut cmd = Command::new(bin());
    cmd.env_clear();
    // Keep PATH so the process can run; it changes no behavior under test.
    if let Ok(path) = std::env::var("PATH") {
        cmd.env("PATH", path);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(args);
    cmd.arg("--output").arg(out_path);
    cmd.stdin(std::process::Stdio::null()); // AC8: prove no stdin is read.
    let output = cmd.output().expect("failed to run binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Locate a POSIX shell for the shell-dependent tests, or None to skip.
fn find_shell() -> Option<&'static str> {
    ["bash", "sh"].into_iter().find(|sh| {
        Command::new(sh)
            .arg("-c")
            .arg("exit 0")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("wxd-config-test-{}-{}", std::process::id(), name));
    p
}

#[test]
fn help_exits_zero_and_lists_required_vars() {
    // AC11
    let out_path = tmp_path("help.sh");
    let output = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(output.status.success(), "--help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for var in ["OCP_URL", "IBM_ENTITLEMENT_KEY", "COMPONENTS", "VERSION"] {
        assert!(stdout.contains(var), "--help missing {var}");
    }
    assert!(stdout.contains("--non-interactive"));
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn non_interactive_generates_valid_shell_with_all_vars() {
    // AC2 + AC8
    let out_path = tmp_path("vars-valid.sh");
    let (ok, _stdout, stderr) = run_clean(&complete_env(), &["--non-interactive"], &out_path);
    assert!(ok, "non-interactive run should exit 0; stderr:\n{stderr}");
    let contents = std::fs::read_to_string(&out_path).expect("file should exist");

    for (k, _) in complete_env() {
        assert!(contents.contains(&format!("export {k}=")), "missing {k}");
    }

    if let Some(sh) = find_shell() {
        let status = Command::new(sh)
            .arg("-n")
            .arg(&out_path)
            .status()
            .expect("run shell -n");
        assert!(status.success(), "generated file is not valid shell");
    }
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn shell_significant_values_round_trip_through_source() {
    // AC7: source the file and echo each var back; compare to the original.
    let Some(sh) = find_shell() else {
        eprintln!("no POSIX shell found; skipping round-trip test");
        return;
    };
    let out_path = tmp_path("roundtrip.sh");
    let env = complete_env();
    let (ok, _o, e) = run_clean(&env, &["--non-interactive"], &out_path);
    assert!(ok, "generation failed: {e}");

    let env_map: HashMap<&str, &str> = env.iter().cloned().collect();
    for var in ["OCP_PASSWORD", "IBM_ENTITLEMENT_KEY", "COMPONENTS"] {
        let script = format!("source '{}'; printf '%s' \"${var}\"", out_path.display());
        let output = Command::new(sh)
            .arg("-c")
            .arg(&script)
            .output()
            .expect("source file");
        assert!(output.status.success(), "sourcing failed for {var}");
        let got = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            got, *env_map[var],
            "round-trip mismatch for {var}: got {got:?}"
        );
    }
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn two_runs_are_byte_identical() {
    // AC9
    let path_a = tmp_path("det-a.sh");
    let path_b = tmp_path("det-b.sh");
    run_clean(&complete_env(), &["--non-interactive"], &path_a);
    run_clean(&complete_env(), &["--non-interactive"], &path_b);
    let a = std::fs::read(&path_a).unwrap();
    let b = std::fs::read(&path_b).unwrap();
    assert_eq!(a, b, "output must be deterministic");
    let _ = std::fs::remove_file(path_a);
    let _ = std::fs::remove_file(path_b);
}

#[test]
fn secrets_are_masked_in_stdout() {
    // AC10
    let out_path = tmp_path("mask.sh");
    let (ok, stdout, _e) = run_clean(&complete_env(), &["--non-interactive"], &out_path);
    assert!(ok);
    assert!(stdout.contains("IBM_ENTITLEMENT_KEY = ********"));
    assert!(!stdout.contains("ey-super-secret-entitlement"));
    assert!(!stdout.contains("p@ss w$rd"));
    let _ = std::fs::remove_file(out_path);
}

#[test]
fn missing_required_var_fails_and_writes_no_file() {
    // AC3
    let out_path = tmp_path("missing.sh");
    let _ = std::fs::remove_file(&out_path);
    let mut env = complete_env();
    env.retain(|(k, _)| *k != "IBM_ENTITLEMENT_KEY");
    let (ok, _o, stderr) = run_clean(&env, &["--non-interactive"], &out_path);
    assert!(!ok, "missing required var must fail");
    assert!(stderr.contains("IBM_ENTITLEMENT_KEY"));
    assert!(!out_path.exists(), "no file should be written on failure");
}
