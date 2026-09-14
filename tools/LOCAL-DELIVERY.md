# Local build and delivery

Run from the repository on macOS. Bump each changed product's source version
before building; these commands read existing versions and never auto-bump them.
They do not commit, push, publish, change TUN settings, or clear application data.

```sh
# Android: build sideload APK into ~/Downloads.
python3 tools/local-delivery.py android

# Build and upgrade the existing sideload app, preserving data.
python3 tools/local-delivery.py android --serial AD7B024702000799

# Desktop: build and locally sign .app into ~/Downloads.
python3 tools/local-delivery.py desktop

# Headless: build, back up, replace, restart and verify existing macOS service.
python3 tools/local-delivery.py headless mac@10.10.10.18

# Local regression checks; no device or host mutation.
python3 -m unittest discover -s tools -p 'test_local_delivery.py'
```

Android always targets `com.duxweb.codux.sideload`, using Android Studio's ADB
at `~/Library/Android/sdk/platform-tools/adb`. Flutter comes from this project's
`android/local.properties`; Java comes from Android Studio. Installation requires
an existing sideload package with the same verified signer. The installed APK's
hash must match the delivered file. Launch the app in the desired pane afterward;
the script does not rearrange split-screen tasks.

For Android LAN discovery changes, check the visible sideload app with
`python3 tools/check_android_lan_discovery.py --serial AD7B024702000799 --expect held`.
After hiding it, use `--expect released`; after returning, use `--expect held`
again. This read-only check verifies the installed multicast permission and the
app UID's Wi-Fi multicast lock, not successful mDNS discovery or NAT traversal.

Desktop reuses `apps/desktop/scripts/release/package-gpui.mjs`. The resulting app
has a local ad-hoc signature, not a notarized public-release signature. Final
copies are verified. Existing different same-name downloads are preserved; new
copies receive a content-hash suffix.

Headless requires macOS on both machines, matching CPU architectures, SSH access,
and an existing `com.codux.host` user LaunchAgent. Its plist owns the executable
and data paths. The remote helper validates hash, version and signature before
replacement, preserves a hash-named binary backup, and verifies the new process's
executable inode, published status, configuration, pairings and stable identity.
An already-active identical binary skips replacement and restart. Restart ends
the old host process and its terminal sessions. On failure, it reports an error
and retains the backup; it does not silently roll back or change configuration.

Prerequisites: existing Rust, Node, Flutter and Android SDK toolchains. Packaging
uses the repository's existing DMG/updater tooling. This local workflow does not
change the GitHub release or public installer paths.
