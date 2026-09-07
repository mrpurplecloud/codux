import 'dart:convert';
import 'package:shared_preferences/shared_preferences.dart';
import '../models/remote_models.dart';
import 'device_snapshot_store.dart';

class StorageService {
  StorageService({DeviceSnapshotStore? deviceSnapshots})
    : _deviceSnapshots =
          deviceSnapshots ??
          DeviceSnapshotStore(read: _readDeviceSeed, write: _writeDevices);

  static const devicesKey = 'codux.mobile.devices';
  static const singleDeviceKey = 'codux.mobile.device';
  static const lastDeviceIdKey = 'codux.mobile.last_device_id';
  static const settingsKey = 'codux.mobile.settings';
  static const projectCachePrefix = 'codux.mobile.projects';

  final DeviceSnapshotStore _deviceSnapshots;

  static Future<DeviceSnapshotSeed> _readDeviceSeed() async {
    final prefs = await SharedPreferences.getInstance();
    final value = prefs.getString(devicesKey);
    if (value != null && value.isNotEmpty) {
      final list = jsonDecode(value) as List<dynamic>;
      final devices = list
          .map((item) => StoredDevice.fromJson(item as Map<String, dynamic>))
          .toList();
      final normalized = devices.map(_normalizeDeviceTransport).toList();
      return DeviceSnapshotSeed(
        normalized,
        requiresCommit: _encodeDevices(normalized) != _encodeDevices(devices),
      );
    }
    final singleDevice = prefs.getString(singleDeviceKey);
    if (singleDevice != null && singleDevice.isNotEmpty) {
      final migrated = [
        StoredDevice.fromJson(jsonDecode(singleDevice) as Map<String, dynamic>),
      ].map(_normalizeDeviceTransport);
      return DeviceSnapshotSeed(migrated, requiresCommit: true);
    }
    return DeviceSnapshotSeed(const []);
  }

  static Future<bool> _writeDevices(List<StoredDevice> devices) async {
    final prefs = await SharedPreferences.getInstance();
    return prefs.setString(devicesKey, _encodeDevices(devices));
  }

  Future<DeviceSnapshot> initializeDevices() => _deviceSnapshots.initialize();

  Future<DeviceSnapshot> currentDeviceSnapshot() => _deviceSnapshots.snapshot();

  Future<DeviceSnapshot?> applyRequiredDeviceMutation(
    DeviceMutation mutation,
  ) => _deviceSnapshots.applyRequired(mutation);

  Future<DeviceSnapshot?> applyBackgroundDeviceMutation(
    DeviceMutation mutation,
  ) => _deviceSnapshots.applyBackground(mutation);

  /// Drop transports this build cannot dial, keeping the rest as stored.
  ///
  /// Previously rebuilt each survivor field by field, which quietly discarded
  /// the endpoint ticket on every app start — so a device that had one after
  /// pairing lost it on the next launch, and with it the host's direct
  /// addresses. Filtering is the whole job; the candidates pass through
  /// untouched.
  static StoredDevice _normalizeDeviceTransport(StoredDevice device) {
    final transports = device.transports
        .where((candidate) => candidate.kind == RemoteTransportKind.iroh)
        .toList();
    return device.copyWith(transports: transports);
  }

  Future<String?> loadLastDeviceId() async {
    final prefs = await SharedPreferences.getInstance();
    final value = prefs.getString(lastDeviceIdKey);
    return value == null || value.isEmpty ? null : value;
  }

  Future<void> saveLastDeviceId(String deviceId) async {
    if (deviceId.isEmpty) return;
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(lastDeviceIdKey, deviceId);
  }

  Future<MobileSettings?> loadSettings() async {
    final prefs = await SharedPreferences.getInstance();
    final value = prefs.getString(settingsKey);
    if (value == null || value.isEmpty) return null;
    return MobileSettings.fromJson(jsonDecode(value) as Map<String, dynamic>);
  }

  Future<void> saveSettings(MobileSettings settings) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(settingsKey, jsonEncode(settings.toJson()));
  }

  Future<List<ProjectInfo>> loadCachedProjects(StoredDevice device) async {
    final prefs = await SharedPreferences.getInstance();
    final value = prefs.getString(_projectCacheKey(device));
    if (value == null || value.isEmpty) return [];
    final list = jsonDecode(value) as List<dynamic>;
    return list
        .map((item) => ProjectInfo.fromJson(item as Map<String, dynamic>))
        .toList();
  }

  Future<void> saveCachedProjects(
    StoredDevice device,
    List<ProjectInfo> projects,
  ) async {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(
      _projectCacheKey(device),
      jsonEncode(projects.map((item) => item.toJson()).toList()),
    );
  }

  String _projectCacheKey(StoredDevice device) =>
      '$projectCachePrefix.${device.hostId}';
}

String _encodeDevices(Iterable<StoredDevice> devices) =>
    jsonEncode(devices.map((item) => item.toJson()).toList());
