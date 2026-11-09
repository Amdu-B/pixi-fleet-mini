# A real ROS 2 release (RoboStack)

`sample-robot-app/releases/ros2-jazzy/` is a complete release: ROS 2 Jazzy (`ros-base` plus the demo nodes) from [RoboStack](https://robostack.github.io/GettingStarted), locked for `linux-64`.

```toml
[workspace]
name = "robot-ros2"
channels = [
  "https://conda.anaconda.org/robostack-jazzy",
  "https://conda.anaconda.org/conda-forge",
]
platforms = ["linux-64"]

[dependencies]
ros-jazzy-ros-base = "*"
ros-jazzy-demo-nodes-cpp = "*"

[tasks]
healthcheck = "python healthcheck.py"        # can rclpy start a node?
start = "ros2 run demo_nodes_cpp talker"
```

Run it: `./scripts/demo-ros2.sh` (about 320 MB on the first run, a few seconds afterwards). The script rolls it out to two robots and prints the `talker` output from each.

## Notes

* **Channels.** RoboStack's own getting-started page points at `https://prefix.dev/robostack-jazzy`. This release was locked against the `conda.anaconda.org` mirrors because that is what was reachable where it was built. To use the prefix.dev channel, change the channel line and run `pixi lock`; the rest is unchanged.
* **Other distros.** Replace `jazzy` in the channel and package names (RoboStack publishes several distros; check the page for the current list and for the odd package name per distro).
* **Platforms.** Add `linux-aarch64` for ARM robots and re-lock. The sample Python releases already include it.
* **A better health check.** The sample one only proves `rclpy` can create a node. On a real robot, check that drivers are present, expected topics appear, disk space is sufficient, and so on. Exit code 0 means healthy.
* **Your own workspace.** Put your ROS 2 packages in the release directory (text files only for now) and call them from the `start` task.
