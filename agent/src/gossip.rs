//! Peer-to-peer state exchange.
//!
//! Every tick the node pushes its whole fleet state to every peer it knows and
//! merges the peer's answer into its own (push-pull in one round trip). Peers
//! are the bootstrap list plus every robot address seen in the state, which is
//! how the fleet discovers itself starting from a single known address.
//!
//! Fine for a fleet of dozens. Larger fleets would gossip with a few random
//! peers per tick and send deltas instead of full state.

use crate::node::Node;
use crate::state::FleetState;
use std::sync::Arc;
use std::time::Duration;

pub async fn run(node: Arc<Node>) {
    let mut tick = tokio::time::interval(node.cfg.gossip_interval);
    loop {
        tick.tick().await;
        node.heartbeat();
        for peer in node.known_peers() {
            if let Err(e) = exchange(&node, &peer).await {
                tracing::debug!("gossip with {peer} failed: {e:#}");
            }
        }
    }
}

async fn exchange(node: &Node, peer: &str) -> anyhow::Result<()> {
    let mine = node.snapshot();
    let theirs: FleetState = node
        .http
        .post(format!("{peer}/state"))
        .timeout(Duration::from_secs(3))
        .json(&mine)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    node.merge_remote(&theirs);
    Ok(())
}
