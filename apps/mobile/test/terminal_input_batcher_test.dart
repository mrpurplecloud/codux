import 'dart:async';

import 'package:codux_flutter/services/terminal_activation.dart';
import 'package:codux_flutter/services/terminal_input_batcher.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('sends single printable keystrokes immediately', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final activation = TerminalActivation('session-1');
    final batcher = TerminalInputBatcher(
      activeActivation: () => activation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: activation, data: 'l');
    batcher.add(activation: activation, data: 's');

    expect(sent, ['session-1:l', 'session-1:s']);
    expect(batcher.hasPendingData, isFalse);
    expect(timers, isEmpty);
  });

  test('coalesces multi-character printable input until delay fires', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final activation = TerminalActivation('session-1');
    final batcher = TerminalInputBatcher(
      activeActivation: () => activation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: activation, data: 'gi');
    batcher.add(activation: activation, data: 't ');

    expect(sent, isEmpty);
    expect(batcher.pendingData, 'git ');

    timers.last.fire();

    expect(sent, ['session-1:git ']);
    expect(batcher.hasPendingData, isFalse);
  });

  test('discards old owner before buffering input for a new activation', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final activationA = TerminalActivation('session-a');
    var activeActivation = activationA;
    final batcher = TerminalInputBatcher(
      activeActivation: () => activeActivation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: activationA, data: 'old input');
    final activationB = TerminalActivation('session-b');
    activeActivation = activationB;
    batcher.add(activation: activationB, data: 'new input');

    expect(sent, isEmpty);
    expect(batcher.pendingData, 'new input');
    timers.last.fire();
    expect(sent, ['session-b:new input']);
  });

  test('flushes pending printable input before control keys', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final activationA = TerminalActivation('session-a');
    var activeActivation = activationA;
    final batcher = TerminalInputBatcher(
      activeActivation: () => activeActivation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: activationA, data: 'git status');
    final activationB = TerminalActivation('session-b');
    activeActivation = activationB;
    batcher.add(activation: activationB, data: '\r');

    expect(sent, ['session-b:\r']);
    expect(batcher.hasPendingData, isFalse);
    timers.single.fire();
    expect(sent, ['session-b:\r']);
  });

  test('stale control input cannot flush the current activation', () {
    final sent = <String>[];
    final stale = TerminalActivation('session-a');
    final current = TerminalActivation('session-b');
    final batcher = TerminalInputBatcher(
      activeActivation: () => current,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
    );

    batcher.add(activation: current, data: 'current input');
    batcher.add(activation: stale, data: '\r');

    expect(sent, isEmpty);
    expect(batcher.pendingData, 'current input');
  });

  test('old timer cannot send after A-B-A activation replacement', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final firstA = TerminalActivation('session-a');
    var activeActivation = firstA;
    final batcher = TerminalInputBatcher(
      activeActivation: () => activeActivation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: firstA, data: 'old input');
    activeActivation = TerminalActivation('session-b');
    activeActivation = TerminalActivation('session-a');
    timers.single.fire();

    expect(sent, isEmpty);
    expect(batcher.hasPendingData, isFalse);
  });

  test('flushes when batch reaches size limit', () {
    final sent = <String>[];
    final activation = TerminalActivation('session-1');
    final batcher = TerminalInputBatcher(
      activeActivation: () => activation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      maxBatchCharacters: 4,
    );

    batcher.add(activation: activation, data: 'ab');
    batcher.add(activation: activation, data: 'cd');

    expect(sent, ['session-1:abcd']);
    expect(batcher.hasPendingData, isFalse);
  });

  test('reset discards pending input', () {
    final timers = <_FakeTimer>[];
    final sent = <String>[];
    final activation = TerminalActivation('session-1');
    final batcher = TerminalInputBatcher(
      activeActivation: () => activation,
      send: (owner, data) => sent.add('${owner.sessionId}:$data'),
      timerFactory: (delay, callback) {
        final timer = _FakeTimer(callback);
        timers.add(timer);
        return timer;
      },
    );

    batcher.add(activation: activation, data: 'abc');
    batcher.reset();
    timers.single.fire();

    expect(sent, isEmpty);
    expect(batcher.hasPendingData, isFalse);
  });
}

final class _FakeTimer implements Timer {
  _FakeTimer(this._callback);

  final void Function() _callback;
  var _active = true;

  void fire() {
    if (!_active) return;
    _active = false;
    _callback();
  }

  @override
  void cancel() {
    _active = false;
  }

  @override
  bool get isActive => _active;

  @override
  int get tick => 0;
}
