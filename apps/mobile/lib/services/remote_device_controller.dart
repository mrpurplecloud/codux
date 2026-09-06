import 'package:flutter/foundation.dart' show listEquals;

import '../models/remote_models.dart';

class RemoteDeviceState {
  const RemoteDeviceState({required this.devices, required this.activeDevice});

  final List<StoredDevice> devices;
  final StoredDevice? activeDevice;
}

class RemoteDeviceUpdateResult {
  const RemoteDeviceUpdateResult({
    required this.state,
    required this.updatedDevice,
  });

  final RemoteDeviceState state;
  final StoredDevice? updatedDevice;
}

class RemoteDeviceRemoveResult {
  const RemoteDeviceRemoveResult({
    required this.state,
    required this.removedActive,
  });

  final RemoteDeviceState state;
  final bool removedActive;
}

class RemoteDeviceController {
  const RemoteDeviceController();

  RemoteDeviceState preserveActive({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
  }) {
    final activeId = activeDevice?.deviceId;
    return RemoteDeviceState(
      devices: List.unmodifiable(devices),
      activeDevice: _deviceById(devices, activeId) ?? _firstOrNull(devices),
    );
  }

  RemoteDeviceState upsertAndActivate({
    required List<StoredDevice> devices,
    required StoredDevice device,
  }) {
    final next = [
      device,
      ...devices.where((item) => item.deviceId != device.deviceId),
    ];
    return RemoteDeviceState(
      devices: List.unmodifiable(next),
      activeDevice: device,
    );
  }

  RemoteDeviceState replace({
    required List<StoredDevice> devices,
    required StoredDevice device,
    required StoredDevice? activeDevice,
  }) {
    final next = devices
        .map((item) => item.deviceId == device.deviceId ? device : item)
        .toList();
    return RemoteDeviceState(
      devices: List.unmodifiable(next),
      activeDevice: activeDevice?.deviceId == device.deviceId
          ? device
          : _deviceById(next, activeDevice?.deviceId) ?? _firstOrNull(next),
    );
  }

  /// Apply [change] to the device with [deviceId], keeping the active reference
  /// pointing at whichever record it pointed at before.
  ///
  /// [change] returns null — or the same instance — when it would change
  /// nothing; the result then reports `updatedDevice: null` so the caller can
  /// skip a disk write. Several of these run on every connection and usually
  /// have nothing to say.
  RemoteDeviceUpdateResult _mutateDevice({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required String deviceId,
    required StoredDevice? Function(StoredDevice) change,
  }) {
    StoredDevice? updated;
    final next = devices.map((item) {
      if (item.deviceId != deviceId) return item;
      final changed = change(item);
      if (changed == null || identical(changed, item)) return item;
      updated = changed;
      return changed;
    }).toList();
    final result = updated;
    if (result == null) {
      return RemoteDeviceUpdateResult(
        state: preserveActive(devices: devices, activeDevice: activeDevice),
        updatedDevice: null,
      );
    }
    return RemoteDeviceUpdateResult(
      state: RemoteDeviceState(
        devices: List.unmodifiable(next),
        activeDevice: activeDevice?.deviceId == deviceId
            ? result
            : _deviceById(next, activeDevice?.deviceId) ?? _firstOrNull(next),
      ),
      updatedDevice: result,
    );
  }

  RemoteDeviceUpdateResult updateHostName({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required String deviceId,
    required String? hostName,
  }) => _mutateDevice(
    devices: devices,
    activeDevice: activeDevice,
    deviceId: deviceId,
    change: (device) => device.copyWith(hostName: hostName),
  );

  /// Adopt the dial candidates [deviceId] just reported for itself.
  ///
  /// A stored candidate is a snapshot from pairing time; the host recomputes
  /// its own from its live endpoint on every `host.info`. Taking the reported
  /// set is how stored addresses follow a host that moved, instead of freezing
  /// at whatever was true when the two were first paired.
  ///
  /// An empty report changes nothing: a host that told us nothing has not told
  /// us to forget what we have, and discarding a working candidate set on the
  /// strength of silence would strand the device. Reports `updatedDevice: null`
  /// when nothing changed, so the caller can skip a disk write — `host.info`
  /// arrives on every connection and usually says the same thing.
  ///
  /// Other stored fields, including the remembered direct addresses, are left
  /// untouched: the two address sources are independent fallbacks and neither
  /// may erase the other.
  RemoteDeviceUpdateResult replaceTransports({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required String deviceId,
    required List<RemoteTransportCandidate> transports,
  }) => _mutateDevice(
    devices: devices,
    activeDevice: activeDevice,
    deviceId: deviceId,
    change: (device) =>
        transports.isEmpty || listEquals(device.transports, transports)
        ? null
        : device.copyWith(transports: transports),
  );

  /// Record that [deviceId] was just reached at [addr], so the next dial can try
  /// it alongside the addresses frozen into the pairing ticket.
  ///
  /// Reports `updatedDevice: null` when nothing changed — an unknown device, or
  /// an address that is already the most recent — so the caller can skip a disk
  /// write. This runs on every path report of a live connection, so "no change"
  /// is by far the common case.
  RemoteDeviceUpdateResult rememberDirectAddr({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required String deviceId,
    required String addr,
  }) => _mutateDevice(
    devices: devices,
    activeDevice: activeDevice,
    deviceId: deviceId,
    change: (device) => device.withDirectAddr(addr),
  );

  RemoteDeviceRemoveResult remove({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required StoredDevice device,
  }) {
    final removedActive = activeDevice?.deviceId == device.deviceId;
    final next = devices
        .where((item) => item.deviceId != device.deviceId)
        .toList();
    return RemoteDeviceRemoveResult(
      state: preserveActive(devices: next, activeDevice: activeDevice),
      removedActive: removedActive,
    );
  }

  StoredDevice? _deviceById(List<StoredDevice> devices, String? deviceId) {
    if (deviceId == null || deviceId.isEmpty) return null;
    for (final device in devices) {
      if (device.deviceId == deviceId) return device;
    }
    return null;
  }

  StoredDevice? _firstOrNull(List<StoredDevice> devices) =>
      devices.isEmpty ? null : devices.first;
}
