#!/usr/bin/env python3
"""Local macOS delivery. Versions come from source; bump them before building."""

import argparse
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SDK = Path.home() / "Library/Android/sdk"
ADB = SDK / "platform-tools/adb"
DOWNLOADS = Path.home() / "Downloads"


def run(*args, cwd=ROOT, env=None, capture=False):
    return (
        subprocess.check_output(args, cwd=cwd, env=env, text=True).strip()
        if capture
        else subprocess.run(args, cwd=cwd, env=env, check=True)
    )


def fingerprint(path):
    value = hashlib.sha256()
    files = (
        sorted(p for p in path.rglob("*") if p.is_file()) if path.is_dir() else [path]
    )
    for item in files:
        if path.is_dir():
            value.update(str(item.relative_to(path)).encode() + b"\0")
            value.update(str(item.stat().st_mode & 0o777).encode() + b"\0")
        with item.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                value.update(block)
    return value.hexdigest()


def deliver(source, name, directory=DOWNLOADS):
    directory.mkdir(parents=True, exist_ok=True)
    digest = fingerprint(source)
    destination = directory / name
    if destination.exists() and fingerprint(destination) != digest:
        destination = destination.with_name(
            f"{destination.stem}-{digest[:12]}{destination.suffix}"
        )
    if not destination.exists():
        if source.is_dir():
            shutil.copytree(source, destination)
        else:
            with source.open("rb") as original, destination.open("xb") as copied:
                shutil.copyfileobj(original, copied)
    if fingerprint(destination) != digest:
        raise RuntimeError(f"Delivery checksum mismatch: {destination}")
    print(f"Delivered: {destination}\nSHA-256: {digest}", flush=True)
    return destination


def cargo_version(product):
    return re.search(
        r'^version = "([^"]+)"', (ROOT / f"apps/{product}/Cargo.toml").read_text(), re.M
    )[1]


def signer(apk, apksigner, env):
    output = run(
        str(apksigner), "verify", "--print-certs", str(apk), env=env, capture=True
    )
    certificates = re.findall(
        r"^Signer #\d+ certificate SHA-256 digest: (\w+)$", output, re.M
    )
    if not certificates:
        raise RuntimeError("APK has no verified signer")
    return certificates


def require_same_signer(new_apk, installed_apk, apksigner, env):
    if signer(new_apk, apksigner, env) != signer(installed_apk, apksigner, env):
        raise RuntimeError(
            "Installed sideload signer differs; app and data were not replaced"
        )


def android(serial):
    mobile = ROOT / "apps/mobile"
    version = re.search(
        r"^version: (\S+)", (mobile / "pubspec.yaml").read_text(), re.M
    )[1]
    name, code = version.split("+")
    properties = dict(
        line.split("=", 1)
        for line in (mobile / "android/local.properties").read_text().splitlines()
        if "=" in line
    )
    flutter = Path(properties["flutter.sdk"]) / "bin/flutter"
    env = dict(
        os.environ,
        JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home",
    )
    build_tools = max(
        (
            p
            for p in (SDK / "build-tools").iterdir()
            if re.fullmatch(r"\d+\.\d+\.\d+", p.name)
        ),
        key=lambda p: tuple(map(int, p.name.split("."))),
    )
    run(
        str(flutter),
        "build",
        "apk",
        "--release",
        "-Pcodux.appIdSuffix=.sideload",
        cwd=mobile,
        env=env,
    )
    apk = mobile / "build/app/outputs/flutter-apk/app-release.apk"
    badging = run(str(build_tools / "aapt"), "dump", "badging", str(apk), capture=True)
    expected = f"package: name='com.duxweb.codux.sideload' versionCode='{code}' versionName='{name}.sideload'"
    if not badging.startswith(expected):
        raise RuntimeError("APK package/version differs from sideload source version")
    signer(apk, build_tools / "apksigner", env)
    delivered = deliver(apk, f"codux-android-{version}-sideload-arm64.apk")
    if serial:
        with tempfile.TemporaryDirectory(prefix="codux-installed-") as temporary:
            paths = run(
                str(ADB),
                "-s",
                serial,
                "shell",
                "pm",
                "path",
                "com.duxweb.codux.sideload",
                capture=True,
            ).splitlines()
            base = next(
                (
                    line.removeprefix("package:")
                    for line in paths
                    if line.endswith("/base.apk")
                ),
                None,
            )
            if not base:
                raise RuntimeError(
                    "Existing sideload app not found; no installation performed"
                )
            installed = Path(temporary) / "base.apk"
            run(str(ADB), "-s", serial, "pull", base, str(installed))
            require_same_signer(delivered, installed, build_tools / "apksigner", env)
            if fingerprint(installed) == fingerprint(delivered):
                print(
                    "Identical sideload APK already installed; no replacement needed."
                )
                return
        run(str(ADB), "-s", serial, "install", "-r", str(delivered))
        paths = run(
            str(ADB),
            "-s",
            serial,
            "shell",
            "pm",
            "path",
            "com.duxweb.codux.sideload",
            capture=True,
        ).splitlines()
        base = next(
            line.removeprefix("package:")
            for line in paths
            if line.endswith("/base.apk")
        )
        actual = run(
            str(ADB), "-s", serial, "shell", "sha256sum", base, capture=True
        ).split()[0]
        if actual != fingerprint(delivered):
            raise RuntimeError("Installed APK differs from delivered APK")
        print("Verified installed sideload APK; existing app data retained.")


