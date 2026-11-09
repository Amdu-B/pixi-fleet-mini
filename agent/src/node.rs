//! `Node` is the shared in-memory state of one running agent: its replicated
//! view of the fleet, its own status, the event log and the local store.

use crate::installer::PixiConfig;
use crate::state::{Desired, FleetState, Health, RobotStatus};
use crate::store::Store;
use crate::supervisor::Supervisor;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;

const MAX_EVENTS: usize = 300;

#[derive(Clone, Debug)]
pub struct Config {
    pub id: String,
    /// Base URL other nodes use to reach this agent, e.g. `http://robot1:8000`.
    pub addr: String,
    pub data_dir: PathBuf,
    /// Bootstrap peers. More are discovered through gossip.
    pub peers: Vec<String>,
    pub pixi: PixiConfig,
    /// Supervise the robot application (`pixi run start`) after each switch.
    pub run_app: bool,
    /// Operator-only node: never installs releases itself.
    pub operator_only: bool,
    pub gossip_interval: Duration,
    pub reconcile_interval: Duration,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub ts: u64,
    pub level: &'static str,
    pub robot: String,
    pub message: String,
}

struct Inner {
    fleet: FleetState,
    /// Our own entry. We are its only writer, so this copy is authoritative.
    own: RobotStatus,
    /// robot id -> (last counter we saw, when we saw it change). Used for liveness.
    seen: HashMap<String, (u64, Instant)>,
    /// The (release, desired counter) we last tried to apply: do not retry it in a loop.
    attempted: Option<(String, u64)>,
    last_note: Option<String>,
}

pub struct Node {
    pub cfg: Config,
    pub store: Store,
    pub supervisor: Supervisor,
    pub notify: Notify,
    pub http: reqwest::Client,
    inner: Mutex<Inner>,
    events: Mutex<VecDeque<Event>>,
}

#[derive(Serialize, Deserialize)]
pub struct RobotView {
    #[serde(flatten)]
    pub status: RobotStatus,
    pub age_secs: u64,
    pub online: bool,
}

impl Node {
    pub fn new(mut cfg: Config) -> Result<Self> {
        cfg.addr = cfg.addr.trim_end_matches('/').to_string();
        cfg.peers = cfg
            .peers
            .iter()
            .map(|p| p.trim_end_matches('/').to_string())
            .collect();
        let store = Store::open(&cfg.data_dir)?;
        let supervisor = Supervisor::new(cfg.pixi.bin.clone(), store.logs_dir().join("app.log"));
        let own = RobotStatus {
            robot_id: cfg.id.clone(),
            addr: cfg.addr.clone(),
            current_release: store.current_hash(),
            health: if cfg.operator_only {
                Health::Healthy
            } else {
                Health::Unknown
            },
            last_failed: None,
            busy: false,
            operator: cfg.operator_only,
            detail: if cfg.operator_only {
                "operator node (runs no robot software)".into()
            } else {
                "starting".into()
            },
            counter: 1,
        };
        let mut fleet = FleetState::default();
        fleet.robots.insert(own.robot_id.clone(), own.clone());
        let mut seen = HashMap::new();
        seen.insert(own.robot_id.clone(), (own.counter, Instant::now()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            cfg,
            store,
            supervisor,
            notify: Notify::new(),
            http,
            inner: Mutex::new(Inner {
                fleet,
                own,
                seen,
                attempted: None,
                last_note: None,
            }),
            events: Mutex::new(VecDeque::new()),
        })
    }

    // ---- reading -------------------------------------------------------

    pub fn snapshot(&self) -> FleetState {
        self.inner.lock().unwrap().fleet.clone()
    }

    pub fn own(&self) -> RobotStatus {
        self.inner.lock().unwrap().own.clone()
    }

