import 'dart:async';

import 'package:codux_flutter/services/background_keepalive.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  group('shouldKeepAliveInBackground', () {
    test('holds the process only while backgrounded with a live link', () {
      for (final state in const [
        AppLifecycleState.paused,
        AppLifecycleState.hidden,
      ]) {
        expect(
          shouldKeepAliveInBackground(
            enabled: true,
            connected: true,
            state: state,
          ),
          isTrue,
          reason: '$state is a background state',
        );
      }
    });

    test('stays off in the foreground, disconnected, or when disabled', () {
      expect(
        shouldKeepAliveInBackground(
          enabled: true,
          connected: true,
          state: AppLifecycleState.resumed,
        ),
        isFalse,
      );
      expect(
        shouldKeepAliveInBackground(
          enabled: true,
          connected: true,
          state: AppLifecycleState.inactive,
        ),
        isFalse,
      );
      expect(
        shouldKeepAliveInBackground(
          enabled: true,
          connected: false,
          state: AppLifecycleState.paused,
        ),
        isFalse,
      );
      expect(
        shouldKeepAliveInBackground(
          enabled: false,
          connected: true,
          state: AppLifecycleState.paused,
        ),
        isFalse,
      );
    });
  });

  group('BackgroundKeepAlive', () {
    late List<MethodCall> calls;
    late MethodChannel channel;

    setUp(() {
      TestWidgetsFlutterBinding.ensureInitialized();
      calls = [];
      channel = const MethodChannel(BackgroundKeepAlive.channelName);
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            calls.add(call);
            return call.method == 'start' ? true : null;
          });
    });

    tearDown(() {
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, null);
    });

    test('starts and stops through the platform channel', () async {
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      expect(await keepAlive.start(title: 'Codux', body: 'holding'), isTrue);
      expect(keepAlive.isRunning, isTrue);
      await keepAlive.stop();

      expect(keepAlive.isRunning, isFalse);
      expect(calls.map((call) => call.method).toList(), ['start', 'stop']);
      expect(calls.first.arguments, {'title': 'Codux', 'body': 'holding'});
    });

    test('requests background execution settings', () async {
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      await keepAlive.requestBackgroundExecutionSettings();

      expect(calls.single.method, 'requestBackgroundExecutionSettings');
    });

    test(
      'a refused start leaves the caller unprotected, not crashed',
      () async {
        TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
            .setMockMethodCallHandler(channel, (call) async {
              calls.add(call);
              throw PlatformException(code: 'refused');
            });
        final keepAlive = BackgroundKeepAlive(
          channel: channel,
          supported: true,
        );

        expect(await keepAlive.start(title: 'Codux', body: 'holding'), isFalse);
        expect(keepAlive.isRunning, isFalse);
      },
    );

    test('does nothing off Android', () async {
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: false);

      expect(await keepAlive.start(title: 'Codux', body: 'holding'), isFalse);
      await keepAlive.requestBackgroundExecutionSettings();
      await keepAlive.stop();

      expect(calls, isEmpty);
    });

    test('stop without a running service sends nothing', () async {
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      await keepAlive.stop();

      expect(calls, isEmpty);
    });

    test('stop wins while a start response is pending', () async {
      final startResult = Completer<bool>();
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            calls.add(call);
            if (call.method == 'start') return startResult.future;
            return null;
          });
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      final starting = keepAlive.start(title: 'Codux', body: 'holding');
      await Future<void>.delayed(Duration.zero);
      final stopping = keepAlive.stop();
      startResult.complete(true);
      await Future.wait([starting, stopping]);

      expect(calls.map((call) => call.method).toList(), ['start', 'stop']);
      expect(keepAlive.isRunning, isFalse);
    });

    test('a start that lands during a stop is not lost', () async {
      final firstStart = Completer<bool>();
      var starts = 0;
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            calls.add(call);
            if (call.method != 'start') return null;
            starts += 1;
            return starts == 1 ? firstStart.future : true;
          });
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      // Background → foreground → background while the first start is still
      // waiting on the platform: the app ends up wanting the service running.
      final first = keepAlive.start(title: 'Codux', body: 'holding');
      await Future<void>.delayed(Duration.zero);
      final stopping = keepAlive.stop();
      final second = keepAlive.start(title: 'Codux', body: 'holding');
      firstStart.complete(true);
      await Future.wait([first, second, stopping]);

      expect(keepAlive.isRunning, isTrue);
      expect(calls.last.method, 'start');
    });

    test('concurrent starts share one platform request', () async {
      final startResult = Completer<bool>();
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            calls.add(call);
            if (call.method == 'start') return startResult.future;
            return null;
          });
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      final first = keepAlive.start(title: 'Codux', body: 'holding');
      final second = keepAlive.start(title: 'Codux', body: 'holding');
      await Future<void>.delayed(Duration.zero);

      expect(calls.map((call) => call.method).toList(), ['start']);
      startResult.complete(true);
      expect(await first, isTrue);
      expect(await second, isTrue);
      expect(keepAlive.isRunning, isTrue);
    });

    test('concurrent starts share one refusal too', () async {
      final startResult = Completer<bool>();
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(channel, (call) async {
            calls.add(call);
            if (call.method == 'start') return startResult.future;
            return null;
          });
      final keepAlive = BackgroundKeepAlive(channel: channel, supported: true);

      final first = keepAlive.start(title: 'Codux', body: 'holding');
      final second = keepAlive.start(title: 'Codux', body: 'holding');
      await Future<void>.delayed(Duration.zero);
      // Android refuses the foreground service (no permission, background
      // start restriction): the joined caller must not re-ask.
      startResult.complete(false);

      expect(await first, isFalse);
      expect(await second, isFalse);
      expect(calls.map((call) => call.method).toList(), ['start']);
      expect(keepAlive.isRunning, isFalse);
    });
  });
}
