import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/remote_device_controller.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  const controller = RemoteDeviceController();
  const mac = StoredDevice(
    server: 'https://relay.example',
    hostId: 'host-mac',
    deviceId: 'device-mac',
    token: 'token-mac',
    name: 'Mac',
  );
  const win = StoredDevice(
    server: 'https://relay.example',
    hostId: 'host-win',
    deviceId: 'device-win',
    token: 'token-win',
    name: 'Win',
  );

  test('preserves active device when saving the device list', () {
    final state = controller.preserveActive(
      devices: const [mac, win],
      activeDevice: win,
    );

    expect(state.devices, const [mac, win]);
    expect(state.activeDevice, win);
  });

  test('falls back to the first device when active is missing', () {
    final state = controller.preserveActive(
      devices: const [mac, win],
      activeDevice: const StoredDevice(
        server: '',
        hostId: 'missing',
        deviceId: 'missing',
        token: '',
        name: 'Missing',
      ),
    );

    expect(state.activeDevice, mac);
  });
}
