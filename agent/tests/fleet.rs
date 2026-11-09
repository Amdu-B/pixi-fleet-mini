//! End-to-end tests: several agents in one process, talking real HTTP over
//! localhost, with a fake `pixi` so no packages are downloaded.
//!
//! The real thing (actual Pixi, actual conda packages) is exercised by
//! `scripts/demo-local.sh`.

use agent::installer::PixiConfig;
use agent::node::Config;
use agent::release::Bundle;
use agent::state::Health;
use agent::Running;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::net::TcpListener;

struct Agent {
    run: Running,
    addr: String,
    _dir: TempDir,
}

impl Agent {
    fn node(&self) -> &agent::node::Node {
        &self.run.node
    }
}

fn fake_pixi() -> String {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake-pixi.sh");
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
    p.to_string()
}

async fn spawn(id: &str, peers: &[&str], run_app: bool) -> Agent {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        id: id.to_string(),
        addr: addr.clone(),
        data_dir: dir.path().to_path_buf(),
        peers: peers.iter().map(|s| s.to_string()).collect(),
        pixi: PixiConfig {
            bin: fake_pixi(),
            install_timeout: Duration::from_secs(20),
            health_timeout: Duration::from_secs(10),
        },
        run_app,
        operator_only: !run_app && id == "operator",
        gossip_interval: Duration::from_millis(100),
        reconcile_interval: Duration::from_millis(100),
    };
    Agent {
        run: agent::start(cfg, listener).unwrap(),
        addr,
        _dir: dir,
    }
}

fn bundle(label: &str, healthy: bool) -> Bundle {
    let mut files = BTreeMap::new();
    files.insert("pixi.toml".to_string(), format!("# {label}\n"));
    files.insert("pixi.lock".to_string(), "version: 6\n".to_string());
    files.insert(
        "healthcheck.sh".to_string(),
        if healthy {
            "exit 0\n".to_string()
        } else {
            "echo 'sensor driver missing' >&2\nexit 1\n".to_string()
        },
    );
    files.insert(
        "app.sh".to_string(),
        format!("echo 'app {label} running'\nexec sleep 30\n"),
    );
    Bundle {
        label: Some(label.to_string()),
        files,
    }
}

async fn publish(to: &Agent, b: &Bundle) -> String {
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{}/bundles", to.addr))
        .json(b)
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_success(),
        "publish failed: {}",
        r.text().await.unwrap()
    );
    b.hash()
}

async fn desire(to: &Agent, hash: &str, canary: Option<&str>) {
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{}/desired", to.addr))
        .json(&serde_json::json!({"release_hash": hash, "canary": canary}))
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_success(),
        "desire failed: {}",
        r.text().await.unwrap()
    );
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for: {what}");
}

fn on(a: &Agent, hash: &str) -> bool {
    a.node().own().current_release.as_deref() == Some(hash)
        && a.node().own().health == Health::Healthy
}

fn has_event(a: &Agent, needle: &str) -> bool {
    a.node().events().iter().any(|e| e.message.contains(needle))
}

async fn shutdown(agents: Vec<Agent>) {
    for a in agents {
        a.run.shutdown().await;
    }
}

#[tokio::test]
async fn fleet_converges_on_a_good_release() {
    let op = spawn("operator", &[], false).await;
    let r1 = spawn("r1", &[&op.addr], true).await;
    let r2 = spawn("r2", &[&op.addr], true).await;
    let r3 = spawn("r3", &[&op.addr], true).await;

    let v1 = bundle("v1", true);
    let h = publish(&op, &v1).await;
    desire(&op, &h, None).await;

    wait_until("all robots on v1", || {
        on(&r1, &h) && on(&r2, &h) && on(&r3, &h)
    })
    .await;

    // the operator's replicated view agrees
    wait_until("operator sees all three on v1", || {
        let s = op.node().snapshot();
        ["r1", "r2", "r3"].iter().all(|id| {
            s.robots
                .get(*id)
                .map(|r| r.current_release.as_deref() == Some(h.as_str()))
                .unwrap_or(false)
        })
    })
    .await;

    // on disk: `current` points at the release and the app was started from it
    assert_eq!(r1.node().store.current_hash().as_deref(), Some(h.as_str()));
    wait_until("app log written", || {
        std::fs::read_to_string(r1.node().store.logs_dir().join("app.log"))
            .map(|l| l.contains("app v1 running"))
            .unwrap_or(false)
    })
    .await;

    shutdown(vec![op, r1, r2, r3]).await;
}

