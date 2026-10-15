import importlib.util
import json
import os
from pathlib import Path
import plistlib
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "delivery", Path(__file__).with_name("local-delivery.py")
)
delivery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(delivery)
remote_spec = importlib.util.spec_from_file_location(
    "remote", Path(__file__).with_name("deploy-headless.py")
)
remote = importlib.util.module_from_spec(remote_spec)
remote_spec.loader.exec_module(remote)


class LocalDeliveryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.signing_config = Path(temporary.name) / "macos-signing-identity"
        config = patch.object(
            delivery, "MACOS_SIGNING_IDENTITY_FILE", self.signing_config, create=True
        )
        config.start()
        self.addCleanup(config.stop)

    def test_macos_reuses_saved_certificate_without_environment(self):
        self.signing_config.write_text("A" * 40 + "\n")
        available = f'  1) {"A" * 40} "Apple Development: Example (USER123456)"\n'
        with patch.dict(os.environ, {}, clear=True), patch.object(
            delivery, "run", return_value=available
        ):
            try:
                selected = delivery.macos_signing_identity()
            except RuntimeError as error:
                self.fail(f"Saved signing identity was not reused: {error}")
        self.assertEqual(selected, "A" * 40)

    def test_macos_explicit_identity_overrides_saved_certificate_without_fallback(self):
        self.signing_config.write_text("A" * 40 + "\n")
        available = f'  1) {"B" * 40} "Apple Development: Example (USER123456)"\n'
        for environment, expected in (
            (
                {
                    "MACOS_SIGNING_IDENTITY": "B" * 40,
                    "APPLE_SIGNING_IDENTITY": "A" * 40,
                },
                "B" * 40,
            ),
            ({"APPLE_SIGNING_IDENTITY": "B" * 40}, "B" * 40),
            ({"MACOS_SIGNING_IDENTITY": "-"}, None),
            ({}, None),
        ):
            with self.subTest(environment=environment), patch.dict(
                os.environ, environment, clear=True
            ), patch.object(delivery, "run", return_value=available):
                if expected is None:
                    with self.assertRaisesRegex(RuntimeError, "Apple-issued"):
                        delivery.macos_signing_identity()
                else:
                    self.assertEqual(delivery.macos_signing_identity(), expected)

    def test_desktop_rejects_missing_or_adhoc_identity_before_build(self):
        for environment in ({}, {"MACOS_SIGNING_IDENTITY": "-"}):
            with self.subTest(environment=environment), patch.dict(
                os.environ, environment, clear=True
            ), patch.object(
                delivery,
                "run",
                side_effect=AssertionError("Build started without a stable identity"),
            ):
                with self.assertRaisesRegex(RuntimeError, "Apple-issued"):
                    delivery.desktop()

    def test_desktop_rejects_unavailable_or_non_app_certificate(self):
        identities = (
            "  1) " + "A" * 40 + ' "Developer ID Installer: Example (TEAM123456)"\n'
            "     1 valid identities found\n"
        )

        def command(*args, **kwargs):
            if args == ("security", "find-identity", "-v", "-p", "codesigning"):
                return identities
            self.fail("Build started without a valid Apple-issued app certificate")

        for identity in ("Developer ID Application: Missing (TEAM123456)", "A" * 40):
            with self.subTest(identity=identity), patch.dict(
                os.environ, {"MACOS_SIGNING_IDENTITY": identity}, clear=True
            ), patch.object(delivery, "run", side_effect=command):
                with self.assertRaisesRegex(RuntimeError, "Apple-issued"):
                    delivery.desktop()

    def test_desktop_preserves_selected_signer_through_packaging_and_delivery(self):
        certificate_hash = "A" * 40
        version = delivery.cargo_version("desktop")
        original_deliver = delivery.deliver
        for variable, identity, selection in (
            (
                "MACOS_SIGNING_IDENTITY",
                "Developer ID Application: Example (TEAM123456)",
                certificate_hash,
            ),
            (
                "APPLE_SIGNING_IDENTITY",
                "Apple Development: Example (USER123456)",
                "Apple Development: Example (USER123456)",
            ),
        ):
            with self.subTest(
                variable=variable
            ), tempfile.TemporaryDirectory() as temporary:
                downloads = Path(temporary) / "Downloads"
                verified = []

                def command(*args, **kwargs):
                    if args == ("security", "find-identity", "-v", "-p", "codesigning"):
                        return f'  1) {certificate_hash} "{identity}"\n     1 valid identities found'
                    if args[0] == "cargo":
                        return
                    if args[0] == "node":
                        environment = kwargs["env"]
                        self.assertEqual(
                            environment["MACOS_SIGNING_IDENTITY"], certificate_hash
                        )
                        stage = (
                            delivery.ROOT / environment["RELEASE_STAGE_DIR"] / "local"
                        )
                        binary = stage / "Codux.app/Contents/MacOS/Codux"
                        binary.parent.mkdir(parents=True)
                        binary.write_text("fixture binary")
                        binary.chmod(0o755)
                        with tarfile.open(
                            stage / "test-updater.app.tar.gz", "w:gz"
                        ) as archive:
                            archive.add(stage / "Codux.app", arcname="Codux.app")
                        return
                    if args[0] == "tar":
                        with tarfile.open(args[2]) as archive:
                            archive.extractall(args[4], filter="data")
                        return
                    if args[0] == "codesign":
                        self.assertIn("--test-requirement", args)
                        requirement = args[args.index("--test-requirement") + 1]
                        self.assertTrue(requirement.startswith("="))
                        self.assertIn('identifier "com.duxweb.codux"', requirement)
                        self.assertIn("anchor apple generic", requirement)
                        self.assertIn(
                            f'certificate leaf = H"{certificate_hash}"', requirement
                        )
                        verified.append(Path(args[-1]))
                        return
                    if args == (
                        str(downloads / f"Codux-{version}.app/Contents/MacOS/Codux"),
                        "--version",
                    ):
                        return f"codux {version}"
                    self.fail(f"Unexpected command: {args}")

                with patch.dict(
                    os.environ, {variable: selection}, clear=True
                ), patch.object(delivery, "run", side_effect=command), patch.object(
                    delivery,
                    "deliver",
                    side_effect=lambda source, name: original_deliver(
                        source, name, downloads
                    ),
                ):
                    delivery.desktop()
                result = downloads / f"Codux-{version}.app"
                self.assertEqual(verified[-1], result)
                self.assertEqual(
                    (result / "Contents/MacOS/Codux").read_text(), "fixture binary"
                )

    def test_repeated_delivery_preserves_unknown_same_name_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "source.apk"
            source.write_bytes(b"new package")
            downloads = root / "Downloads"
            downloads.mkdir()
            existing = downloads / "codux.apk"
            existing.write_bytes(b"user package")
            delivered = delivery.deliver(source, existing.name, downloads)
            self.assertNotEqual(delivered, existing)
            self.assertEqual(existing.read_bytes(), b"user package")
            self.assertEqual(delivered.read_bytes(), source.read_bytes())
            self.assertEqual(
                delivery.deliver(source, existing.name, downloads), delivered
            )

    def test_signer_mismatch_aborts_upgrade(self):
        with patch.object(delivery, "signer", side_effect=[["new"], ["installed"]]):
            with self.assertRaisesRegex(RuntimeError, "signer differs"):
                delivery.require_same_signer(
                    "new.apk", "installed.apk", "apksigner", {}
                )

    def test_app_delivery_preserves_executable_mode_and_detects_resource_changes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            app = root / "source.app"
            app.mkdir()
            binary = app / "Codux"
            binary.write_bytes(b"binary")
            binary.chmod(0o755)
            first = delivery.deliver(app, "Codux.app", root / "Downloads")
            (app / "resource").write_text("updated resource")
            second = delivery.deliver(app, "Codux.app", root / "Downloads")
            self.assertNotEqual(first, second)
            self.assertEqual((second / "Codux").stat().st_mode & 0o777, 0o755)
            self.assertFalse((first / "resource").exists())

    def test_same_installed_binary_still_restarts_when_process_uses_old_inode(self):
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            binary = home / "codux"
            binary.write_bytes(b"new binary")
            incoming = home / "incoming"
            incoming.write_bytes(binary.read_bytes())
            service = home / "Library/LaunchAgents/com.codux.host.plist"
            service.parent.mkdir(parents=True)
            service.write_bytes(
                plistlib.dumps(
                    {
                        "Label": "com.codux.host",
                        "ProgramArguments": [str(binary), "start"],
                    }
                )
            )
            data = home / ".codux-agent"
            data.mkdir()
            for name in ("codux.toml", "devices.json"):
                (data / name).write_text("existing user data")
            status = {"pid": 1, "host_id": "host", "node_id": "node"}
            (data / "status.json").write_text(json.dumps(status))

            def command(args, **kwargs):
                if args[0] == "/bin/launchctl":
                    (data / "status.json").write_text(json.dumps(dict(status, pid=2)))

            with patch.object(remote.Path, "home", return_value=home), patch.object(
                remote, "active_binary_matches", side_effect=[False, True]
            ), patch.object(
                remote.subprocess, "check_output", return_value="codux test-version\n"
            ), patch.object(
                remote.subprocess, "run", side_effect=command
            ) as calls:
                remote.deploy(incoming, "test-version", remote.digest(incoming))
            self.assertTrue(
                any(
                    call.args[0][0] == "/bin/launchctl" for call in calls.call_args_list
                )
            )
            self.assertEqual(json.loads((data / "status.json").read_text())["pid"], 2)
            self.assertEqual((data / "devices.json").read_text(), "existing user data")


if __name__ == "__main__":
    unittest.main()
