"""A deliberately broken release: simulates a missing sensor driver."""
import sys

print("healthcheck: lidar driver not found", file=sys.stderr)
sys.exit(1)
