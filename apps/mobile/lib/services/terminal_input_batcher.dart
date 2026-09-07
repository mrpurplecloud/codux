import 'dart:async';

import 'terminal_activation.dart';

typedef TerminalInputTimerFactory =
    Timer Function(Duration delay, void Function() callback);
typedef TerminalInputBatchSend =
    void Function(TerminalActivation activation, String data);
typedef TerminalInputActiveActivation = TerminalActivation? Function();

class TerminalInputBatcher {
  TerminalInputBatcher({
    required TerminalInputBatchSend send,
    required TerminalInputActiveActivation activeActivation,
    this.flushDelay = const Duration(milliseconds: 2),
    this.maxBatchCharacters = 128,
    TerminalInputTimerFactory? timerFactory,
  }) : _send = send,
       _activeActivation = activeActivation,
       _timerFactory = timerFactory ?? Timer.new;

  final TerminalInputBatchSend _send;
  final TerminalInputActiveActivation _activeActivation;
  final Duration flushDelay;
  final int maxBatchCharacters;
  final TerminalInputTimerFactory _timerFactory;

  final StringBuffer _buffer = StringBuffer();
  Timer? _flushTimer;
  TerminalActivation? _activation;

  String get pendingData => _buffer.toString();
  bool get hasPendingData => _buffer.isNotEmpty;

  void add({required TerminalActivation activation, required String data}) {
    if (data.isEmpty || !identical(_activeActivation(), activation)) return;
    if (_buffer.isNotEmpty && !identical(_activation, activation)) {
      reset();
    }
    if (_shouldSendImmediately(data)) {
      flush(activation: activation);
      if (identical(_activeActivation(), activation)) {
        _send(activation, data);
      }
      return;
    }
    _activation = activation;
    _buffer.write(data);
    if (_buffer.length >= maxBatchCharacters) {
      flush(activation: activation);
      return;
    }
    _scheduleFlush(activation);
  }

  bool flush({required TerminalActivation activation}) {
    if (!identical(_activeActivation(), activation)) {
      if (identical(_activation, activation)) reset();
      return false;
    }
    if (!identical(_activation, activation)) return false;
    _flushTimer?.cancel();
    _flushTimer = null;
    if (_buffer.isEmpty) return false;
    final data = _buffer.toString();
    _buffer.clear();
    _activation = null;
    _send(activation, data);
    return true;
  }

  void reset() {
    _flushTimer?.cancel();
    _flushTimer = null;
    _buffer.clear();
    _activation = null;
  }

  void dispose() {
    reset();
  }

  void _scheduleFlush(TerminalActivation activation) {
    _flushTimer?.cancel();
    _flushTimer = _timerFactory(flushDelay, () {
      flush(activation: activation);
    });
  }

  bool _shouldSendImmediately(String data) {
    if (data.runes.length == 1) return true;
    for (final codeUnit in data.codeUnits) {
      if (codeUnit < 0x20 || codeUnit == 0x7f) return true;
    }
    return false;
  }
}
