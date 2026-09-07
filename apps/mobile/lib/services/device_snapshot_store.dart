import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart' show listEquals;

import '../models/remote_models.dart';
import 'log_service.dart';

typedef DeviceSnapshotRead = Future<DeviceSnapshotSeed> Function();
typedef DeviceSnapshotWrite = Future<bool> Function(List<StoredDevice> devices);

final class DeviceSnapshotSeed {
  DeviceSnapshotSeed(
    Iterable<StoredDevice> devices, {
    this.requiresCommit = false,
  }) : devices = List.unmodifiable(devices);

  final List<StoredDevice> devices;
  final bool requiresCommit;
}

final class DeviceSnapshot {
  const DeviceSnapshot({required this.revision, required this.devices});

  final int revision;
  final List<StoredDevice> devices;
}

sealed class DeviceMutation {
  const DeviceMutation();

  List<StoredDevice> applyTo(List<StoredDevice> devices);
}

final class DeviceUpserted extends DeviceMutation {
  const DeviceUpserted(this.device);

  final StoredDevice device;

  @override
  List<StoredDevice> applyTo(List<StoredDevice> devices) => List.unmodifiable([
    device,
    ...devices.where((item) => item.deviceId != device.deviceId),
  ]);
}

final class DeviceRemoved extends DeviceMutation {
  const DeviceRemoved(this.deviceId);

  final String deviceId;

  @override
  List<StoredDevice> applyTo(List<StoredDevice> devices) {
    if (!devices.any((item) => item.deviceId == deviceId)) return devices;
    return List.unmodifiable(
      devices.where((item) => item.deviceId != deviceId),
    );
  }
}

final class DeviceHostNameChanged extends DeviceMutation {
  const DeviceHostNameChanged(this.deviceId, this.hostName);

  final String deviceId;
  final String hostName;

  @override
  List<StoredDevice> applyTo(List<StoredDevice> devices) =>
      _replaceDevice(devices, deviceId, (device) {
        if (device.hostName == hostName) return device;
        return device.copyWith(hostName: hostName);
      });
}

final class DeviceTransportsReplaced extends DeviceMutation {
  const DeviceTransportsReplaced(this.deviceId, this.transports);

  final String deviceId;
  final List<RemoteTransportCandidate> transports;

  @override
  List<StoredDevice> applyTo(List<StoredDevice> devices) {
    if (transports.isEmpty) return devices;
    return _replaceDevice(devices, deviceId, (device) {
      if (listEquals(device.transports, transports)) return device;
      return device.copyWith(transports: List.unmodifiable(transports));
    });
  }
}

final class DeviceDirectAddrRemembered extends DeviceMutation {
  const DeviceDirectAddrRemembered(this.deviceId, this.addr);

  final String deviceId;
  final String addr;

  @override
  List<StoredDevice> applyTo(List<StoredDevice> devices) => _replaceDevice(
    devices,
    deviceId,
    (device) => device.withDirectAddr(addr),
  );
}

List<StoredDevice> _replaceDevice(
  List<StoredDevice> devices,
  String deviceId,
  StoredDevice Function(StoredDevice device) replace,
) {
  var changed = false;
  final next = devices
      .map((device) {
        if (device.deviceId != deviceId) return device;
        final replacement = replace(device);
        if (!identical(replacement, device)) changed = true;
        return replacement;
      })
      .toList(growable: false);
  return changed ? List.unmodifiable(next) : devices;
}

/// Owns accepted device state and serializes every durable transition.
class DeviceSnapshotStore {
  DeviceSnapshotStore({
    required DeviceSnapshotRead read,
    required DeviceSnapshotWrite write,
    Duration retryBase = const Duration(milliseconds: 250),
    Duration retryMax = const Duration(seconds: 30),
    Timer Function(Duration, void Function()) timerFactory = Timer.new,
  }) : _read = read,
       _write = write,
       _retryBase = retryBase,
       _retryMax = retryMax,
       _timerFactory = timerFactory;

  final DeviceSnapshotRead _read;
  final DeviceSnapshotWrite _write;
  final Duration _retryBase;
  final Duration _retryMax;
  final Timer Function(Duration, void Function()) _timerFactory;

