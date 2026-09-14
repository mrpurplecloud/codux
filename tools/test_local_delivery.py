import importlib.util
import json
from pathlib import Path
import plistlib
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
