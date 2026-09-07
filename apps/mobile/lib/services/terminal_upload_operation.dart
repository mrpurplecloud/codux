import 'dart:async';

import '../models/remote_models.dart';

enum TerminalUploadErrorDisposition { notUploadError, matched, stale }

TerminalUploadErrorDisposition classifyTerminalUploadError<T extends Object>(
  RelayEnvelope message, {
  required TerminalUploadOperation<T>? activeOperation,
  required String? activeSessionId,
  required String? activeDeviceId,
  required T? activeTransport,
  required bool mounted,
  required bool canUpload,
}) {
  final payload = message.payload;
  final code = payload is Map ? '${payload['code'] ?? ''}' : '';
  if (code != 'terminal_upload_failed') {
    return TerminalUploadErrorDisposition.notUploadError;
  }
  final operation = activeOperation;
  if (operation == null ||
      !operation.acceptsReply(
        message,
        activeOperation: operation,
        activeSessionId: activeSessionId,
        activeDeviceId: activeDeviceId,
        activeTransport: activeTransport,
        mounted: mounted,
        canUpload: canUpload,
      )) {
    return TerminalUploadErrorDisposition.stale;
  }
  return TerminalUploadErrorDisposition.matched;
}

/// Scope in which a legacy, uncorrelated upload reply can be ambiguous.
final class TerminalUploadScope {
  const TerminalUploadScope({
    required this.hostId,
    required this.deviceId,
    required this.sessionId,
    required this.runtimeInstanceId,
  });

  final String hostId;
  final String deviceId;
  final String sessionId;
  final String? runtimeInstanceId;

  TerminalUploadScope copyWith({
    String? hostId,
    String? deviceId,
    String? sessionId,
    String? runtimeInstanceId,
  }) => TerminalUploadScope(
    hostId: hostId ?? this.hostId,
    deviceId: deviceId ?? this.deviceId,
    sessionId: sessionId ?? this.sessionId,
    runtimeInstanceId: runtimeInstanceId ?? this.runtimeInstanceId,
  );

  @override
  bool operator ==(Object other) =>
      other is TerminalUploadScope &&
      other.hostId == hostId &&
      other.deviceId == deviceId &&
      other.sessionId == sessionId &&
      other.runtimeInstanceId == runtimeInstanceId;

  @override
  int get hashCode =>
      Object.hash(hostId, deviceId, sessionId, runtimeInstanceId);
}

/// Immutable owner of one terminal upload across picker, transport and reply
/// awaits. The active slot, not a generation counter, decides ownership.
final class TerminalUploadOperation<T extends Object> {
  TerminalUploadOperation({
    required this.requestId,
    required this.hostId,
    required this.sessionId,
    required this.deviceId,
    required this.transport,
    required this.requestIdSupported,
    required this.runtimeInstanceId,
  });

  final String requestId;
  final String hostId;
  final String sessionId;
  final String deviceId;
  final T transport;
  final bool requestIdSupported;
  final String? runtimeInstanceId;
  final Completer<void> completion = Completer<void>();
  bool _sent = false;
  bool _completedFromReply = false;

  TerminalUploadScope get scope => TerminalUploadScope(
    hostId: hostId,
    deviceId: deviceId,
    sessionId: sessionId,
    runtimeInstanceId: runtimeInstanceId,
  );

  bool get wasSent => _sent;
  bool get completedFromReply => _completedFromReply;

  bool ownsContext({
    required TerminalUploadOperation<T>? activeOperation,
    required String? activeSessionId,
    required String? activeDeviceId,
    required T? activeTransport,
    required bool mounted,
    required bool canUpload,
  }) =>
      mounted &&
      canUpload &&
      identical(activeOperation, this) &&
      activeSessionId == sessionId &&
      activeDeviceId == deviceId &&
      identical(activeTransport, transport);

  bool acceptsReply(
    RelayEnvelope message, {
    required TerminalUploadOperation<T>? activeOperation,
    required String? activeSessionId,
    required String? activeDeviceId,
    required T? activeTransport,
    required bool mounted,
    required bool canUpload,
  }) =>
      _sent &&
      !completion.isCompleted &&
      _matchesRequestId(message.requestId) &&
      message.sessionId == sessionId &&
      message.deviceId == deviceId &&
      ownsContext(
        activeOperation: activeOperation,
        activeSessionId: activeSessionId,
        activeDeviceId: activeDeviceId,
        activeTransport: activeTransport,
        mounted: mounted,
        canUpload: canUpload,
      );

  bool _matchesRequestId(String? replyRequestId) {
    final normalized = replyRequestId?.trim() ?? '';
    if (normalized.isNotEmpty) return normalized == requestId;
    return !requestIdSupported;
  }

  void markSent() {
    _sent = true;
  }

  void completeFromReply() {
    _completedFromReply = true;
    complete();
  }

  void complete() {
    if (!completion.isCompleted) completion.complete();
  }
}

/// Prevents a late legacy reply from completing a later upload in the same
/// host/device/session scope. A terminal reply or confirmed host restart ends
/// the ambiguity.
final class TerminalUploadReplyGuard {
  final Set<TerminalUploadScope> _ambiguousLegacyScopes = {};

  bool canStart(
    TerminalUploadScope scope, {
    required bool requestIdSupported,
  }) => requestIdSupported || !_ambiguousLegacyScopes.contains(scope);

  void retire<T extends Object>(TerminalUploadOperation<T> operation) {
    if (operation.requestIdSupported ||
        !operation.wasSent ||
        operation.completedFromReply) {
      return;
    }
    _ambiguousLegacyScopes.add(operation.scope);
  }

  bool consumeAmbiguousLegacyReply(
    RelayEnvelope message, {
    required String? hostId,
    required String? runtimeInstanceId,
  }) {
    if ((message.requestId?.trim() ?? '').isNotEmpty) return false;
    final payload = message.payload;
    final isTerminalReply =
        message.type == 'terminal.uploaded' ||
        (message.type == 'error' &&
            payload is Map &&
            payload['code'] == 'terminal_upload_failed');
    if (!isTerminalReply) return false;
    final normalizedHostId = hostId?.trim() ?? '';
    final deviceId = message.deviceId?.trim() ?? '';
    final sessionId = message.sessionId?.trim() ?? '';
    if (normalizedHostId.isEmpty || deviceId.isEmpty || sessionId.isEmpty) {
      return false;
    }
    return _ambiguousLegacyScopes.remove(
      TerminalUploadScope(
        hostId: normalizedHostId,
        deviceId: deviceId,
        sessionId: sessionId,
        runtimeInstanceId: runtimeInstanceId,
      ),
    );
  }

  void resetAfterHostRestart(String hostId) {
    _ambiguousLegacyScopes.removeWhere((scope) => scope.hostId == hostId);
  }
}
