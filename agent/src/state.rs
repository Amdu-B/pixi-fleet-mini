//! Replicated fleet state.
//!
//! The state is a map of last-writer-wins registers (a simple state-based CRDT):
//!
//! * `robots[id]`  - one entry per robot, written only by that robot.
//! * `desired`     - what the fleet should run, written by whoever acts as operator.
//!
//! Each register carries a counter. On merge the higher counter wins, and a
//! deterministic tie-break makes the outcome independent of merge order. That
//! gives the three CRDT properties (commutative, associative, idempotent), so
//! peers may gossip in any order, any number of times, and still converge.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Health {
    Unknown,
    Updating,
    Healthy,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RobotStatus {
    pub robot_id: String,
    /// Base URL where peers can reach this robot's agent.
    pub addr: String,
    pub current_release: Option<String>,
    pub health: Health,
    /// The most recent release this robot tried and rejected (health check failed).
    pub last_failed: Option<String>,
    /// Safety gate: a busy robot postpones updates.
    pub busy: bool,
    /// Operator-only node: stores releases and sets the desired state, runs no robot software.
    #[serde(default)]
    pub operator: bool,
    pub detail: String,
    pub counter: u64,
}

impl RobotStatus {
    fn order_key(&self) -> (u64, String) {
        (
            self.counter,
            serde_json::to_string(self).unwrap_or_default(),
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Desired {
    pub release_hash: Option<String>,
    /// Optional canary robot: it goes first, the rest follow only if it turns out healthy.
    pub canary: Option<String>,
    pub counter: u64,
    pub set_by: String,
}

impl Desired {
    fn order_key(&self) -> (u64, String, String) {
        (
            self.counter,
            self.set_by.clone(),
            serde_json::to_string(self).unwrap_or_default(),
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetState {
    pub robots: BTreeMap<String, RobotStatus>,
    pub desired: Desired,
}

impl FleetState {
    /// Merge `other` into `self`. Pure function of the two states.
    pub fn merge(&mut self, other: &FleetState) {
        for (id, theirs) in &other.robots {
            let keep_mine = self
                .robots
                .get(id)
                .map(|mine| mine.order_key() >= theirs.order_key())
                .unwrap_or(false);
            if !keep_mine {
                self.robots.insert(id.clone(), theirs.clone());
            }
        }
        if other.desired.order_key() > self.desired.order_key() {
            self.desired = other.desired.clone();
        }
    }
}

/// What a robot should do about the desired release right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Apply,
    /// Waiting for the canary robot to finish (and prove healthy).
    WaitCanary(String),
    /// The canary rejected this release: the rollout is stopped.
    Halted(String),
}

/// Decentralised staged rollout. Every robot evaluates this on its own copy of
/// the replicated state, so no coordinator is needed.
pub fn rollout_decision(fleet: &FleetState, my_id: &str, desired: &Desired) -> Decision {
    let (Some(hash), Some(canary)) = (&desired.release_hash, &desired.canary) else {
        return Decision::Apply;
    };
    if canary == my_id {
        return Decision::Apply;
    }
    match fleet.robots.get(canary) {
        Some(c) if c.last_failed.as_deref() == Some(hash.as_str()) => {
            Decision::Halted(canary.clone())
        }
        Some(c)
            if c.current_release.as_deref() == Some(hash.as_str())
                && c.health == Health::Healthy =>
        {
            Decision::Apply
        }
        _ => Decision::WaitCanary(canary.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn robot(id: &str, counter: u64, rel: Option<&str>) -> RobotStatus {
        RobotStatus {
            robot_id: id.into(),
            addr: format!("http://{id}"),
            current_release: rel.map(String::from),
            health: Health::Healthy,
            last_failed: None,
            busy: false,
            operator: false,
            detail: String::new(),
            counter,
        }
    }

    // Tiny deterministic generator so the property tests need no extra crates.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn random_state(rng: &mut Rng) -> FleetState {
        let mut s = FleetState::default();
        for id in ["a", "b", "c", "d"] {
            if !rng.next().is_multiple_of(3) {
                let rel = ["r1", "r2", "r3"][(rng.next() % 3) as usize];
                // small counter range on purpose, so ties actually happen
                s.robots
                    .insert(id.into(), robot(id, rng.next() % 4, Some(rel)));
            }
        }
        s.desired = Desired {
            release_hash: Some(format!("h{}", rng.next() % 3)),
            canary: None,
            counter: rng.next() % 4,
            set_by: ["op1", "op2"][(rng.next() % 2) as usize].into(),
        };
        s
    }

    fn merged(a: &FleetState, b: &FleetState) -> FleetState {
        let mut x = a.clone();
        x.merge(b);
        x
    }

    #[test]
    fn merge_is_commutative_associative_idempotent() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for _ in 0..2000 {
            let (a, b, c) = (
                random_state(&mut rng),
                random_state(&mut rng),
                random_state(&mut rng),
            );
            assert_eq!(merged(&a, &b), merged(&b, &a), "commutative");
            assert_eq!(
                merged(&merged(&a, &b), &c),
                merged(&a, &merged(&b, &c)),
                "associative"
            );
            assert_eq!(merged(&a, &a), a, "idempotent");
        }
    }

    #[test]
    fn higher_counter_wins() {
        let mut a = FleetState::default();
        a.robots.insert("r".into(), robot("r", 1, Some("old")));
        let mut b = FleetState::default();
        b.robots.insert("r".into(), robot("r", 2, Some("new")));
        a.merge(&b);
        assert_eq!(a.robots["r"].current_release.as_deref(), Some("new"));
        // merging the stale one back changes nothing
        let mut stale = FleetState::default();
        stale.robots.insert("r".into(), robot("r", 1, Some("old")));
        a.merge(&stale);
        assert_eq!(a.robots["r"].current_release.as_deref(), Some("new"));
    }

    fn desired(hash: &str, canary: Option<&str>) -> Desired {
        Desired {
            release_hash: Some(hash.into()),
            canary: canary.map(String::from),
            counter: 1,
            set_by: "op".into(),
        }
    }

    #[test]
    fn rollout_without_canary_applies_immediately() {
        let f = FleetState::default();
        assert_eq!(
            rollout_decision(&f, "x", &desired("h", None)),
            Decision::Apply
        );
    }

    #[test]
    fn canary_goes_first_then_others_follow() {
        let mut f = FleetState::default();
        f.robots.insert("c".into(), robot("c", 1, Some("old")));
        let d = desired("new", Some("c"));
        assert_eq!(rollout_decision(&f, "c", &d), Decision::Apply);
        assert_eq!(
            rollout_decision(&f, "x", &d),
            Decision::WaitCanary("c".into())
        );
        f.robots.insert("c".into(), robot("c", 2, Some("new")));
        assert_eq!(rollout_decision(&f, "x", &d), Decision::Apply);
    }

    #[test]
    fn failed_canary_halts_the_rollout() {
        let mut f = FleetState::default();
        let mut c = robot("c", 3, Some("old"));
        c.last_failed = Some("bad".into());
        f.robots.insert("c".into(), c);
        assert_eq!(
            rollout_decision(&f, "x", &desired("bad", Some("c"))),
            Decision::Halted("c".into())
        );
    }
}
