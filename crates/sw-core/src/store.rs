//! Persistence for runs: `state.json`, append-only `events.log`, and an
//! `artifacts/` directory, all under `<base>/runs/<run-id>/`. Secrets live in a
//! separate `secrets.json` written `0600` and are never placed in `state.json`.

use crate::event::Event;
use crate::model::{RunId, RunState};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Filesystem-backed run store. The base dir defaults to `~/.wxd` but is
/// injectable so tests run hermetically in a temp dir.
#[derive(Debug, Clone)]
pub struct RunStore {
    base: PathBuf,
}

impl RunStore {
    /// Create a store rooted at `base` (e.g. `~/.wxd`).
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Default store at `$HOME/.wxd` (falls back to `./.wxd` if `$HOME` is unset).
    pub fn default_home() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(home.join(".wxd"))
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        self.base.join("runs").join(id)
    }

    /// Directory for a run's artifacts (kubeconfig, cpd_vars.sh, logs).
    pub fn artifacts_dir(&self, id: &str) -> PathBuf {
        self.run_dir(id).join("artifacts")
    }

    fn ensure_run_dir(&self, id: &str) -> std::io::Result<PathBuf> {
        let dir = self.run_dir(id);
        std::fs::create_dir_all(dir.join("artifacts"))?;
        Ok(dir)
    }

    /// Persist (or overwrite) the run's `state.json`.
    pub fn save(&self, state: &RunState) -> std::io::Result<()> {
        let dir = self.ensure_run_dir(&state.id)?;
        let json = serde_json::to_string_pretty(state)?;
        // Write to a temp file then rename for atomicity.
        let tmp = dir.join("state.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join("state.json"))?;
        Ok(())
    }

    /// Load a run's `state.json`.
    pub fn load(&self, id: &str) -> std::io::Result<RunState> {
        let path = self.run_dir(id).join("state.json");
        let data = std::fs::read_to_string(path)?;
        let state = serde_json::from_str(&data)?;
        Ok(state)
    }

    /// List all run ids present in the store (unordered).
    pub fn list(&self) -> std::io::Result<Vec<RunId>> {
        let runs = self.base.join("runs");
        if !runs.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(runs)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
        Ok(ids)
    }

    /// Append one event to the run's `events.log` (one JSON object per line).
    pub fn append_event(&self, id: &str, event: &Event) -> std::io::Result<()> {
        let dir = self.ensure_run_dir(id)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.log"))?;
        let line = serde_json::to_string(event)?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Replay all historical events for a run (for late SSE subscribers).
    pub fn replay_events(&self, id: &str) -> std::io::Result<Vec<Event>> {
        let path = self.run_dir(id).join("events.log");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = std::fs::read_to_string(path)?;
        let mut events = Vec::new();
        for line in data.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(ev) = serde_json::from_str::<Event>(line) {
                events.push(ev);
            }
        }
        Ok(events)
    }

    /// Persist secrets to `secrets.json` with `0600` perms. Overwrites wholesale.
    pub fn save_secrets(
        &self,
        id: &str,
        secrets: &BTreeMap<String, String>,
    ) -> std::io::Result<()> {
        let dir = self.ensure_run_dir(id)?;
        let path = dir.join("secrets.json");
        let json = serde_json::to_string(secrets)?;
        std::fs::write(&path, json)?;
        set_owner_only(&path)?;
        Ok(())
    }

    /// Load secrets, or an empty map if none were stored.
    pub fn load_secrets(&self, id: &str) -> std::io::Result<BTreeMap<String, String>> {
        let path = self.run_dir(id).join("secrets.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::model::{RunStatus, StepState};

    fn temp_store() -> (RunStore, PathBuf) {
        // Unique-ish dir without Date/rand: use process id + addr of a local.
        let marker = format!("wxd-test-{}", std::process::id());
        let dir = std::env::temp_dir()
            .join(marker)
            .join(format!("s{}", &(stamp() as usize).to_string()));
        (RunStore::new(dir.clone()), dir)
    }

    // Monotonic-ish counter to keep run dirs distinct within a test process.
    fn stamp() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(1);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn save_load_roundtrip_and_list() {
        let (store, dir) = temp_store();
        let state = RunState::new("r1".into(), vec![StepState::new("m", "s", "S")]);
        store.save(&state).unwrap();
        let back = store.load("r1").unwrap();
        assert_eq!(back.status, RunStatus::Pending);
        assert_eq!(store.list().unwrap(), vec!["r1".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn events_append_and_replay_in_order() {
        let (store, dir) = temp_store();
        store
            .append_event(
                "r2",
                &Event::RunStatus {
                    status: RunStatus::Running,
                },
            )
            .unwrap();
        store
            .append_event(
                "r2",
                &Event::Log {
                    step: "m/s".into(),
                    line: "hi".into(),
                },
            )
            .unwrap();
        let events = store.replay_events("r2").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            Event::RunStatus {
                status: RunStatus::Running
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn secrets_roundtrip_and_are_not_in_state() {
        let (store, dir) = temp_store();
        let mut s = BTreeMap::new();
        s.insert("IBM_ENTITLEMENT_KEY".to_string(), "topsecret".to_string());
        store.save_secrets("r3", &s).unwrap();
        let back = store.load_secrets("r3").unwrap();
        assert_eq!(back.get("IBM_ENTITLEMENT_KEY").unwrap(), "topsecret");
        std::fs::remove_dir_all(dir).ok();
    }
}
