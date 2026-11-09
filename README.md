# pixi-fleet-mini

A small, working prototype of **fleet management for robots built on [Pixi](https://pixi.sh)**.

I describe what the fleet should run (a Pixi environment, pinned by a lockfile). A Rust agent on every robot makes that true: it fetches the release from a neighbour, installs it, health-checks it, and only then switches to it atomically. If the health check fails, the robot never leaves its working release. Robots share state peer to peer, so a rollout keeps spreading even when the operator is gone.

```
operator ──publish/desire──▶ robot1 ◀──gossip──▶ robot2 ◀──gossip──▶ robot3
                               │                    │                   │
                      fetch bundle from any peer that has it (hash-verified)
                               │                    │                   │
                  install ▸ health check ▸ atomic switch ▸ restart app  (or roll back)
```

This is a learning and portfolio project, not a product. [What it does not do](#limitations) is listed honestly below.

## What it demonstrates

| Idea | How it is done here |
|---|---|
| Declarative desired state | One "desired release" replicated across the fleet; each robot reconciles toward it (a feedback loop: setpoint, measurement, controller). |
| Reproducible releases | A release is a `pixi.toml` + `pixi.lock` (+ app files), identified by the SHA-256 of its contents. Installed with `pixi install --locked`. |
| Atomic updates, automatic rollback | The new release is installed *next to* the old one; `current` is a symlink flipped with one `rename(2)`. Failed health check: the new directory is deleted and nothing else changes. |
| No central server | Robots gossip their state (a last-writer-wins CRDT map) and download releases from each other. Discovery works from a single known address. |
| Staged rollouts | A canary robot goes first; the others proceed only after it reports healthy, and stop if it rejects the release. Decided locally on each robot from replicated state. |
| Safety gate | A robot marked *busy* postpones its update until it is idle. |
| Operator UI | A single dependency-free web page served by every agent: fleet table, rollout form, event log. |

## Quick start (no Docker)

You need [Pixi](https://pixi.sh) and Rust (`rustup`). The first run downloads Python from conda-forge.

```bash
git clone <this repo> && cd pixi-fleet-mini
./scripts/demo-local.sh
```

The script starts an operator and three robots as separate processes and plays four scenarios:

1. roll out `v1-good` to everyone
2. roll out `v2-good` with `robot1` as canary
3. roll out a **broken** `v3-broken`: the canary rejects it and rolls back, the others never try it
4. **kill the operator**, roll out `v4-good` through `robot1`: it spreads peer to peer

Expected output (abridged, from a real run):

```
== 3/4  roll out a BROKEN v3 with robot1 as canary
ROBOT        RELEASE   HEALTH    ONLINE  DETAIL
robot1       54424ab4  healthy   yes     rejected 8b4acaa4, rolled back: health check failed (exit status: 1): ... lidar driver not found
robot2       54424ab4  healthy   yes     running 54424ab4
robot3       54424ab4  healthy   yes     running 54424ab4
   robot2: rollout of 8b4acaa4 halted: canary robot1 rejected it
```

A **real ROS 2** variant (RoboStack, Jazzy, about 320 MB on first run): `./scripts/demo-ros2.sh`.

## Run it by hand

```bash
cd agent && cargo build --release && cd ..
AGENT=agent/target/release/agent

$AGENT run --id operator --operator --listen 127.0.0.1:8000 --data-dir /tmp/fleet/operator &
$AGENT run --id robot1 --listen 127.0.0.1:8001 --data-dir /tmp/fleet/robot1 --peer http://127.0.0.1:8000 &
$AGENT run --id robot2 --listen 127.0.0.1:8002 --data-dir /tmp/fleet/robot2 --peer http://127.0.0.1:8000 &

$AGENT rollout sample-robot-app/releases/v1-good --to http://127.0.0.1:8000     # publish + desire
$AGENT status --to http://127.0.0.1:8000
```

Open <http://127.0.0.1:8000> for the operator UI. Every agent serves it, and each shows the fleet from its own point of view.

### CLI

| Command | Purpose |
|---|---|
| `agent run --id ID [--peer URL]… [--operator] [--no-app]` | run a node |
| `agent publish DIR --to URL` | pack a release directory, store it on a node, print its hash |
| `agent desire --to URL --hash H [--canary ROBOT]` | set the desired release |
| `agent rollout DIR --to URL [--canary ROBOT]` | publish + desire in one step |
| `agent busy on\|off --to URL` | safety gate for the robot behind the URL |
| `agent status --to URL` | fleet table |
| `agent wait --to URL --hash H --robots a,b [--rejected]` | block until the fleet reached a state (scripts/CI) |
| `agent hash DIR` | content hash of a release directory |

### Docker

```bash
docker compose up --build
docker compose exec operator agent rollout /releases/v1-good --to http://localhost:8000
```

> **Honest note:** the Docker files were written but *not built or run* where this project was developed (no Docker daemon was available). Everything they wrap was tested; the containers themselves were not. If something is off, `scripts/demo-local.sh` shows the intended behaviour.

## Writing a release

A release is any directory that is a valid Pixi project with a lockfile and two tasks:

```toml
[tasks]
healthcheck = "python healthcheck.py"   # exit 0 = healthy; run BEFORE the release goes live
start       = "python app.py"           # the robot application; run AFTER the switch
```

`sample-robot-app/releases/` has four small ones and `ros2-jazzy` is a real ROS 2 environment. Details: [docs/ros2-release.md](docs/ros2-release.md).

## Tests

```bash
cd agent && cargo test
```

* **12 unit tests**: hashing, path-traversal rejection, the atomic switch and cleanup, bundle corruption detection, and a randomised property test (2000 cases) that the state merge is commutative, associative and idempotent.
* **7 end-to-end tests**: several agents in one process, real HTTP, a fake `pixi` so they run in about a second. They cover convergence, rejection and rollback, canary halt then recovery, peer-to-peer spread through a chain with discovery, the busy gate, refusal of unsafe input, and restart recovery.
* **Demo scripts**: the same scenarios against **real Pixi and real conda-forge/RoboStack packages**.

## Project layout

```
agent/                 Rust crate: library + `agent` binary
  src/release.rs         content-addressed bundles, path safety
  src/state.rs           replicated fleet state (LWW CRDT) + canary rules
  src/store.rs           on-disk layout, atomic symlink switch
  src/installer.rs       `pixi install --locked`, `pixi run healthcheck`
  src/supervisor.rs      runs/restarts the robot application
  src/node.rs            shared state of one agent, event log, liveness
  src/gossip.rs          peer-to-peer exchange and discovery
  src/reconcile.rs       the control loop: converge or roll back
  src/server.rs          HTTP API for peers and the UI
  src/client.rs, main.rs CLI
  ui/index.html          operator UI (no framework, no build step)
  tests/                 end-to-end tests + fake pixi
sample-robot-app/      example releases (v1..v4, ros2-jazzy)
scripts/               demo-local.sh, demo-ros2.sh
docker/, docker-compose.yml
docs/                  ARCHITECTURE.md, ros2-release.md, pixi-feedback.md, report.pdf
```

How it works in depth: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The write-up with results is [docs/report.pdf](docs/report.pdf).

## Limitations

Deliberately out of scope, and what I would do next:

* **No authentication or signing.** Anyone who can reach an agent can set the desired release. Next: signed releases (hash + signature checked by the robot), mTLS between peers.
* **Peers share the release *definition*, not the conda packages.** `pixi install` still downloads packages from the channel. Next: a peer-served package cache or `pixi-pack` bundles, so robots work with no internet at all.
* **Full-state gossip to every peer** each tick. Fine for dozens of robots. Next: random-subset gossip with deltas.
* **Last-writer-wins on `desired`.** Two operators writing at the same moment: the higher counter wins, the other is silently overwritten. Fine for one operator at a time, but not a real conflict policy.
* **Liveness is a heartbeat counter**, not a failure detector; clocks are not used for ordering.
* **The supervisor does not restart a crashed app** and has no watchdog. The health check runs before the switch, not continuously.
* **Linux only** (symlinks, process groups). Releases carry text files only.
* **Docker files untested** (see above); real hardware untested; ROS 2 demo uses `talker` only, with no robot hardware.

## License

MIT
