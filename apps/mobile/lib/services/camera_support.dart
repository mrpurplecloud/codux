import 'dart:io';

import 'package:device_info_plus/device_info_plus.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'log_service.dart';

/// Why the camera may or may not be used for scanning.
enum CameraAvailability {
  /// The camera can be started.
  available,

  /// The device reports no camera at all (e-ink readers, kiosk tablets).
  missingHardware,

  /// A previous start never finished — the camera took the process with it.
  blockedAfterCrash,
}

/// Decides whether the QR scanner may touch the camera at all.
///
/// A missing camera is not a catchable Dart error. `mobile_scanner` unwraps the
/// CameraX provider future with `get()` inside a listener that runs on the
/// Android main thread, so on a device without a camera the failure surfaces as
/// an uncaught Java exception and the process dies before any `catch` in Dart
/// can run. The scanner therefore has to know *before* it starts, which is what
/// this class answers.
///
/// Two signals, because neither alone is enough:
///  * `android.hardware.camera.any` — the device's own declaration. Correct on
///    normal phones, absent on camera-less hardware.
///  * a persisted "start in flight" marker — covers hardware that declares a
///    camera it cannot actually open. The marker is written before the start
///    and cleared once the start settles, so a start that killed the process
///    leaves the marker behind and the next launch keeps away from the camera.
///    That turns a repeating crash into a single one.
class CameraSupport {
  CameraSupport();

  static const pendingStartKey = 'codux.mobile.camera_start_pending';
  static const _anyCameraFeature = 'android.hardware.camera.any';

  Future<CameraAvailability>? _availability;

  /// Resolved once per instance; the answer cannot change while the app runs.
  Future<CameraAvailability> resolve() => _availability ??= _detect();

  Future<CameraAvailability> _detect() async {
    // Only Android dies this way, and only Android exposes the feature list
    // cheaply. Elsewhere a missing camera arrives as an ordinary plugin error,
    // which the scanner's own error handling already covers.
    if (!Platform.isAndroid) return CameraAvailability.available;
    try {
      final prefs = await SharedPreferences.getInstance();
      if (prefs.getBool(pendingStartKey) ?? false) {
        CoduxLog.warn(
          '[codux-flutter-camera] previous camera start never finished; keeping the camera off',
        );
        return CameraAvailability.blockedAfterCrash;
      }
      final info = await DeviceInfoPlugin().androidInfo;
      if (info.systemFeatures.contains(_anyCameraFeature)) {
        return CameraAvailability.available;
      }
      CoduxLog.info('[codux-flutter-camera] device reports no camera hardware');
      return CameraAvailability.missingHardware;
    } catch (error) {
      // Pairing by hand still works; gambling on a start that can kill the
      // process does not. Treat an unreadable device as camera-less.
      CoduxLog.warn('[codux-flutter-camera] capability probe failed: $error');
      return CameraAvailability.missingHardware;
    }
  }

  /// Records that a camera start is about to run. Must be awaited so the flag
  /// reaches disk before the call that may not return.
  Future<void> markStartAttempt() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.setBool(pendingStartKey, true);
    } catch (error) {
      CoduxLog.warn('[codux-flutter-camera] start marker write failed: $error');
    }
  }

  /// Clears the marker once a start returned — success or a caught failure.
  /// Only a start that never returned at all leaves the marker set.
  Future<void> markStartSettled() async {
    try {
      final prefs = await SharedPreferences.getInstance();
      await prefs.remove(pendingStartKey);
    } catch (error) {
      CoduxLog.warn('[codux-flutter-camera] start marker clear failed: $error');
    }
  }

  /// Drops the crash block so a one-off native failure does not disable
  /// scanning forever. The next [resolve] probes the hardware again.
  Future<void> clearCrashBlock() async {
    await markStartSettled();
    _availability = null;
  }
}
