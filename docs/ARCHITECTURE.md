# Architecture

## Components

Every node runs the same binary. A node is a *robot* (installs releases and runs the application) or an *operator* (`--operator`: stores releases, sets the desired state, installs nothing). Any robot can also act as operator through the CLI or UI, which is what makes the operator optional.

```
                 ┌────────────────────── one agent ──────────────────────┐
 peers / UI ───▶ │ HTTP server ──┐                                       │
                 │               ▼                                       │
                 │          Node (shared state)                          │
                 │   fleet view · own status · events · store            │
                 │      ▲                    ▲                           │
                 │  gossip loop         reconcile loop ──▶ pixi CLI      │
                 │  (every 2 s)         (on change/1 s)    install/check │
                 │                            │                          │
                 │                            ▼                          │
                 │                      supervisor ──▶ `pixi run start`  │
                 └───────────────────────────────────────────────────────┘
```

## Release = content-addressed bundle

A bundle is a map `relative path -> UTF-8 text` containing at least `pixi.toml` and `pixi.lock`. Its id is the SHA-256 over `(len(path), path, len(content), content)` for every file in sorted path order. The length prefixes make `("ab","c")` and `("a","bc")` hash differently. The label is not hashed.

Consequences:

* A bundle received from *any* peer can be verified: recompute the hash, compare with what was asked for. A malicious or buggy peer cannot substitute different content.
* Hashes are used as directory names, so they are checked to be exactly 64 lowercase hex characters first. That rules out `../` tricks. Paths *inside* a bundle are checked too (no absolute paths, no `..`).
* Bundles are capped at 4 MiB.

## On-disk layout

```
<data-dir>/
  bundles/<hash>.json      release definitions (what peers exchange)
  releases/<hash>/         unpacked + installed (.pixi inside)
  current  -> releases/<hash>
  previous -> releases/<hash>
  logs/app.log
```

`current` is replaced by creating a temporary symlink and `rename(2)`-ing it over `current`. Rename is atomic on POSIX, so at every instant `current` names one complete release, even if the machine loses power during an update. After a successful switch, releases other than `current` and `previous` are deleted.

## The reconcile loop (the heart)

```
desired = fleet.desired
if desired == mine                         → done
if already tried this desired change       → wait for a new one
decision = rollout_decision(canary rules)  → Apply | WaitCanary | Halted
if robot is busy                           → postpone
bundle = local cache or fetch from peers (hash verified)
materialize into releases/<hash>/
pixi install --locked
pixi run healthcheck
  ok   → atomic switch, restart app, update status = healthy
  fail → delete releases/<hash>/, status = healthy on the OLD release,
         last_failed = hash, reason in `detail`
```

The control-loop view: *setpoint* is the desired release, *measurement* is the current release, *controller* is this loop, *actuator* is the install + switch. The rollback is not a separate step. The old release is never touched until the new one has proven itself, so "rolling back" is simply "not switching".

"Already tried" matters: without it a rejected release would be retried every second. It is keyed by `(hash, desired counter)`, so an operator re-issuing the same hash deliberately (a new counter) triggers a fresh attempt.

## Replicated state

`FleetState = { robots: map id -> RobotStatus, desired: Desired }`

Both parts are last-writer-wins registers ordered by a counter:

* `robots[id]` is written **only by robot `id`**, which increments its counter on every change (and on every heartbeat).
* `desired` is written by whoever acts as operator; the writer sets `counter = seen + 1`.

`merge(a, b)` keeps, per entry, the one with the higher `(counter, tie-break)`. The tie-break is a deterministic function of the content, so the result does not depend on merge order. The property test checks that the merge is **commutative, associative and idempotent**, which is what makes it safe to gossip in any order, with duplicates and delays.

Two details worth knowing:

* **Restart.** A robot that restarts begins with counter 1 while peers still remember its old, higher counter, so its fresh state would lose every merge. The first time it hears a higher counter for itself it jumps to `theirs + 1`. A node also never accepts anyone else's version of its own entry.
* **Liveness.** `online` is not a timestamp. Each node remembers when it last saw a robot's counter *change*. A robot whose counter stops rising for ~10 s shows as offline. No clock synchronisation is needed.

## Gossip and discovery

Every tick a node posts its full state to each known peer and merges the reply (push-pull in one request). Known peers = bootstrap `--peer` list + every robot address present in the state. A robot started with only the operator's address therefore learns about all others within a gossip round or two.

## Canary rollouts without a coordinator

`desired` may name a canary robot. Each robot decides for itself, using only replicated state:

| Situation | Decision |
|---|---|
| no canary, or I am the canary | apply |
| canary runs the release and is healthy | apply |
| canary recorded `last_failed == release` | **halted**: do not try |
| otherwise | wait |

So a bad release is stopped by one robot's failure, and the others never even install it.

## HTTP API

| Route | Purpose |
|---|---|
| `GET /` | operator UI |
| `GET/POST /state` | read the view / gossip |
| `GET/POST /bundles`, `GET /bundles/{hash}` | list / publish / download |
| `POST /desired`, `POST /busy` | operator actions |
| `GET /events`, `GET /api/fleet` | UI data |

There is no authentication (see Limitations in the README). The UI builds all DOM with `textContent`, so robot ids, labels and error messages from other machines are never interpreted as HTML (tested with a label containing `<b>`).

## Failure modes handled

* Health check fails or times out → rollback; the whole process group of a timed-out command is killed.
* `pixi install` fails (bad lockfile, no network) → rollback, reason reported.
* A peer sends a wrong or oversized bundle → rejected by hash/size check.
* Corrupt local bundle file → detected on read; refetched from peers.
* Crash mid-update → `current` still names a complete release; a half-written `releases/<hash>/` is wiped and rebuilt on the next attempt.
* Restart → the release on disk is health-checked again before being reported healthy; the app is restarted.
* Unreachable peers → ignored per tick, no effect on others.