#[tokio::test]
async fn broken_release_is_rejected_and_every_robot_stays_on_the_old_one() {
    let op = spawn("operator", &[], false).await;
    let r1 = spawn("r1", &[&op.addr], true).await;
    let r2 = spawn("r2", &[&op.addr], true).await;

    let good = bundle("v1", true);
    let gh = publish(&op, &good).await;
    desire(&op, &gh, None).await;
    wait_until("both on v1", || on(&r1, &gh) && on(&r2, &gh)).await;

    let bad = bundle("v2-broken", false);
    let bh = publish(&op, &bad).await;
    desire(&op, &bh, None).await;

    wait_until("both reject v2", || {
        r1.node().own().last_failed.as_deref() == Some(bh.as_str())
            && r2.node().own().last_failed.as_deref() == Some(bh.as_str())
    })
    .await;

    for r in [&r1, &r2] {
        let own = r.node().own();
        assert_eq!(
            own.current_release.as_deref(),
            Some(gh.as_str()),
            "must still run v1"
        );
        assert_eq!(own.health, Health::Healthy);
        assert_eq!(r.node().store.current_hash().as_deref(), Some(gh.as_str()));
        assert!(
            !r.node().store.release_dir(&bh).exists(),
            "failed release must be cleaned up"
        );
        assert!(
            own.detail.contains("sensor driver missing"),
            "reason is reported: {}",
            own.detail
        );
    }
    shutdown(vec![op, r1, r2]).await;
}

#[tokio::test]
async fn failed_canary_stops_the_rollout_and_a_good_one_goes_through() {
    let op = spawn("operator", &[], false).await;
    let r1 = spawn("r1", &[&op.addr], true).await;
    let r2 = spawn("r2", &[&op.addr], true).await;
    let r3 = spawn("r3", &[&op.addr], true).await;

    let good = bundle("v1", true);
    let gh = publish(&op, &good).await;
    desire(&op, &gh, None).await;
    wait_until("all on v1", || on(&r1, &gh) && on(&r2, &gh) && on(&r3, &gh)).await;

    // broken release, r1 is the canary
    let bad = bundle("v2-broken", false);
    let bh = publish(&op, &bad).await;
    desire(&op, &bh, Some("r1")).await;
    wait_until("canary r1 rejects", || {
        r1.node().own().last_failed.as_deref() == Some(bh.as_str())
    })
    .await;
    wait_until("r2 and r3 report the halt", || {
        has_event(&r2, "halted") && has_event(&r3, "halted")
    })
    .await;
    for r in [&r2, &r3] {
        let own = r.node().own();
        assert_eq!(
            own.last_failed, None,
            "r2/r3 must never even have tried the broken release"
        );
        assert_eq!(own.current_release.as_deref(), Some(gh.as_str()));
    }

    // a fixed release, canary again: r1 first, then the rest follow
    let fixed = bundle("v3", true);
    let fh = publish(&op, &fixed).await;
    desire(&op, &fh, Some("r1")).await;
    wait_until("everyone on v3", || {
        on(&r1, &fh) && on(&r2, &fh) && on(&r3, &fh)
    })
    .await;

    shutdown(vec![op, r1, r2, r3]).await;
}

