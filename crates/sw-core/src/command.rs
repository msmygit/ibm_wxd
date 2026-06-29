//! The single seam for every external command (`openshift-install`, `oc`,
//! `cpd-cli`, `helm`, `aws`). Real impl shells out; mock impl powers hermetic
//! tests. No module calls `std::process` directly.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;

/// Outcome of running an external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Abstraction over running external programs. Implementors must be `Send + Sync`
/// so the orchestrator can share one runner across steps/tasks.
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Run `program` with `args`. Returns the captured output. Implementations
    /// should not panic on a non-zero exit — they return it in `status`.
    async fn run(&self, program: &str, args: &[String]) -> std::io::Result<CommandOutput>;

    /// Run `program` with `args` plus extra environment variables (e.g.
    /// `KUBECONFIG`, so the command targets a specific cluster). The default
    /// ignores the env and delegates to [`run`](Self::run) — mock runners keep
    /// their existing matching/recording behavior; real runners override it.
    async fn run_with_env(
        &self,
        program: &str,
        args: &[String],
        _env: &[(String, String)],
    ) -> std::io::Result<CommandOutput> {
        self.run(program, args).await
    }
}

/// Real implementation that shells out via `tokio::process::Command`.
#[derive(Debug, Default, Clone)]
pub struct RealCommandRunner;

#[async_trait]
impl CommandRunner for RealCommandRunner {
    async fn run(&self, program: &str, args: &[String]) -> std::io::Result<CommandOutput> {
        self.run_with_env(program, args, &[]).await
    }

    async fn run_with_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
    ) -> std::io::Result<CommandOutput> {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().await?;
        Ok(CommandOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// A single canned response for the mock runner.
#[derive(Debug, Clone)]
pub struct MockResponse {
    /// Substring that must appear in the joined "program arg1 arg2" string for
    /// this response to match. Empty string matches anything.
    pub matches: String,
    pub output: CommandOutput,
}

impl MockResponse {
    pub fn ok(matches: &str, stdout: &str) -> Self {
        Self {
            matches: matches.to_string(),
            output: CommandOutput {
                status: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        }
    }

    pub fn fail(matches: &str, status: i32, stderr: &str) -> Self {
        Self {
            matches: matches.to_string(),
            output: CommandOutput {
                status,
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        }
    }
}

/// Hermetic test runner. Records every invocation and replays canned responses
/// in FIFO order, honoring the first response whose `matches` substring is found
/// in the command line.
#[derive(Debug, Default)]
pub struct MockCommandRunner {
    responses: Mutex<VecDeque<MockResponse>>,
    calls: Mutex<Vec<String>>,
}

impl MockCommandRunner {
    pub fn new(responses: Vec<MockResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The recorded command lines, in call order ("program arg1 arg2").
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl CommandRunner for MockCommandRunner {
    async fn run(&self, program: &str, args: &[String]) -> std::io::Result<CommandOutput> {
        let line = if args.is_empty() {
            program.to_string()
        } else {
            format!("{} {}", program, args.join(" "))
        };
        self.calls.lock().unwrap().push(line.clone());

        let mut q = self.responses.lock().unwrap();
        // First, try a matching response without consuming non-matching ones.
        if let Some(pos) = q
            .iter()
            .position(|r| r.matches.is_empty() || line.contains(&r.matches))
        {
            return Ok(q.remove(pos).unwrap().output);
        }
        // No canned response: default to a benign success so tests need only
        // declare the responses they care about.
        Ok(CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_matches_by_substring_and_records_calls() {
        let runner = MockCommandRunner::new(vec![
            MockResponse::ok("version", "v1.2.3"),
            MockResponse::fail("create cluster", 1, "boom"),
        ]);

        let v = runner
            .run("oc", &["version".into()])
            .await
            .unwrap();
        assert!(v.success());
        assert_eq!(v.stdout, "v1.2.3");

        let c = runner
            .run("openshift-install", &["create".into(), "cluster".into()])
            .await
            .unwrap();
        assert!(!c.success());
        assert_eq!(c.stderr, "boom");

        assert_eq!(
            runner.calls(),
            vec![
                "oc version".to_string(),
                "openshift-install create cluster".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn mock_defaults_to_success_when_unmatched() {
        let runner = MockCommandRunner::new(vec![]);
        let out = runner.run("aws", &["sts".into()]).await.unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, "");
    }

    #[tokio::test]
    async fn real_runner_injects_env() {
        // Verify run_with_env actually exports the variable to the child.
        let runner = RealCommandRunner;
        let out = runner
            .run_with_env(
                "sh",
                &["-c".into(), "printf %s \"$KUBECONFIG\"".into()],
                &[("KUBECONFIG".into(), "/tmp/kc.test".into())],
            )
            .await
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, "/tmp/kc.test");
    }
}
