//! The reconcile loop: make this robot match the desired release.
//!
//! ```text
//! desired release differs from what I run?
//!   -> canary / busy gates allow it?
//!   -> have the bundle? if not, ask peers (and verify the hash)
//!   -> unpack into releases/<hash>/, `pixi install --locked`, run the health check
//!        pass: atomically flip `current`, restart the app
//!        fail: delete the new directory; `current` never moved = rollback
//! ```
//!
//! This is a feedback loop in the control-engineering sense: desired release =
//! setpoint, current release = measurement, the agent = controller.

use crate::installer;
use crate::node::Node;
use crate::release::{short, Bundle};
use crate::state::{rollout_decision, Decision, Health};
use anyhow::{bail, Result};
use std::path::Path;
use std::sync::Arc;

pub async fn run(node: Arc<Node>) {
    startup_check(&node).await;
    loop {
        tokio::select! {
            _ = node.notify.notified() => {}
            _ = tokio::time::sleep(node.cfg.reconcile_interval) => {}
        }
        if let Err(e) = reconcile_once(&node).await {
            node.event("error", format!("reconcile error: {e:#}"));
        }
    }
}

/// After a restart: verify the release found on disk before reporting it healthy.
async fn startup_check(node: &Node) {
    let Some(hash) = node.store.current_hash() else {
        node.mutate_own(|o| o.detail = "no release installed yet".into());
        return;
    };
    let dir = node.store.release_dir(&hash);
    match installer::healthcheck(&node.cfg.pixi, &dir).await {
        Ok(_) => {
            node.mutate_own(|o| {
                o.health = Health::Healthy;
                o.detail = format!("recovered release {} from disk", short(&hash));
            });
            node.event(
                "info",
                format!(
                    "found release {} on disk, health check passed",
                    short(&hash)
                ),
            );
            if node.cfg.run_app {
                start_app(node, &dir, &hash).await;
            }
        }
        Err(e) => {
            let msg = truncate(&format!("{e:#}"), 240);
            node.mutate_own(|o| {
                o.health = Health::Failed;
                o.detail = format!("release {} on disk is unhealthy: {msg}", short(&hash));
            });
            node.event(
                "error",
                format!(
                    "release {} on disk failed its health check: {e:#}",
                    short(&hash)
                ),
            );
        }
    }
}

pub async fn reconcile_once(node: &Node) -> Result<()> {
    let fleet = node.snapshot();
    let desired = fleet.desired.clone();
    let Some(hash) = desired.release_hash.clone() else {
        return Ok(());
    };
    let own = node.own();

    if own.current_release.as_deref() == Some(hash.as_str()) {
        node.clear_note();
        return Ok(()); // converged
    }
    if node.was_attempted(&hash, desired.counter) {
        return Ok(()); // already tried (and rejected) for this rollout; wait for a new one
    }

    match rollout_decision(&fleet, &node.cfg.id, &desired) {
        Decision::Apply => {}
        Decision::WaitCanary(c) => {
            node.note(
                &format!("wait:{hash}:{c}"),
                format!("release {} is waiting for canary {c}", short(&hash)),
            );
            return Ok(());
        }
        Decision::Halted(c) => {
            node.note(
                &format!("halt:{hash}:{c}"),
                format!("rollout of {} halted: canary {c} rejected it", short(&hash)),
            );
            return Ok(());
        }
    }

    if own.busy {
        node.note(
            &format!("busy:{hash}"),
            "robot is busy, postponing the update",
        );
        return Ok(());
    }

    let Some(bundle) = obtain_bundle(node, &hash).await else {
        node.note(
            &format!("nobundle:{hash}"),
            format!(
                "release {} not available yet: no known peer has it",
                short(&hash)
            ),
        );
        return Ok(());
    };
    apply(node, &bundle, desired.counter).await
}

