"""Stand-in for the robot software: prints a heartbeat so you can see which release runs."""
import time

VERSION = "v3"
print(f"robot app {VERSION} started", flush=True)
while True:
    time.sleep(2)
    print(f"robot app {VERSION} heartbeat", flush=True)
