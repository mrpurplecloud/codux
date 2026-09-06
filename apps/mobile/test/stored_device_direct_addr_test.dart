import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/remote_device_controller.dart';
import 'package:flutter_test/flutter_test.dart';

StoredDevice device({List<String> directAddrs = const []}) => StoredDevice(
  server: 'https://relay.example',
  hostId: 'host-1',
  deviceId: 'device-1',
  token: 'token-1',
  name: 'Phone',
  directAddrs: directAddrs,
);

void main() {
  group('StoredDevice.withDirectAddr', () {
    test('records the address a working connection used', () {
      final updated = device().withDirectAddr('10.8.0.3:41234');

      expect(updated.directAddrs, ['10.8.0.3:41234']);
    });

    test('promotes a known address instead of duplicating it', () {
      final updated = device(
        directAddrs: ['192.168.1.5:41234', '10.8.0.3:41234'],
      ).withDirectAddr('10.8.0.3:41234');

      expect(updated.directAddrs, ['10.8.0.3:41234', '192.168.1.5:41234']);
    });

    test('returns the same instance when nothing would change', () {
      final original = device(directAddrs: ['10.8.0.3:41234']);

      expect(
        identical(original.withDirectAddr('10.8.0.3:41234'), original),
        isTrue,
        reason: 'callers use identity to skip a pointless write',
      );
      expect(identical(original.withDirectAddr('  '), original), isTrue);
    });

    test('keeps a bounded history, dropping the least recently used', () {
      var current = device();
      for (final addr in ['a:1', 'b:2', 'c:3', 'd:4']) {
        current = current.withDirectAddr(addr);
      }

      expect(current.directAddrs.length, StoredDevice.maxDirectAddrs);
      expect(current.directAddrs.first, 'd:4');
      expect(
        current.directAddrs,
        isNot(contains('a:1')),
        reason: 'the oldest address ages out',
      );
    });
  });

  group('persistence', () {
    test('survives a json round trip', () {
      final original = device(directAddrs: ['10.8.0.3:41234']);

      final restored = StoredDevice.fromJson(original.toJson());

      expect(restored.directAddrs, ['10.8.0.3:41234']);
    });

    test('a record written before this field existed still loads', () {
      final legacy = {
        'server': 'https://relay.example',
        'hostId': 'host-1',
        'deviceId': 'device-1',
        'token': 'token-1',
        'name': 'Phone',
        'transports': <Map<String, dynamic>>[],
      };

      final restored = StoredDevice.fromJson(legacy);

      expect(restored.directAddrs, isEmpty);
      expect(
        restored.toJson().containsKey('directAddrs'),
        isFalse,
        reason: 'a device that never connected stays byte-identical to before',
      );
      expect(restored.withDirectAddr('10.8.0.3:41234').directAddrs, [
        '10.8.0.3:41234',
      ]);
    });

    test('a malformed stored value degrades to no history', () {
      final restored = StoredDevice.fromJson({
        'hostId': 'host-1',
        'deviceId': 'device-1',
        'token': 't',
        'name': 'Phone',
        'directAddrs': 'not-a-list',
      });

      expect(restored.directAddrs, isEmpty);
    });
  });

  group('RemoteDeviceController.rememberDirectAddr', () {
    const controller = RemoteDeviceController();

    test('updates the matching device and the active reference', () {
      final devices = [device()];

      final result = controller.rememberDirectAddr(
        devices: devices,
        activeDevice: devices.first,
        deviceId: 'device-1',
        addr: '10.8.0.3:41234',
      );

      expect(result.updatedDevice?.directAddrs, ['10.8.0.3:41234']);
      expect(result.state.activeDevice?.directAddrs, ['10.8.0.3:41234']);
      expect(result.state.devices.first.directAddrs, ['10.8.0.3:41234']);
    });

    test('reports no update when the address is already the most recent', () {
      final devices = [
        device(directAddrs: ['10.8.0.3:41234']),
      ];

      final result = controller.rememberDirectAddr(
        devices: devices,
        activeDevice: devices.first,
        deviceId: 'device-1',
        addr: '10.8.0.3:41234',
      );

      expect(
        result.updatedDevice,
        isNull,
        reason: 'an unchanged device must not trigger a disk write',
      );
    });

    test('reports no update for an unknown device', () {
      final devices = [device()];

      final result = controller.rememberDirectAddr(
        devices: devices,
        activeDevice: devices.first,
        deviceId: 'someone-else',
        addr: '10.8.0.3:41234',
      );

      expect(result.updatedDevice, isNull);
      expect(result.state.devices.first.directAddrs, isEmpty);
    });
  });
}