/// Local cache first, then peers. Whatever a peer sends is verified against the
/// requested hash before it is stored or used.
async fn obtain_bundle(node: &Node, hash: &str) -> Option<Bundle> {
    match node.store.get_bundle(hash) {
        Ok(Some(b)) => return Some(b),
        Ok(None) => {}
        Err(e) => node.event(
            "warn",
            format!(
                "local copy of {} is unusable, refetching: {e:#}",
                short(hash)
            ),
        ),
    }
    for peer in node.known_peers() {
        match fetch_from_peer(node, &peer, hash).await {
            Ok(Some(bundle)) => {
                if let Err(e) = node.store.put_bundle(&bundle) {
                    node.event("warn", format!("could not store bundle: {e:#}"));
                    continue;
                }
                node.event(
                    "info",
                    format!("fetched release {} from peer {peer}", short(hash)),
                );
                return Some(bundle);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!("fetching {} from {peer} failed: {e:#}", short(hash)),
        }
    }
    None
}

async fn fetch_from_peer(node: &Node, peer: &str, hash: &str) -> Result<Option<Bundle>> {
    let resp = node
        .http
        .get(format!("{peer}/bundles/{hash}"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let bundle: Bundle = resp.error_for_status()?.json().await?;
    bundle.validate()?;
    if bundle.hash() != hash {
        bail!("peer {peer} sent a bundle that does not match the requested hash");
    }
    Ok(Some(bundle))
}

async fn apply(node: &Node, bundle: &Bundle, desired_counter: u64) -> Result<()> {
    let hash = bundle.hash();
    let s = short(&hash).to_string();
    let label = bundle
        .label
        .as_deref()
        .map(|l| format!(" ({l})"))
        .unwrap_or_default();
    node.mark_attempted(&hash, desired_counter);
    node.mutate_own(|o| {
        o.health = Health::Updating;
        o.detail = format!("installing {s}");
    });
    node.event("info", format!("applying release {s}{label}"));

    let outcome: Result<()> = async {
        let dir = node.store.materialize(bundle)?;
        installer::install(&node.cfg.pixi, &dir).await?;
        installer::healthcheck(&node.cfg.pixi, &dir).await?;
        node.store.switch_to(&hash)?; // the only step that changes what the robot runs
        Ok(())
    }
    .await;

    match outcome {
        Ok(()) => {
            node.mutate_own(|o| {
                o.current_release = Some(hash.clone());
                o.health = Health::Healthy;
                o.last_failed = None;
                o.detail = format!("running {s}");
            });
            node.event(
                "info",
                format!("release {s}{label} is active (health check passed)"),
            );
            if node.cfg.run_app {
                start_app(node, &node.store.release_dir(&hash), &hash).await;
            }
            match node.store.gc() {
                Ok(removed) if !removed.is_empty() => {
                    node.event("info", format!("removed {} old release(s)", removed.len()))
                }
                Ok(_) => {}
                Err(e) => node.event("warn", format!("cleanup failed: {e:#}")),
            }
        }
        Err(e) => {
            node.store.remove_release(&hash);
            let staying = node.store.current_hash();
            let reason = truncate(&format!("{e:#}"), 240);
            node.mutate_own(|o| {
                o.last_failed = Some(hash.clone());
                o.health = if staying.is_some() {
                    Health::Healthy
                } else {
                    Health::Failed
                };
                o.detail = format!("rejected {s}, rolled back: {reason}");
            });
            let stay_msg = staying
                .map(|h| format!("staying on {}", short(&h)))
                .unwrap_or_else(|| "nothing installed".into());
            node.event(
                "error",
                format!("release {s}{label} rejected, {stay_msg}: {e:#}"),
            );
        }
    }
    Ok(())
}

async fn start_app(node: &Node, dir: &Path, hash: &str) {
    match node.supervisor.restart(dir, short(hash)).await {
        Ok(pid) => node.event("info", format!("robot app (re)started, pid {pid}")),
        Err(e) => node.event("error", format!("could not start the robot app: {e:#}")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}...")
    }
}