    /// Static bootstrap peers plus every robot address learned through gossip.
    pub fn known_peers(&self) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        let mut set: BTreeSet<String> = self.cfg.peers.iter().cloned().collect();
        for r in g.fleet.robots.values() {
            if r.robot_id != self.cfg.id && !r.addr.is_empty() {
                set.insert(r.addr.trim_end_matches('/').to_string());
            }
        }
        set.remove(&self.cfg.addr);
        set.into_iter().collect()
    }

    pub fn robot_views(&self) -> Vec<RobotView> {
        let g = self.inner.lock().unwrap();
        let online_within = (self.cfg.gossip_interval * 5).max(Duration::from_secs(10));
        g.fleet
            .robots
            .values()
            .map(|r| {
                let age = g
                    .seen
                    .get(&r.robot_id)
                    .map(|(_, t)| t.elapsed())
                    .unwrap_or_default();
                RobotView {
                    status: r.clone(),
                    age_secs: age.as_secs(),
                    online: age <= online_within,
                }
            })
            .collect()
    }

    // ---- writing -------------------------------------------------------

    /// Changes our own status entry and bumps its counter so peers accept it.
    pub fn mutate_own(&self, f: impl FnOnce(&mut RobotStatus)) {
        let mut g = self.inner.lock().unwrap();
        f(&mut g.own);
        g.own.counter += 1;
        let own = g.own.clone();
        g.fleet.robots.insert(own.robot_id.clone(), own);
        refresh_seen(&mut g);
    }

    /// Periodic no-op update. The rising counter is the liveness signal.
    pub fn heartbeat(&self) {
        self.mutate_own(|_| {});
    }

    /// Merges a state received from a peer. Returns true if `desired` changed.
    pub fn merge_remote(&self, remote: &FleetState) -> bool {
        let mut g = self.inner.lock().unwrap();
        let before = g.fleet.desired.clone();
        // After a restart our counter may be behind what peers still remember
        // about us. Jump past it so our next entry wins everywhere.
        if let Some(theirs) = remote.robots.get(&self.cfg.id) {
            if theirs.counter >= g.own.counter {
                g.own.counter = theirs.counter + 1;
                let own = g.own.clone();
                g.fleet.robots.insert(own.robot_id.clone(), own);
            }
        }
        let mut incoming = remote.clone();
        incoming.robots.remove(&self.cfg.id); // never take someone else's word about us
        g.fleet.merge(&incoming);
        refresh_seen(&mut g);
        let changed = g.fleet.desired != before;
        drop(g);
        if changed {
            self.notify.notify_one();
        }
        changed
    }

    /// Acts as operator: declares what the fleet should run.
    pub fn set_desired(&self, release_hash: Option<String>, canary: Option<String>) -> Desired {
        let mut g = self.inner.lock().unwrap();
        let d = Desired {
            release_hash,
            canary,
            counter: g.fleet.desired.counter + 1,
            set_by: self.cfg.id.clone(),
        };
        g.fleet.desired = d.clone();
        drop(g);
        self.notify.notify_one();
        d
    }

    pub fn mark_attempted(&self, hash: &str, counter: u64) {
        self.inner.lock().unwrap().attempted = Some((hash.to_string(), counter));
    }

    pub fn was_attempted(&self, hash: &str, counter: u64) -> bool {
        self.inner.lock().unwrap().attempted.as_ref() == Some(&(hash.to_string(), counter))
    }

    // ---- events --------------------------------------------------------

    pub fn event(&self, level: &'static str, message: impl Into<String>) {
        let message = message.into();
        match level {
            "error" => tracing::error!("{message}"),
            "warn" => tracing::warn!("{message}"),
            _ => tracing::info!("{message}"),
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut q = self.events.lock().unwrap();
        if q.len() >= MAX_EVENTS {
            q.pop_front();
        }
        q.push_back(Event {
            ts,
            level,
            robot: self.cfg.id.clone(),
            message,
        });
    }

    /// Like `event`, but only when `key` differs from the previous note.
    /// Keeps "still waiting for X" from flooding the log every 2 seconds.
    pub fn note(&self, key: &str, message: impl Into<String>) {
        {
            let mut g = self.inner.lock().unwrap();
            if g.last_note.as_deref() == Some(key) {
                return;
            }
            g.last_note = Some(key.to_string());
        }
        self.event("info", message);
    }

    pub fn clear_note(&self) {
        self.inner.lock().unwrap().last_note = None;
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().iter().cloned().collect()
    }
}

fn refresh_seen(g: &mut Inner) {
    let now = Instant::now();
    let Inner { fleet, seen, .. } = g;
    for (id, r) in &fleet.robots {
        let e = seen.entry(id.clone()).or_insert((r.counter, now));
        if e.0 != r.counter {
            *e = (r.counter, now);
        }
    }
}