def desktop():
    version = cargo_version("desktop")
    run("cargo", "build", "--locked", "--release", "-p", "codux")
    with tempfile.TemporaryDirectory(prefix="codux-desktop-") as temporary:
        env = dict(
            os.environ,
            RELEASE_STAGE_DIR=os.path.relpath(temporary, ROOT),
            RELEASE_BUILD_ID="local",
            RELEASE_WRITE_STABLE_ALIAS="false",
            MACOS_SIGNING_IDENTITY="-",
        )
        run("node", "apps/desktop/scripts/release/package-gpui.mjs", env=env)
        stage = Path(temporary) / "local"
        (archive,) = stage.glob("*-updater.app.tar.gz")
        run("tar", "-xzf", str(archive), "-C", str(stage))
        delivered = deliver(stage / "Codux.app", f"Codux-{version}.app")
        run("codesign", "--verify", "--deep", "--strict", str(delivered))
        actual = run(str(delivered / "Contents/MacOS/Codux"), "--version", capture=True)
        if actual != f"codux {version}":
            raise RuntimeError("Delivered desktop version differs from source")
        print(actual)


def headless(host):
    if host.startswith("-") or not re.fullmatch(r"[\w.@:-]+", host):
        raise ValueError("Expected SSH host or user@host")
    run("cargo", "build", "--locked", "--release", "-p", "codux-agent")
    binary = ROOT / "target/release/codux-agent"
    ssh = ("ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", host)
    temporary = run(*ssh, "mktemp -d /tmp/codux-delivery.XXXXXXXX", capture=True)
    if not re.fullmatch(r"/tmp/codux-delivery\.[A-Za-z0-9]+", temporary):
        raise RuntimeError("Unexpected remote staging directory")
    try:
        run("scp", "-q", str(binary), f"{host}:{temporary}/codux-agent")
        run(
            "scp",
            "-q",
            str(ROOT / "tools/deploy-headless.py"),
            f"{host}:{temporary}/deploy.py",
        )
        command = shlex.join(
            [
                "/usr/bin/python3",
                f"{temporary}/deploy.py",
                f"{temporary}/codux-agent",
                cargo_version("agent"),
                fingerprint(binary),
            ]
        )
        run(*ssh, command)
    finally:
        run(
            *ssh,
            f"rm -f {temporary}/codux-agent {temporary}/deploy.py && rmdir {temporary}",
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="product", required=True)
    commands.add_parser(
        "android",
        help="Build sideload APK into Downloads; optionally upgrade existing sideload",
    ).add_argument("--serial")
    commands.add_parser("desktop", help="Build signed local macOS .app into Downloads")
    commands.add_parser(
        "headless", help="Build and upgrade existing macOS launchd host"
    ).add_argument("host")
    args = parser.parse_args()
    if args.product == "android":
        android(args.serial)
    elif args.product == "desktop":
        desktop()
    else:
        headless(args.host)
