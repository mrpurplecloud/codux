#!/usr/bin/env python3
"""Remote helper for local-delivery.py; upgrades an existing macOS launchd host."""

import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def active_binary_matches(binary, pid):
    result = subprocess.run(
        ["/usr/sbin/lsof", "-a", "-p", str(pid), "-d", "txt", "-Fni"],
        text=True,
        capture_output=True,
    )
    if result.returncode:
        return False
    inode = None
    for line in result.stdout.splitlines():
        if line.startswith("i"):
            inode = int(line[1:])
        elif line == "n" + str(binary):
            return inode == binary.stat().st_ino
    return False


def deploy(incoming, version, expected_hash):
    service = Path.home() / "Library/LaunchAgents/com.codux.host.plist"
    with service.open("rb") as stream:
        configuration = plistlib.load(stream)
    arguments = configuration["ProgramArguments"]
    if (
        configuration["Label"] != "com.codux.host"
        or len(arguments) != 2
        or arguments[1] != "start"
    ):
        raise RuntimeError("Expected existing codux start launchd service")
    binary = Path(arguments[0])
    if not binary.is_absolute() or binary.is_symlink() or not binary.is_file():
        raise RuntimeError(
            "Service executable must be an existing absolute regular file"
        )
    data = Path(
        configuration.get("EnvironmentVariables", {}).get(
            "CODUX_AGENT_DATA_DIR", Path.home() / ".codux-agent"
        )
    )
    if digest(incoming) != expected_hash:
        raise RuntimeError("Uploaded binary checksum mismatch")
    output = subprocess.check_output([str(incoming), "version"], text=True)
    if output.splitlines()[0] != f"codux {version}":
        raise RuntimeError("Uploaded binary version mismatch")
    subprocess.run(
        ["/usr/bin/codesign", "--verify", "--strict", str(incoming)], check=True
    )
    protected = {name: digest(data / name) for name in ("codux.toml", "devices.json")}
    previous = json.loads((data / "status.json").read_text())
    old_hash = digest(binary)
    if old_hash == expected_hash and active_binary_matches(binary, previous["pid"]):
        print(f"Already running codux {version}; no replacement or restart needed.")
        return

    backup = binary.with_name(f"{binary.name}.backup-{old_hash[:12]}")
    if backup.is_symlink():
        raise RuntimeError(f"Backup symlink preserved: {backup}")
    if backup.exists():
        if digest(backup) != old_hash:
            raise RuntimeError(f"Unknown backup preserved: {backup}")
    else:
        with binary.open("rb") as source, backup.open("xb") as destination:
            shutil.copyfileobj(source, destination)
        shutil.copystat(binary, backup)
    if digest(backup) != old_hash:
        raise RuntimeError("Backup checksum mismatch")
    # Stage beside the destination so replacement is atomic on this filesystem.
    with tempfile.NamedTemporaryFile(
        dir=binary.parent, prefix=".codux-", delete=False
    ) as staged:
        with incoming.open("rb") as source:
            shutil.copyfileobj(source, staged)
    staged_path = Path(staged.name)
    try:
        staged_path.chmod(0o755)
        if digest(staged_path) != expected_hash or digest(binary) != old_hash:
            raise RuntimeError("Binary changed before replacement; deployment stopped")
        os.replace(staged_path, binary)
    finally:
        staged_path.unlink(missing_ok=True)
    print(f"Backup: {backup}", flush=True)
    target = f"gui/{os.getuid()}/{configuration['Label']}"
    subprocess.run(["/bin/launchctl", "kickstart", "-k", target], check=True)
    # Host startup may spend 12 seconds registering relay before publishing status.
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        try:
            status = json.loads((data / "status.json").read_text())
            if status["pid"] != previous["pid"] and active_binary_matches(
                binary, status["pid"]
            ):
                break
        except (FileNotFoundError, json.JSONDecodeError):
            pass  # Status publication is in progress; the deadline still applies.
        time.sleep(0.5)
    else:
        raise RuntimeError(
            f"New host did not publish live status; backup retained: {backup}"
        )
    for name, original in protected.items():
        if digest(data / name) != original:
            raise RuntimeError(
                f"{name} changed; inspect before claiming data preservation"
            )
    if any(status[key] != previous[key] for key in ("host_id", "node_id")):
        raise RuntimeError("Host identity changed after restart")
    print(f'Running codux {version}, PID {status["pid"]}; executable inode verified.')
    print("Configuration, pairings and host identity unchanged.")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("Usage: deploy-headless.py INCOMING_BINARY VERSION SHA256")
    deploy(Path(sys.argv[1]), sys.argv[2], sys.argv[3])
