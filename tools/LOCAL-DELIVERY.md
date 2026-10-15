# Local build and delivery

Run from the repository on macOS. Bump each changed product's source version
before building; these commands read existing versions and never auto-bump them.
They do not commit, push, publish, change TUN settings, or clear application data.

```sh
# Android: build sideload APK into ~/Downloads.
python3 tools/local-delivery.py android

# Build and upgrade the existing sideload app, preserving data.
python3 tools/local-delivery.py android --serial AD7B024702000799

# Desktop: choose an existing certificate if no local default is saved.
security find-identity -v -p codesigning
export MACOS_SIGNING_IDENTITY="<full certificate name or SHA-1 from the command above>"
# Build certificate-signed .app into ~/Downloads.
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

Desktop reuses `apps/desktop/scripts/release/package-gpui.mjs` with an explicit
Apple-issued app signing identity. `MACOS_SIGNING_IDENTITY` takes precedence over
`APPLE_SIGNING_IDENTITY`; either accepts a full certificate name or SHA-1. Without
either variable, the script reads `~/.config/codux/macos-signing-identity`, a local
text file containing the selected certificate's full name or SHA-1. Save the
selection there once to reuse it in new terminals; it contains no private key and
is not committed. The selected certificate is printed before building. A missing
or expired selected certificate fails; another certificate is never auto-selected.
The certificate and its private key must already be available in the keychain.
Apple Development (including legacy Mac Developer) supports local development;
Developer ID Application is the appropriate identity for distribution outside
the Mac App Store. Missing, ambiguous, invalid and ad-hoc identities stop delivery
before compilation. No certificate is created, imported or silently substituted.

For free local development, sign in to an Apple Account in Xcode Settings,
select its Personal Team and use Manage Certificates to create an Apple
Development certificate. Free provisioning has restrictions; this is not a
Developer ID or notarized distribution. See Apple's [account overview](https://developer.apple.com/help/account/basics/about-your-developer-account)
and [local network privacy guidance](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).

If a new certificate appears under `Matching identities` but not `Valid identities`
in `security find-identity -p codesigning`, check its validity dates and matching
Apple WWDR intermediate certificate. Obtain missing intermediates from
[Apple PKI](https://www.apple.com/certificateauthority/); do not bypass validation
by marking an untrusted certificate as Always Trust.

Keep the same signing identity and bundle ID (`com.duxweb.codux`) across builds.
The script pins the selected certificate's SHA-1 for that build; this is not the
binary's changing `cdhash`. The packager generates the certificate-based code
requirement normally; it does not copy an old ad-hoc signature or override the
designated requirement. Both the staged app and final Downloads copy must verify
against Apple's trust anchor, the expected bundle ID and the selected certificate.
Notarization remains separate and uses the packager's existing Apple credentials.
Existing different same-name downloads are preserved with a content-hash suffix.

Moving from an old ad-hoc app to a certificate-signed app may require a one-time
permission grant. Finish or safely restart tasks launched by the old app: their
permission attribution can outlive its window. Do not run old ad-hoc builds beside
the new app using the same bundle ID, and install upgrades at a stable path such
as `/Applications/Codux.app`. Verify permissions on the actual macOS app after
migration; packaging checks alone do not prove privacy prompts are resolved.

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
