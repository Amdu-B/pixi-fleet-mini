# Pixi / RoboStack: notes from building this

These are observations from building this project, written down so they can feed the "how I used Pixi, and what I would improve" section of an application. They are *my observations from one build*, not a survey.

## What worked well

* **`--locked` is the right primitive for a fleet.** `pixi install --locked` fails if the lockfile does not match the manifest, which is exactly the guarantee a deploy agent wants: no solving on the robot, every robot gets the same packages.
* **Install time is small.** Python releases lock in under a second; a ROS 2 Jazzy `ros-base` environment (about 420 packages, 320 MB) installed from a lockfile in about 15 seconds on a machine with a fast link.
* **`pixi run <task>` is a good contract between agent and release.** Making `healthcheck` and `start` ordinary Pixi tasks meant the agent needed no knowledge of what a release contains.
* **Lockfiles cover several platforms at once** (`linux-64` and `linux-aarch64` in one file), which suits mixed x86/ARM fleets.

## Friction I hit

* **No supported way to pre-fetch for offline use inside the same workflow.** A robot with no internet cannot `pixi install --locked`, even if a neighbour has every package. I ended up sharing only the release *definition* between peers. A peer-served package cache (or first-class `pixi-pack` integration) would make the "works without a stable connection" story complete.
* **Task output goes into error text.** `pixi run` prints a `Pixi task (healthcheck): ...` header line. When a health check fails, that header ends up inside the failure reason the agent reports. A quiet flag or structured exit information would make failures easier to report cleanly.
* **A health-check convention would help fleets.** I defined `healthcheck` and `start` myself. A documented convention (or a manifest field) for "is this environment healthy" would let tooling like this rely on it.
* **Channel naming for RoboStack.** The docs point at `prefix.dev/robostack-<distro>` while mirrors exist elsewhere; I had to know which was reachable from where I built. A short note on mirrors and how to switch (and re-lock) would save time.
* **Locking is a separate step from editing.** Easy to forget `pixi lock` after changing a manifest; `--locked` catches it, but only at deploy time on the robot. A pre-publish check (`agent publish` could run `pixi lock --check`, which exists) would catch it earlier. I did not implement that here.

## Things I would explore upstream

* A `pixi` subcommand that reports "is this directory's environment installed and consistent with the lockfile?" in machine-readable form, for agents.
* Atomic environment switching as a library feature: the agent does it with directories and a symlink; Pixi knows more about what is safe to share between releases (hard-linked package caches).

