import '../models/remote_models.dart';

class RemoteDeviceState {
  const RemoteDeviceState({required this.devices, required this.activeDevice});

  final List<StoredDevice> devices;
  final StoredDevice? activeDevice;
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
