"""Health check for the ROS 2 release: can rclpy start a node?

A real robot would also check drivers, topics, hardware, ...
Exit code 0 = healthy.
"""
import rclpy

rclpy.init()
node = rclpy.create_node("healthcheck")
node.destroy_node()
rclpy.shutdown()
print("healthcheck: rclpy ok")