  Future<void> _operations = Future<void>.value();
  Future<DeviceSnapshot>? _initialization;
  List<StoredDevice> _accepted = const [];
  _PendingDeviceSnapshot? _background;
  int _revision = 0;
  int _consecutiveFailures = 0;
  bool _initialized = false;
  bool _backgroundDrainQueued = false;
  Timer? _retryTimer;

  Future<DeviceSnapshot> initialize() {
    if (_initialized) return Future.value(_snapshot);
    final existing = _initialization;
    if (existing != null) return existing;
    final future = _serialize(() async {
      if (_initialized) return _snapshot;
      final seed = await _read();
      final devices = List<StoredDevice>.unmodifiable(seed.devices);
      if (seed.requiresCommit) await _writeDevices(devices);
      _accepted = devices;
      _revision += 1;
      _initialized = true;
      return _snapshot;
    });
    _initialization = future;
    unawaited(
      future.then<void>(
        (_) {
          if (identical(_initialization, future)) _initialization = null;
        },
        onError: (Object _, StackTrace _) {
          if (identical(_initialization, future)) _initialization = null;
        },
      ),
    );
    return future;
  }

  Future<DeviceSnapshot> snapshot() async {
    await initialize();
    return _serialize(() async => _snapshot);
  }

  /// Commits first, then publishes. Rejected state can never leak into a later
  /// background snapshot.
  Future<DeviceSnapshot?> applyRequired(DeviceMutation mutation) async {
    await initialize();
    return _serialize(() async {
      final next = mutation.applyTo(_accepted);
      if (identical(next, _accepted)) return null;
      await _writeDevices(next);
      _accepted = List.unmodifiable(next);
      _revision += 1;
      _background = null;
      _retryTimer?.cancel();
      _retryTimer = null;
      _consecutiveFailures = 0;
      return _snapshot;
    });
  }

  /// Publishes current intent immediately, then retains it until storage
  /// converges. A newer required commit supersedes this full-state projection.
  Future<DeviceSnapshot?> applyBackground(DeviceMutation mutation) async {
    await initialize();
    return _serialize(() async {
      final next = mutation.applyTo(_accepted);
      if (identical(next, _accepted)) return null;
      _accepted = List.unmodifiable(next);
      _revision += 1;
      _background = _PendingDeviceSnapshot(_revision, _accepted);
      _retryTimer?.cancel();
      _retryTimer = null;
      _startBackgroundDrain();
      return _snapshot;
    });
  }

  DeviceSnapshot get _snapshot =>
      DeviceSnapshot(revision: _revision, devices: _accepted);

  Future<T> _serialize<T>(Future<T> Function() operation) {
    final result = Completer<T>();
    _operations = _operations.then((_) async {
      try {
        result.complete(await operation());
      } catch (error, stackTrace) {
        result.completeError(error, stackTrace);
      }
    });
    return result.future;
  }

  void _startBackgroundDrain() {
    if (_backgroundDrainQueued || _background == null || _retryTimer != null) {
      return;
    }
    _backgroundDrainQueued = true;
    final drain = _serialize(() async {
      _backgroundDrainQueued = false;
      final pending = _background;
      if (pending == null) return;
      try {
        await _writeDevices(pending.devices);
      } catch (error) {
        _scheduleRetry(pending.revision, error);
        return;
      }
      if (_background?.revision == pending.revision) {
        _background = null;
      }
      _consecutiveFailures = 0;
    });
    unawaited(drain.then((_) => _startBackgroundDrain()));
  }

  Future<void> _writeDevices(List<StoredDevice> devices) async {
    if (!await _write(devices)) {
      throw StateError('device snapshot write returned false');
    }
  }

  void _scheduleRetry(int failedRevision, Object error) {
    if (_retryTimer != null) return;
    final shift = math.min(_consecutiveFailures, 20);
    final delayMillis = math.min(
      _retryBase.inMilliseconds * (1 << shift),
      _retryMax.inMilliseconds,
    );
    _consecutiveFailures += 1;
    CoduxLog.warn(
      '[codux-flutter-storage] device snapshot write failed '
      'revision=$failedRevision retryMs=$delayMillis error=$error',
    );
    _retryTimer = _timerFactory(Duration(milliseconds: delayMillis), () {
      _retryTimer = null;
      _startBackgroundDrain();
    });
  }
}

final class _PendingDeviceSnapshot {
  const _PendingDeviceSnapshot(this.revision, this.devices);

  final int revision;
  final List<StoredDevice> devices;
}
