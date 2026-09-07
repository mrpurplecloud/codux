import 'dart:async';

import '../models/remote_models.dart';
import 'log_service.dart';
import 'remote_protocol.dart';
import 'terminal_activation.dart';

typedef TerminalInputReliableSend = bool Function(RelayEnvelope message);
typedef TerminalInputActiveActivation = TerminalActivation? Function();
typedef TerminalInputReliableTimerFactory =
    Timer Function(Duration delay, void Function() callback);

class TerminalInputReliableSender {
  TerminalInputReliableSender({
    required TerminalInputReliableSend send,
    required TerminalInputActiveActivation activeActivation,
    this.maxAttempts = 4,
    this.retryBaseDelay = const Duration(milliseconds: 700),
    TerminalInputReliableTimerFactory? timerFactory,
  }) : _send = send,
       _activeActivation = activeActivation,
       _timerFactory = timerFactory ?? Timer.new;

  final TerminalInputReliableSend _send;
  final TerminalInputActiveActivation _activeActivation;
  final TerminalInputReliableTimerFactory _timerFactory;
  final int maxAttempts;
  final Duration retryBaseDelay;

  final Map<String, _PendingTerminalInput> _pending = {};
  int _seq = 0;

  int get pendingCount => _pending.length;

  bool send({
    required TerminalActivation activation,
    required String data,
    required String source,
    bool retry = true,
  }) {
    if (!identical(_activeActivation(), activation) || data.isEmpty) {
      return false;
    }
    final inputId = '${DateTime.now().microsecondsSinceEpoch}-${++_seq}';
    CoduxLog.info(
      '[codux-flutter-input] source=$source id=$inputId bytes=${data.codeUnits.length} session=${activation.sessionId}',
    );
    _pending[inputId] = _PendingTerminalInput(
      inputId: inputId,
      activation: activation,
      data: data,
      source: source,
      retry: retry,
    );
    _sendPending(inputId);
    return true;
  }

  void handleAck(RelayEnvelope message) {
    final payload = message.payload;
    if (payload is! Map) return;
    final inputId = payload['inputId']?.toString();
    if (inputId == null || inputId.isEmpty) return;
    final pending = _pending.remove(inputId);
    pending?.retryTimer?.cancel();
    if (CoduxLog.isDebugEnabled) {
      CoduxLog.debug(
        '[codux-flutter-input] ack id=$inputId ok=${payload['ok'] ?? true}',
      );
    }
  }

  void clear({TerminalActivation? activation}) {
    final entries = _pending.entries.toList();
    for (final entry in entries) {
      if (activation != null &&
          !identical(entry.value.activation, activation)) {
        continue;
      }
      entry.value.retryTimer?.cancel();
      _pending.remove(entry.key);
    }
  }

  void clearSession(String sessionId) {
    final entries = _pending.entries.toList();
    for (final entry in entries) {
      if (entry.value.activation.sessionId != sessionId) continue;
      entry.value.retryTimer?.cancel();
      _pending.remove(entry.key);
    }
  }

  void dispose() {
    clear();
  }

  void _sendPending(String inputId) {
    final pending = _pending[inputId];
    if (pending == null) return;
    final activeActivation = _activeActivation();
    if (!identical(pending.activation, activeActivation)) {
      pending.retryTimer?.cancel();
      _pending.remove(inputId);
      CoduxLog.info(
        '[codux-flutter-input] drop stale id=$inputId pending=${pending.activation.sessionId} active=${activeActivation?.sessionId ?? ''}',
      );
      return;
    }
    pending.retryTimer?.cancel();
    final sent = _send(
      RelayEnvelope(
        type: RemoteMessageType.terminalInput,
        sessionId: pending.activation.sessionId,
        payload: {
          'data': pending.data,
          'inputId': pending.inputId,
          'source': pending.source,
        },
      ),
    );
    if (!sent) {
      CoduxLog.warn('[codux-flutter-input] send failed id=$inputId');
    }
    if (!pending.retry) {
      _pending.remove(inputId);
      return;
    }
    pending.attempt += 1;
    if (pending.attempt >= maxAttempts) {
      _pending.remove(inputId);
      CoduxLog.warn('[codux-flutter-input] ack exhausted id=$inputId');
      return;
    }
    pending.retryTimer = _timerFactory(
      Duration(milliseconds: retryBaseDelay.inMilliseconds * pending.attempt),
      () => _sendPending(inputId),
    );
  }
}

class _PendingTerminalInput {
  _PendingTerminalInput({
    required this.inputId,
    required this.activation,
    required this.data,
    required this.source,
    required this.retry,
  });

  final String inputId;
  final TerminalActivation activation;
  final String data;
  final String source;
  final bool retry;
  int attempt = 0;
  Timer? retryTimer;
}
