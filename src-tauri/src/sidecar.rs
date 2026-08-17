//! Lifecycle for the Python ingestion worker.
//!
//! The contract with the worker is deliberately thin: we start a process and it
//! talks to the same SQLite file. No IPC, no shared memory, no ordering
//! assumptions. That is what lets the same worker move to a cloud machine later
//! without the app changing.
//!
//! If the worker can't be started (no Python, missing script, a packaged build
//! without the sidecar), the app keeps working — uploads queue up and the UI
//! says the worker is down. Nothing is lost; the queue drains when a worker
//! appears.

use serde::Serialize;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

pub struct Sidecar {
    child: Mutex<Option<Child>>,
    /// Why the worker isn't running, if it isn't.
    problem: Mutex<Option<String>>,
}

#[derive(Serialize)]
pub struct WorkerStatus {
    /// A worker process this app started is alive.
    pub running: bool,
    /// Last time any worker reported in (UTC, from the shared database).
    pub last_heartbeat: Option<String>,
    pub problem: Option<String>,
}

impl Sidecar {
    pub fn new() -> Sidecar {
        Sidecar {
            child: Mutex::new(None),
            problem: Mutex::new(None),
        }
    }

    /// Where the worker script lives: an explicit override, the bundled copy
    /// next to the executable, or the repo checkout in development.
    fn script_path() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("ROOTED_WORKER") {
            let path = PathBuf::from(path);
            return path.exists().then_some(path);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let bundled = dir.join("../Resources/sidecar/worker.py");
                if bundled.exists() {
                    return Some(bundled);
                }
            }
        }
        let in_repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecar/worker.py");
        in_repo.exists().then_some(in_repo)
    }

    fn python() -> String {
        std::env::var("ROOTED_PYTHON").unwrap_or_else(|_| "python3".into())
    }

    pub fn start(&self, db_path: &std::path::Path) {
        let Some(script) = Sidecar::script_path() else {
            self.set_problem(Some(
                "ingestion worker not found (set ROOTED_WORKER to sidecar/worker.py)".into(),
            ));
            return;
        };

        match Command::new(Sidecar::python())
            .arg(&script)
            .arg("--db")
            .arg(db_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                *self.child.lock().unwrap() = Some(child);
                self.set_problem(None);
            }
            Err(e) => self.set_problem(Some(format!(
                "could not start the ingestion worker ({}): {e}",
                Sidecar::python()
            ))),
        }
    }

    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn set_problem(&self, problem: Option<String>) {
        *self.problem.lock().unwrap() = problem;
    }

    pub fn status(&self, last_heartbeat: Option<String>) -> WorkerStatus {
        // `try_wait` reaps the process if it exited, so "running" stays honest.
        let mut guard = self.child.lock().unwrap();
        let running = match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    self.set_problem(Some(format!("the ingestion worker exited ({status})")));
                    *guard = None;
                    false
                }
                Err(_) => false,
            },
            None => false,
        };
        WorkerStatus {
            running,
            last_heartbeat,
            problem: self.problem.lock().unwrap().clone(),
        }
    }
}
