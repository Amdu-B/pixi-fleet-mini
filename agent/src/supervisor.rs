//! Runs the robot application (`pixi run start`) and restarts it when a new
//! release becomes active.
//!
//! Deliberately small: there is no watchdog that restarts a crashed app. See
//! "Limitations" in the README.

use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct Supervisor {
    pixi_bin: String,
    log_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl Supervisor {
    pub fn new(pixi_bin: String, log_path: PathBuf) -> Self {
        Self {
            pixi_bin,
            log_path,
            child: Mutex::new(None),
        }
    }

    /// Stops the running app (if any) and starts `start` from the release in `dir`.
    /// Returns the new process id.
    pub async fn restart(&self, dir: &Path, release_short: &str) -> Result<u32> {
        self.stop().await;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("cannot open {}", self.log_path.display()))?;
        writeln!(log, "--- agent: starting release {release_short} ---")?;
        let log_err = log.try_clone()?;
        let child = Command::new(&self.pixi_bin)
            .arg("run")
            .arg("--locked")
            .arg("--manifest-path")
            .arg(dir.join("pixi.toml"))
            .arg("start")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .context("could not start the robot application")?;
        let pid = child.id().unwrap_or(0);
        *self.child.lock().await = Some(child);
        Ok(pid)
    }

    /// Asks the whole process group to stop (SIGTERM), then force-kills after 3 s.
    pub async fn stop(&self) {
        let Some(mut child) = self.child.lock().await.take() else {
            return;
        };
        if let Some(pid) = child.id() {
            // SAFETY: plain syscalls on the process group created in `restart`.
            unsafe { libc::killpg(pid as i32, libc::SIGTERM) };
            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
            {
                unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
                let _ = child.wait().await;
            }
        }
    }
}