#[tokio::test]
async fn release_spreads_peer_to_peer_without_the_operator() {
    // r1 <- r2 <- r3 form a chain; nobody knows the whole fleet up front and
    // r1 plays operator, so r3 can only get the bundle through its neighbours.
    let r1 = spawn("r1", &[], true).await;
    let r2 = spawn("r2", &[&r1.addr], true).await;
    let r3 = spawn("r3", &[&r2.addr], true).await;

    let v1 = bundle("v1", true);
    let h = publish(&r1, &v1).await;
    desire(&r1, &h, None).await;

    wait_until("chain converges", || {
        on(&r1, &h) && on(&r2, &h) && on(&r3, &h)
    })
    .await;
    assert!(
        has_event(&r3, "fetched release"),
        "r3 must have downloaded the bundle from a peer"
    );
    // discovery: r1 never had r3 in its bootstrap list but learned about it through gossip
    wait_until("r1 discovers r3", || {
        r1.node().snapshot().robots.contains_key("r3")
    })
    .await;
    assert!(r1.node().known_peers().contains(&r3.addr));

    shutdown(vec![r1, r2, r3]).await;
}

#[tokio::test]
async fn busy_robot_postpones_the_update_until_it_is_idle() {
    let op = spawn("operator", &[], false).await;
    let r1 = spawn("r1", &[&op.addr], true).await;
    let r2 = spawn("r2", &[&op.addr], true).await;

    let v1 = bundle("v1", true);
    let h1 = publish(&op, &v1).await;
    desire(&op, &h1, None).await;
    wait_until("both on v1", || on(&r1, &h1) && on(&r2, &h1)).await;

    r1.node().mutate_own(|o| o.busy = true);
    let v2 = bundle("v2", true);
    let h2 = publish(&op, &v2).await;
    desire(&op, &h2, None).await;

    wait_until("r2 updates", || on(&r2, &h2)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        r1.node().own().current_release.as_deref(),
        Some(h1.as_str()),
        "busy robot must wait"
    );

    r1.node().mutate_own(|o| o.busy = false);
    r1.node().notify.notify_one();
    wait_until("r1 updates once idle", || on(&r1, &h2)).await;

    shutdown(vec![op, r1, r2]).await;
}

#[tokio::test]
async fn server_refuses_unsafe_bundles_and_unknown_releases() {
    let a = spawn("a", &[], false).await;
    let c = reqwest::Client::new();

    let mut evil = bundle("evil", true);
    evil.files.insert("../escape.txt".into(), "x".into());
    let r = c
        .post(format!("{}/bundles", a.addr))
        .json(&evil)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    let r = c
        .get(format!("{}/bundles/{}", a.addr, "0".repeat(64)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    let r = c
        .post(format!("{}/desired", a.addr))
        .json(&serde_json::json!({"release_hash": "0".repeat(64)}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        400,
        "cannot point the fleet at a release nobody has"
    );

    shutdown(vec![a]).await;
}

#[tokio::test]
async fn restarted_robot_recovers_its_release_and_its_state_wins_again() {
    let op = spawn("operator", &[], false).await;
    let r1 = spawn("r1", &[&op.addr], false).await;
    let v1 = bundle("v1", true);
    let h = publish(&op, &v1).await;
    desire(&op, &h, None).await;
    wait_until("r1 on v1", || on(&r1, &h)).await;

    // "reboot" r1 on the same data directory
    let data_dir = r1._dir.path().to_path_buf();
    let old_counter = r1.node().own().counter;
    r1.run.shutdown().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let cfg = Config {
        id: "r1".into(),
        addr,
        data_dir,
        peers: vec![op.addr.clone()],
        pixi: PixiConfig {
            bin: fake_pixi(),
            install_timeout: Duration::from_secs(20),
            health_timeout: Duration::from_secs(10),
        },
        run_app: false,
        operator_only: false,
        gossip_interval: Duration::from_millis(100),
        reconcile_interval: Duration::from_millis(100),
    };
    let again = agent::start(cfg, listener).unwrap();

    wait_until("r1 healthy again after restart", || {
        let o = again.node.own();
        o.current_release.as_deref() == Some(h.as_str()) && o.health == Health::Healthy
    })
    .await;
    // its counter jumped past what peers remembered, so the operator sees the fresh entry
    wait_until("operator sees the new r1 entry", || {
        op.node()
            .snapshot()
            .robots
            .get("r1")
            .map(|r| r.counter > old_counter && r.health == Health::Healthy)
            .unwrap_or(false)
    })
    .await;

    again.shutdown().await;
    op.run.shutdown().await;
}
