#!/usr/bin/env python3
"""Read-only device check for the sideload app's LAN multicast lifecycle.

Run with --expect held while Codux is visible, then --expect released after
leaving it, and held again after returning. Does not start/stop/install apps.
This checks Android prerequisites, not successful mDNS discovery or NAT traversal.
"""

import argparse
from pathlib import Path
import re
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--expect", choices=("held", "released"), required=True)
    args = parser.parse_args()
    adb = Path("/Users/mac/Library/Android/sdk/platform-tools/adb")
    package = "com.duxweb.codux.sideload"

    def shell(*command):
        return subprocess.check_output(
            [str(adb), "-s", args.serial, "shell", *command],
            text=True,
            stderr=subprocess.PIPE,
            timeout=15,
        )

    packages = shell("pm", "list", "packages", "-U", package)
    uid = re.search(rf"^package:{re.escape(package)} uid:(\d+)\s*$", packages, re.M)
    if uid is None:
        raise RuntimeError("sideload package UID unavailable")
    permissions = shell("dumpsys", "package", package)
    if not re.search(
        r"android\.permission\.CHANGE_WIFI_MULTICAST_STATE:\s+granted=true",
        permissions,
    ):
        raise RuntimeError("installed app lacks CHANGE_WIFI_MULTICAST_STATE")
    wifi = shell("dumpsys", "wifi")
    lines = wifi.splitlines()
    try:
        start = lines.index("Multicast Locks held:") + 1
    except ValueError:
        raise RuntimeError("device does not expose multicast lock state")
    locks = []
    for line in lines[start:]:
        if not line.strip():
            break
        locks.append(line)
    held = bool(re.search(rf"\buid[= :]+{uid[1]}\b", "\n".join(locks), re.I))
    if held != (args.expect == "held"):
        raise RuntimeError(
            f"expected multicast lock {args.expect}; "
            f"UID {uid[1]} lock is {'held' if held else 'released'}"
        )
    print(f"PASS {package}: multicast permission granted, lock {args.expect}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        sys.exit(1)
