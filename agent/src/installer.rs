//! Thin wrappers around the `pixi` CLI.
//!
//! The agent does not solve or resolve anything itself. It only asks Pixi to
//! install exactly what the lockfile says (`--locked` fails if the lockfile is
//! out of date) and to run the release's own `healthcheck` task.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Debug)]
pub struct PixiConfig {
    /// Path or name of the pixi executable.
    pub bin: String,
    pub install_timeout: Duration,
    pub health_timeout: Duration,
}

impl Default for PixiConfig {
    fn default() -> Self {
        Self {
            bin: "pixi".into(),
            install_timeout: Duration::from_secs(900),
            health_timeout: Duration::from_secs(60),
        }
    }
}

/// `pixi install --locked` for the release in `dir`.
pub async fn install(cfg: &PixiConfig, dir: &Path) -> Result<()> {
    let mut cmd = Command::new(&cfg.bin);
    cmd.arg("install")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(dir.join("pixi.toml"));
    run(cmd, cfg.install_timeout, "pixi install")
        .await
        .map(|_| ())
}

/// `pixi run healthcheck` for the release in `dir`. Exit code 0 means healthy.
pub async fn healthcheck(cfg: &PixiConfig, dir: &Path) -> Result<String> {
    let mut cmd = Command::new(&cfg.bin);
    cmd.arg("run")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(dir.join("pixi.toml"))
        .arg("healthcheck");
    run(cmd, cfg.health_timeout, "health check").await
}

/// Runs a command to completion with a timeout. Output (stdout+stderr) is
/// captured; on failure its tail is included in the error so it ends up in the
/// fleet event log. The command gets its own process group so that a timeout
/// also kills everything it spawned (for example the Python process behind
/// `pixi run`).
async fn run(mut cmd: Command, timeout: Duration, what: &str) -> Result<String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let child = cmd
        .spawn()
        .with_context(|| format!("could not start {what} (is Pixi installed and on PATH?)"))?;
    let pid = child.id();
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => {
            if let Some(pid) = pid {
                // SAFETY: plain syscall on a process group we created above.
                unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
            }
            bail!("{what} timed out after {}s", timeout.as_secs());
        }
        Ok(r) => r.with_context(|| format!("{what} did not finish"))?,
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let tail = tail(text.trim(), 600);
    if !out.status.success() {
        bail!("{what} failed ({}): {}", out.status, tail);
    }
    Ok(tail)
}

fn tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", &s[start..])
}
