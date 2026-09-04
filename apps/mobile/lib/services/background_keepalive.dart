import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'log_service.dart';

/// Whether the host link should be held while the app is not in front.
///
/// The link dies on its own once the process stops sending iroh heartbeats, so
/// "keep it alive" means "keep the process scheduled" — which only makes sense
/// while there is a link to keep and the user asked for it.
bool shouldKeepAliveInBackground({
  required bool enabled,
  required bool connected,
  required AppLifecycleState state,
}) {
  if (!enabled || !connected) return false;
  return state == AppLifecycleState.paused || state == AppLifecycleState.hidden;
}

/// Android foreground service that keeps this process out of the freezer while
/// the app sits in the background, so the transport's heartbeats keep flowing
/// and the user comes back to the same connection.
///
/// Every other platform is a no-op: iOS suspends the process outright and has
/// no supported way to hold a UDP socket in the background.
class BackgroundKeepAlive {
  BackgroundKeepAlive({MethodChannel? channel, bool? supported})
    : _channel = channel ?? const MethodChannel(channelName),
      _supported = supported ?? Platform.isAndroid;

  static const channelName = 'codux/background_keepalive';

  final MethodChannel _channel;
  final bool _supported;
  bool _running = false;
  int _commandSerial = 0;
  int _pendingStarts = 0;

  bool get isRunning => _running;

  /// Start (or refresh) the service. Returns whether it is running afterwards;
  /// a refused start leaves the pre-change behaviour — the link drops and the
  /// app reconnects on resume.
  Future<bool> start({
    required String title,
    required String body,
  }) async {
    if (!_supported) return false;
    final commandSerial = ++_commandSerial;
    _pendingStarts += 1;
    var started = false;
    try {
      started =
          await _channel.invokeMethod<bool>('start', {
            'title': title,
            'body': body,
          }) ??
          false;
    } on PlatformException catch (error) {
      CoduxLog.warn(
        '[codux-flutter-keepalive] start failed code=${error.code} message=${error.message ?? ''}',
      );
    } on MissingPluginException {
      // A build without the native side cannot hold the process.
    } finally {
      _pendingStarts -= 1;
    }
    if (commandSerial != _commandSerial) return false;
    _running = started;
    return _running;
  }

  Future<void> stop() async {
    if (!_supported || (!_running && _pendingStarts == 0)) return;
    final commandSerial = ++_commandSerial;
    _running = false;
    try {
      await _channel.invokeMethod<void>('stop');
    } on PlatformException catch (error) {
      CoduxLog.warn(
        '[codux-flutter-keepalive] stop failed code=${error.code} message=${error.message ?? ''}',
      );
      if (commandSerial == _commandSerial) _running = true;
    } on MissingPluginException {
      // Nothing to stop on a build without the native side.
    }
  }
}
