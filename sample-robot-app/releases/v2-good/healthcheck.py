"""Health check for the sample robot app.

Pixi runs this as `pixi run healthcheck`. Exit code 0 = healthy, anything else
= the agent rejects the release and keeps the robot on its current one.
A real robot would check sensors, drivers, a ROS 2 node, disk space, etc.
"""
import sys

print("healthcheck: python", sys.version.split()[0], "- ok")
sys.exit(0)
