//! Command-line client functions: talk to a running agent over HTTP.

use crate::release::{short, Bundle};
use crate::server::FleetView;
use crate::state::{Desired, Health};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::{Duration, Instant};

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?)
}

fn base(to: &str) -> String {
    to.trim_end_matches('/').to_string()
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    bail!("agent answered {status}: {body}");
}

#[derive(Deserialize)]
struct Stored {
    hash: String,
}

/// Packs `dir` into a bundle and uploads it. Returns the release hash.
pub async fn publish(dir: &Path, label: Option<String>, to: &str) -> Result<String> {
    let label = label.or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()));
    let bundle = Bundle::from_dir(dir, label)?;
    let resp = http()?
        .post(format!("{}/bundles", base(to)))
        .json(&bundle)
        .send()
        .await
        .with_context(|| format!("cannot reach {to}"))?;
    let stored: Stored = check(resp).await?.json().await?;
    Ok(stored.hash)
}

pub async fn desire(to: &str, hash: &str, canary: Option<String>) -> Result<Desired> {
    let resp = http()?
        .post(format!("{}/desired", base(to)))
        .json(&serde_json::json!({ "release_hash": hash, "canary": canary }))
        .send()
        .await
        .with_context(|| format!("cannot reach {to}"))?;
    Ok(check(resp).await?.json().await?)
}

pub async fn set_busy(to: &str, busy: bool) -> Result<()> {
    let resp = http()?
        .post(format!("{}/busy", base(to)))
        .json(&serde_json::json!({ "busy": busy }))
        .send()
        .await
        .with_context(|| format!("cannot reach {to}"))?;
    check(resp).await?;
    Ok(())
}

pub async fn fleet(to: &str) -> Result<FleetView> {
    let resp = http()?
        .get(format!("{}/api/fleet", base(to)))
        .send()
        .await
        .with_context(|| format!("cannot reach {to}"))?;
    Ok(check(resp).await?.json().await?)
}

/// Prints the fleet as a table.
pub async fn print_status(to: &str) -> Result<()> {
    let f = fleet(to).await?;
    println!(
        "{:<12} {:<9} {:<9} {:<7} DETAIL",
        "ROBOT", "RELEASE", "HEALTH", "ONLINE"
    );
    for r in &f.robots {
        let s = &r.status;
        println!(
            "{:<12} {:<9} {:<9} {:<7} {}{}",
            s.robot_id,
            s.current_release.as_deref().map(short).unwrap_or("-"),
            format!("{:?}", s.health).to_lowercase(),
            if r.online { "yes" } else { "no" },
            if s.busy { "[busy] " } else { "" },
            s.detail
        );
    }
    println!(
        "\ndesired: {}  canary: {}  (#{} set by {})",
        f.desired.release_hash.as_deref().map(short).unwrap_or("-"),
        f.desired.canary.as_deref().unwrap_or("-"),
        f.desired.counter,
        if f.desired.set_by.is_empty() {
            "-"
        } else {
            &f.desired.set_by
        }
    );
    Ok(())
}

/// Blocks until every robot in `robots` runs `hash` and is healthy, or (with
/// `rejected`) until every one of them has rejected it. Used by the demo script.
pub async fn wait(
    to: &str,
    hash: &str,
    robots: &[String],
    rejected: bool,
    timeout: Duration,
) -> Result<()> {
    let start = Instant::now();
    loop {
        if let Ok(f) = fleet(to).await {
            let done = robots.iter().all(|id| {
                f.robots
                    .iter()
                    .find(|r| &r.status.robot_id == id)
                    .map(|r| {
                        if rejected {
                            r.status.last_failed.as_deref() == Some(hash)
                        } else {
                            r.status.current_release.as_deref() == Some(hash)
                                && r.status.health == Health::Healthy
                        }
                    })
                    .unwrap_or(false)
            });
            if done {
                return Ok(());
            }
        }
        if start.elapsed() > timeout {
            bail!(
                "timed out after {}s waiting for {}",
                timeout.as_secs(),
                short(hash)
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
