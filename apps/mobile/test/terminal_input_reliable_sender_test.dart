import 'dart:async';

import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/terminal_activation.dart';
import 'package:codux_flutter/services/terminal_input_reliable_sender.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('sends terminal input with input id and retries until ack', () {
    final timers = <_FakeTimer>[];
    final sent = <RelayEnvelope>[];
    final activation = TerminalActivation('session-1');
    final sender = TerminalInputReliableSender(
      activeActivation: () => activation,
      retryBaseDelay: const Duration(milliseconds: 10),
      send: (message) {
        sent.add(message);
        return true;
      },
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    expect(
      sender.send(activation: activation, data: 'a', source: 'key'),
      isTrue,
    );
    expect(sent.single.type, 'terminal.input');
    expect(sent.single.sessionId, 'session-1');
    expect((sent.single.payload as Map)['data'], 'a');
    expect((sent.single.payload as Map)['source'], 'key');

    timers.single.fire();

    expect(sent.length, 2);
    sender.handleAck(
      RelayEnvelope(
        type: 'terminal.input.ack',
        payload: {'inputId': (sent.last.payload as Map)['inputId']},
      ),
    );
    expect(sender.pendingCount, 0);
  });

  test('does not revive an old retry after A-B-A activation changes', () {
    final timers = <_FakeTimer>[];
    final sent = <RelayEnvelope>[];
    final firstA = TerminalActivation('session-a');
    var activeActivation = firstA;
    final sender = TerminalInputReliableSender(
      activeActivation: () => activeActivation,
      send: (message) {
        sent.add(message);
        return true;
      },
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    sender.send(activation: firstA, data: 'a', source: 'key');
    activeActivation = TerminalActivation('session-b');
    activeActivation = TerminalActivation('session-a');
    timers.single.fire();

    expect(sent.length, 1);
    expect(sender.pendingCount, 0);
    expect(timers.single.isActive, isFalse);
    expect(timers.single.wasCancelled, isTrue);
  });

  test('drops pending input when active session disappears', () {
    final timers = <_FakeTimer>[];
    final activation = TerminalActivation('session-1');
    TerminalActivation? activeActivation = activation;
    final sender = TerminalInputReliableSender(
      activeActivation: () => activeActivation,
      send: (_) => true,
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    sender.send(activation: activation, data: 'a', source: 'key');
    activeActivation = null;
    timers.single.fire();

    expect(sender.pendingCount, 0);
    expect(timers.single.isActive, isFalse);
    expect(timers.single.wasCancelled, isTrue);
  });

  test('clear cancels pending retry timers', () {
    final timers = <_FakeTimer>[];
    final activation = TerminalActivation('session-1');
    final sender = TerminalInputReliableSender(
      activeActivation: () => activation,
      send: (_) => true,
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    sender.send(activation: activation, data: 'a', source: 'key');
    sender.clear(activation: activation);

    expect(sender.pendingCount, 0);
    expect(timers.single.isActive, isFalse);
  });

  test('non retrying control input is sent once and not retained', () {
    final timers = <_FakeTimer>[];
    final sent = <RelayEnvelope>[];
    final activation = TerminalActivation('session-1');
    final sender = TerminalInputReliableSender(
      activeActivation: () => activation,
      send: (message) {
        sent.add(message);
        return true;
      },
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    final ok = sender.send(
      activation: activation,
      data: '\u0003',
      source: 'key',
      retry: false,
    );

    expect(ok, isTrue);
    expect(sent, hasLength(1));
    expect((sent.single.payload as Map)['data'], '\u0003');
    expect(sender.pendingCount, 0);
    expect(timers, isEmpty);
  });
}

final class _FakeTimer implements Timer {
  _FakeTimer(this._callback);

  final void Function() _callback;
  var _active = true;
  var wasCancelled = false;

  void fire() {
    if (!_active) return;
    _active = false;
    _callback();
  }

  @override
  void cancel() {
    wasCancelled = true;
    _active = false;
  }

  @override
  bool get isActive => _active;

  @override
  int get tick => 0;
}
