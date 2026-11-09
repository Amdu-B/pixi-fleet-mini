//! pixi-fleet-mini: a small peer-to-peer fleet agent built on Pixi.
//!
//! Module map (see docs/ARCHITECTURE.md for the full picture):
//!
//! * [`release`]    content-addressed release bundles
//! * [`state`]      replicated fleet state (a last-writer-wins CRDT) and canary rules
//! * [`store`]      on-disk layout, atomic `current` symlink switch
//! * [`installer`]  `pixi install --locked` / `pixi run healthcheck`
//! * [`supervisor`] runs the robot application, restarts it on a new release
//! * [`node`]       the shared in-memory state of one agent
//! * [`gossip`]     peer-to-peer state exchange and peer discovery
//! * [`reconcile`]  the control loop: converge to the desired release, or roll back
//! * [`server`]     HTTP API used by peers and the web UI
//! * [`client`]     CLI helpers that talk to a running agent

pub mod client;
pub mod gossip;
pub mod installer;
pub mod node;
pub mod reconcile;
pub mod release;
pub mod server;
pub mod state;
pub mod store;
pub mod supervisor;

use anyhow::Result;
use node::{Config, Node};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// A running agent: its shared state plus the background tasks.
pub struct Running {
    pub node: Arc<Node>,
    handles: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops the background tasks and the supervised robot application.
    pub async fn shutdown(self) {
        for h in &self.handles {
            h.abort();
        }
        self.node.supervisor.stop().await;
    }
}

/// Starts the HTTP server, the gossip loop and the reconcile loop.
/// The listener is passed in so callers (and tests) can bind port 0 first.
pub fn start(cfg: Config, listener: tokio::net::TcpListener) -> Result<Running> {
    let node = Arc::new(Node::new(cfg)?);
    let app = server::router(node.clone());
    let mut handles = vec![
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("HTTP server stopped: {e}");
            }
        }),
        tokio::spawn(gossip::run(node.clone())),
    ];
    if !node.cfg.operator_only {
        handles.push(tokio::spawn(reconcile::run(node.clone())));
    }
    Ok(Running { node, handles })
}
