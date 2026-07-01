//! wxd-config binary entrypoint.
//!
//! Thin wrapper: it wires real stdin/stdout/stderr, the process environment, and
//! the filesystem into [`wxd_config::run`], then maps the outcome to an exit
//! code. All logic and validation live in the library so they are unit-testable
//! without a process or a cluster.

use std::io::Write;
use std::process::ExitCode;

use wxd_config::{run, Io, RunOutcome, StdinPrompter};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut prompter = StdinPrompter;

    let env = |k: &str| std::env::var(k).ok();
    let read_file = |p: &str| std::fs::read_to_string(p);
    let mut write_file =
        |path: &str, contents: &str| -> std::io::Result<()> { std::fs::write(path, contents) };

    let result = {
        let mut io = Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &env,
            prompter: &mut prompter,
            write_file: &mut write_file,
            read_file: &read_file,
        };
        run(&args, &mut io)
    };

    match result {
        Ok(RunOutcome::Printed) | Ok(RunOutcome::Generated(_)) => ExitCode::SUCCESS,
        Ok(RunOutcome::Failed) => ExitCode::FAILURE,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: {e}");
            ExitCode::FAILURE
        }
    }
}
