import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/storage_service.dart';
import 'dart:convert';
import 'package:flutter_test/flutter_test.dart';
import 'package:shared_preferences/shared_preferences.dart';
// Test the existing plugin's failure contract without adding a production seam.
// ignore: depend_on_referenced_packages
import 'package:shared_preferences_platform_interface/shared_preferences_platform_interface.dart';

void main() {
  setUp(() {
    SharedPreferences.setMockInitialValues({});
  });

  test('project list cache is scoped by host without relay url', () async {
    final storage = StorageService();
    const device = StoredDevice(
      server: 'https://relay-a.example',
      hostId: 'host-1',
      deviceId: 'device-1',
      token: 'token',
      name: 'Phone',
      transports: [
        RemoteTransportCandidate(
          kind: RemoteTransportKind.iroh,
          url: 'https://relay-a.example',
          nodeId: 'node-1',
          relayUrl: 'https://relay.example',
        ),
      ],
    );
    const sameHostWithDifferentServer = StoredDevice(
      server: 'https://relay-b.example',
      hostId: 'host-1',
      deviceId: 'device-1',
      token: 'token',
      name: 'Phone',
      transports: [
        RemoteTransportCandidate(
          kind: RemoteTransportKind.iroh,
          url: 'https://relay-b.example',
          nodeId: 'node-1',
          relayUrl: 'https://relay.example',
        ),
      ],
    );

    await storage.saveCachedProjects(device, const [
      ProjectInfo(id: 'project-1', name: 'Codux', path: '/Volumes/Web/codux'),
    ]);

    final cached = await storage.loadCachedProjects(
      sameHostWithDifferentServer,
    );

    expect(cached, hasLength(1));
    expect(cached.single.id, 'project-1');
    expect(cached.single.name, 'Codux');
    expect(cached.single.path, '/Volumes/Web/codux');
    expect(await storage.loadLastProjectId(device), isNull);
  });

  test('last working project persists independently for each host', () async {
    final storage = StorageService();
    const desktop = StoredDevice(
      server: 'https://relay.example',
      hostId: 'desktop-host',
      deviceId: 'phone',
      token: '',
      name: 'Desktop',
    );
    const headless = StoredDevice(
      server: 'https://relay.example',
      hostId: 'headless-host',
      deviceId: 'phone',
      token: '',
      name: 'Headless',
    );
    await storage.saveLastProjectId(desktop, 'project-2');
    await storage.saveLastProjectId(headless, 'project-1');
    SharedPreferences.resetStatic();

    final restored = StorageService();
    expect(await restored.loadLastProjectId(desktop), 'project-2');
    expect(await restored.loadLastProjectId(headless), 'project-1');
  });

  test('rapid project reversal persists the last choice', () async {
    final storage = StorageService();
    const device = StoredDevice(
      server: '',
      hostId: 'host',
      deviceId: 'phone',
      token: '',
      name: 'Host',
    );
    await Future.wait([
      storage.saveLastProjectId(device, 'project-2'),
      storage.saveLastProjectId(device, 'project-1'),
      storage.saveLastProjectId(device, 'project-2'),
    ]);
    SharedPreferences.resetStatic();
    expect(await StorageService().loadLastProjectId(device), 'project-2');
  });

  test('last working project reports a rejected persistent write', () async {
    SharedPreferencesStorePlatform.instance = _RejectingPreferencesStore();
    const device = StoredDevice(
      server: '',
      hostId: 'host',
      deviceId: 'phone',
      token: '',
      name: 'Host',
    );
    await expectLater(
      StorageService().saveLastProjectId(device, 'project-2'),
      throwsStateError,
    );
  });

  test('loadDevices keeps iroh transports from cached devices', () async {
    SharedPreferences.setMockInitialValues({
      'flutter.${StorageService.devicesKey}': jsonEncode([
        {
          'server': 'https://relay.example',
          'hostId': 'host-1',
          'deviceId': 'device-1',
          'token': '',
          'name': 'Phone',
          'transports': [
            {
              'kind': RemoteTransportKind.iroh,
              'role': 'host',
              'url': 'https://relay.example',
              'nodeId': 'node-1',
              'relayUrl': 'https://relay.example',
              'ticket': 'endpoint-ticket',
            },
          ],
        },
      ]),
    });

    final devices = (await StorageService().initializeDevices()).devices;

    expect(devices.single.transports, hasLength(1));
    final iroh = devices.single.transportByKind(RemoteTransportKind.iroh);
    expect(iroh?.nodeId, 'node-1');
    expect(iroh?.relayUrl, 'https://relay.example');
    // Load must not quietly rewrite what was stored. Rebuilding each candidate
    // field by field dropped the endpoint ticket on every app start, so a
    // device paired with direct addresses lost them at the next launch.
    expect(iroh?.ticket, 'endpoint-ticket');
  });

  test('device normalization writes through the snapshot owner', () async {
    SharedPreferences.setMockInitialValues({
      'flutter.${StorageService.devicesKey}': jsonEncode([
        {
          'hostId': 'host-1',
          'deviceId': 'device-1',
          'token': '',
          'name': 'Phone',
          'transports': [
            {'kind': 'unsupported'},
          ],
        },
      ]),
    });

    final devices = (await StorageService().initializeDevices()).devices;
    final persisted = (await SharedPreferences.getInstance()).getString(
      StorageService.devicesKey,
    );

    expect(devices.single.transports, isEmpty);
    expect(persisted, isNotNull);
    expect((jsonDecode(persisted!) as List).single['transports'], isEmpty);
  });

  test('single-device migration writes through the snapshot owner', () async {
    SharedPreferences.setMockInitialValues({
      'flutter.${StorageService.singleDeviceKey}': jsonEncode({
        'hostId': 'host-1',
        'deviceId': 'device-1',
        'token': '',
        'name': 'Phone',
      }),
    });
    final devices = (await StorageService().initializeDevices()).devices;
    final persisted = (await SharedPreferences.getInstance()).getString(
      StorageService.devicesKey,
    );

    expect(devices.single.hostId, 'host-1');
    expect(persisted, isNotNull);
  });
}

class _RejectingPreferencesStore extends InMemorySharedPreferencesStore {
  _RejectingPreferencesStore() : super.empty();

  @override
  Future<bool> setValue(String valueType, String key, Object value) async =>
      false;
}
