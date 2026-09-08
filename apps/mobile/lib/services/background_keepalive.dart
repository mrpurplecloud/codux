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

  /// What the app wants the service to be — the only authority. [_running] is
  /// what the platform last reported. Every call records the desire and joins
  /// the one worker that closes the gap, so interleaved start/stop calls can
  /// never strand the service in the state the last *command* asked for
  /// instead of the state the app wants.
  bool _desired = false;
  bool _running = false;
  String _title = '';
  String _body = '';

  /// The single reconcile worker. Callers join it; they never send commands of
  /// their own, so one intent is one platform request.
  Future<void>? _worker;

  bool get isRunning => _running;

  Future<void> requestBackgroundExecutionSettings() async {
    if (!_supported) return;
    try {
      await _channel.invokeMethod<void>('requestBackgroundExecutionSettings');
    } on PlatformException catch (error) {
      CoduxLog.warn(
        '[codux-flutter-keepalive] background settings request failed '
        'code=${error.code} message=${error.message ?? ''}',
      );
    } on MissingPluginException {
      return;
    }
  }

  /// Start (or refresh) the service. Returns whether it is running afterwards;
  /// a refused start leaves the pre-change behaviour — the link drops and the
  /// app reconnects on resume.
  Future<bool> start({required String title, required String body}) async {
    if (!_supported) return false;
    _title = title;
    _body = body;
    _desired = true;
    await _reconcile();
    return _running;
  }

  Future<void> stop() async {
    if (!_supported) return;
    _desired = false;
    await _reconcile();
  }

  /// Join the reconcile worker, starting it if the last one has finished.
  Future<void> _reconcile() => _worker ??= _drive()
      // Cleared before the joined future completes, so the next desire starts
      // a fresh worker rather than joining a finished one.
      .whenComplete(() => _worker = null);

  /// Send one command at a time until the desire is reached, re-reading it
  /// after every command — a start that lands mid-stop (or the reverse) is
  /// honoured by this same loop. A refused command ends the pass: the platform
  /// said no, and retrying it here would only repeat the request.
  Future<void> _drive() async {
    while (_desired != _running) {
      final target = _desired;
      if (!await _invoke(target)) {
        _running = !target;
        return;
      }
      _running = target;
    }
  }

  /// Send one command. Returns whether it was applied; `false` means the
  /// platform refused and the caller must not keep retrying.
  Future<bool> _invoke(bool target) async {
    try {
      if (target) {
        return await _channel.invokeMethod<bool>('start', {
              'title': _title,
              'body': _body,
            }) ??
            false;
      }
      await _channel.invokeMethod<void>('stop');
      return true;
    } on PlatformException catch (error) {
      CoduxLog.warn(
        '[codux-flutter-keepalive] ${target ? 'start' : 'stop'} failed '
        'code=${error.code} message=${error.message ?? ''}',
      );
      return false;
    } on MissingPluginException {
      // A build without the native side can neither hold nor release the
      // process.
      return false;
    }
  }
}
