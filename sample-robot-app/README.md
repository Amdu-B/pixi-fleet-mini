# Sample releases

Each directory under `releases/` is one **release**: a complete Pixi project
(`pixi.toml` + `pixi.lock` + the files it needs). The agent packs a directory,
hashes it, and ships it around the fleet.

| release     | what it is                                              |
|-------------|---------------------------------------------------------|
| `v1-good`   | Python 3.12/3.13 from conda-forge, prints a heartbeat   |
| `v2-good`   | same environment, app prints `v2`                       |
| `v3-broken` | health check fails on purpose (simulated missing driver)|
| `v4-good`   | used by the demo to roll out while the operator is down |

The two Pixi tasks every release must provide:

* `healthcheck`: exit code 0 means healthy. Run **before** the release goes live.
* `start`: the robot application. Run by the agent **after** the release goes live.

## Make your own release

```bash
cp -r releases/v1-good releases/my-release
cd releases/my-release
# edit pixi.toml / app.py, then re-lock:
pixi lock
```

The lockfile is mandatory: the agent installs with `pixi install --locked`, so
every robot gets exactly the same packages.

## A ROS 2 release (RoboStack)

See [../docs/ros2-release.md](../docs/ros2-release.md).
