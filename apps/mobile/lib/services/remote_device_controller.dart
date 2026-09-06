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

  RemoteDeviceUpdateResult updateHostName({
    required List<StoredDevice> devices,
    required StoredDevice? activeDevice,
    required String deviceId,
    required String? hostName,
  }) {
    StoredDevice? updated;
    final next = devices.map((item) {
      if (item.deviceId != deviceId) return item;
      updated = item.copyWith(hostName: hostName);
      return updated!;
    }).toList();
    if (updated == null) {
      return RemoteDeviceUpdateResult(
        state: preserveActive(devices: devices, activeDevice: activeDevice),
        updatedDevice: null,
      );
    }
    return RemoteDeviceUpdateResult(
      state: RemoteDeviceState(
        devices: List.unmodifiable(next),
        activeDevice: activeDevice?.deviceId == deviceId
            ? updated
            : _deviceById(next, activeDevice?.deviceId) ?? _firstOrNull(next),
      ),
      updatedDevice: updated,
    );
  }

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
  }) {
    if (transports.isEmpty) {
      return RemoteDeviceUpdateResult(
        state: preserveActive(devices: devices, activeDevice: activeDevice),
        updatedDevice: null,
      );
    }
    StoredDevice? updated;
    final next = devices.map((item) {
      if (item.deviceId != deviceId) return item;
      if (_sameTransports(item.transports, transports)) return item;
      updated = item.copyWith(transports: transports);
      return updated!;
    }).toList();
    if (updated == null) {
      return RemoteDeviceUpdateResult(
        state: preserveActive(devices: devices, activeDevice: activeDevice),
        updatedDevice: null,
      );
    }
    return RemoteDeviceUpdateResult(
      state: RemoteDeviceState(
        devices: List.unmodifiable(next),
        activeDevice: activeDevice?.deviceId == deviceId
            ? updated
            : _deviceById(next, activeDevice?.deviceId) ?? _firstOrNull(next),
      ),
      updatedDevice: updated,
    );
  }

  bool _sameTransports(
    List<RemoteTransportCandidate> a,
    List<RemoteTransportCandidate> b,
  ) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i += 1) {
      if (a[i] != b[i]) return false;
    }
    return true;
  }

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
  }) {
    StoredDevice? updated;
    final next = devices.map((item) {
      if (item.deviceId != deviceId) return item;
      final promoted = item.withDirectAddr(addr);
      if (identical(promoted, item)) return item;
      updated = promoted;
      return promoted;
    }).toList();
    if (updated == null) {
      return RemoteDeviceUpdateResult(
        state: preserveActive(devices: devices, activeDevice: activeDevice),
        updatedDevice: null,
      );
    }
    return RemoteDeviceUpdateResult(
      state: RemoteDeviceState(
        devices: List.unmodifiable(next),
        activeDevice: activeDevice?.deviceId == deviceId
            ? updated
            : _deviceById(next, activeDevice?.deviceId) ?? _firstOrNull(next),
      ),
      updatedDevice: updated,
    );
  }

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
