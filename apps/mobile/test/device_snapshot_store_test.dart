import 'dart:async';

import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/device_snapshot_store.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('repeated initialize returns the current accepted snapshot', () async {
    final store = DeviceSnapshotStore(
      read: () async => DeviceSnapshotSeed([_device()]),
      write: (_) async => true,
    );

    final initial = await store.initialize();
    await store.applyBackground(
      const DeviceDirectAddrRemembered('device-1', '192.0.2.1:443'),
    );
    final repeated = await store.initialize();

    expect(repeated.revision, greaterThan(initial.revision));
    expect(repeated.devices.single.directAddrs, ['192.0.2.1:443']);
  });

  test(
    'required failure is never revived by a later background mutation',
    () async {
      final writes = <List<StoredDevice>>[];
      final store = DeviceSnapshotStore(
        read: () async => DeviceSnapshotSeed([_device(hostName: 'accepted')]),
        write: (devices) async {
          writes.add(devices);
          return devices.single.hostName != 'rejected';
        },
      );
      await store.initialize();

      await expectLater(
        store.applyRequired(
          const DeviceHostNameChanged('device-1', 'rejected'),
        ),
        throwsA(isA<StateError>()),
      );
      await store.applyBackground(
        const DeviceDirectAddrRemembered('device-1', '192.0.2.1:443'),
      );
      final accepted = await store.snapshot();

      expect(accepted.devices.single.hostName, 'accepted');
      expect(accepted.devices.single.directAddrs, ['192.0.2.1:443']);
      expect(writes, hasLength(2));
      expect(writes.last.single.hostName, 'accepted');
    },
  );

  test('serializes required writes and commits invocation order', () async {
    final writes = <List<StoredDevice>>[];
    final completions = <Completer<bool>>[];
    final store = DeviceSnapshotStore(
      read: () async => DeviceSnapshotSeed(const []),
      write: (devices) {
        writes.add(devices);
        final completion = Completer<bool>();
        completions.add(completion);
        return completion.future;
      },
    );
    await store.initialize();

    final first = store.applyRequired(DeviceUpserted(_device()));
    final second = store.applyRequired(
      DeviceUpserted(_device(deviceId: 'device-2')),
    );
    await _pumpEventQueue();
    expect(writes, hasLength(1));

    completions.first.complete(true);
    await _pumpEventQueue();
    expect(writes, hasLength(2));
    expect(writes.last.map((device) => device.deviceId), [
      'device-2',
      'device-1',
    ]);

    completions.last.complete(true);
    await Future.wait([first, second]);
  });

  test(
    'background mutation remains accepted while its write retries',
    () async {
      final timers = <_FakeTimer>[];
      var attempts = 0;
      final store = DeviceSnapshotStore(
        read: () async => DeviceSnapshotSeed([_device()]),
        write: (_) async {
          attempts += 1;
          return attempts > 1;
        },
        timerFactory: (duration, callback) {
          final timer = _FakeTimer(callback);
          timers.add(timer);
          return timer;
        },
      );
      await store.initialize();

      await store.applyBackground(
        const DeviceDirectAddrRemembered('device-1', '192.0.2.1:443'),
      );
      await _pumpEventQueue();

      expect((await store.snapshot()).devices.single.directAddrs, isNotEmpty);
      expect(timers, hasLength(1));
      timers.single.fire();
      await _pumpEventQueue();
      expect(attempts, 2);
    },
  );

  test('required commit supersedes an older scheduled retry', () async {
    final writes = <List<StoredDevice>>[];
    final timers = <_FakeTimer>[];
    var attempts = 0;
    final store = DeviceSnapshotStore(
      read: () async => DeviceSnapshotSeed([_device()]),
      write: (devices) async {
        writes.add(devices);
        attempts += 1;
        return attempts > 1;
      },
      timerFactory: (duration, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );
    await store.initialize();
    await store.applyBackground(
      const DeviceDirectAddrRemembered('device-1', '192.0.2.1:443'),
    );
    await _pumpEventQueue();

    await store.applyRequired(
      const DeviceHostNameChanged('device-1', 'renamed'),
    );

    expect(writes, hasLength(2));
    expect(writes.last.single.hostName, 'renamed');
    expect(writes.last.single.directAddrs, ['192.0.2.1:443']);
    expect(timers.single.isActive, isFalse);
    timers.single.fire();
    await _pumpEventQueue();
    expect(writes, hasLength(2));
  });

  test(
    'live mutation queued during initialize applies after loaded state',
    () async {
      final read = Completer<DeviceSnapshotSeed>();
      final store = DeviceSnapshotStore(
        read: () => read.future,
        write: (_) async => true,
      );

      final initialize = store.initialize();
      final liveMutation = store.applyBackground(
        DeviceUpserted(_device(deviceId: 'device-live')),
      );
      read.complete(DeviceSnapshotSeed([_device(deviceId: 'device-stored')]));
      await initialize;
      await liveMutation;

      expect(
        (await store.snapshot()).devices.map((device) => device.deviceId),
        ['device-live', 'device-stored'],
      );
    },
  );

  test('initial normalization commits before a queued live mutation', () async {
    final writes = <List<StoredDevice>>[];
    final completions = <Completer<bool>>[];
    final store = DeviceSnapshotStore(
      read: () async => DeviceSnapshotSeed([
        _device(deviceId: 'device-normalized'),
      ], requiresCommit: true),
      write: (devices) {
        writes.add(devices);
        final completion = Completer<bool>();
        completions.add(completion);
        return completion.future;
      },
    );

    final initialize = store.initialize();
    final liveMutation = store.applyBackground(
      DeviceUpserted(_device(deviceId: 'device-live')),
    );
    await _pumpEventQueue();
    expect(writes.single.map((device) => device.deviceId), [
      'device-normalized',
    ]);

    completions.first.complete(true);
    await initialize;
    await _pumpEventQueue();
    expect(writes.last.map((device) => device.deviceId), [
      'device-live',
      'device-normalized',
    ]);

    completions.last.complete(true);
    await liveMutation;
  });
}

StoredDevice _device({String deviceId = 'device-1', String? hostName}) =>
    StoredDevice(
      server: '',
      hostId: 'host-$deviceId',
      deviceId: deviceId,
      token: 'token',
      name: 'Phone',
      hostName: hostName,
    );

Future<void> _pumpEventQueue() => Future<void>.delayed(Duration.zero);

final class _FakeTimer implements Timer {
  _FakeTimer(this._callback);

  final void Function() _callback;
  bool _active = true;

  void fire() {
    if (!_active) return;
    _active = false;
    _callback();
  }

  @override
  void cancel() => _active = false;

  @override
  bool get isActive => _active;

  @override
  int get tick => _active ? 0 : 1;
}
