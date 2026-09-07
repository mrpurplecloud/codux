import '../models/remote_models.dart';

/// Identity of one period in which a terminal session is active.
///
/// Equality is intentionally object identity. A session selected again after
/// another session is a new activation even when [sessionId] is unchanged.
final class TerminalActivation {
  TerminalActivation(this.sessionId) {
    if (sessionId.trim().isEmpty) {
      throw ArgumentError.value(sessionId, 'sessionId', 'must not be empty');
    }
  }

  final String sessionId;
}

/// Whether a pane callback still belongs to the activation it was built for.
///
/// Two nulls mean the explicit pre-session bootstrap state. A null on only one
/// side is a stale callback, not a wildcard match.
bool isCurrentTerminalPaneActivation({
  required TerminalActivation? activeActivation,
  required TerminalActivation? paneActivation,
}) => identical(activeActivation, paneActivation);

/// One outstanding request to insert a restored AI session into a terminal.
final class TerminalRestoreIntent {
  TerminalRestoreIntent({required this.requestId, required this.activation}) {
    if (requestId.trim().isEmpty) {
      throw ArgumentError.value(requestId, 'requestId', 'must not be empty');
    }
  }

  final String requestId;
  final TerminalActivation activation;

  bool accepts(
    RelayEnvelope reply, {
    required TerminalActivation? activeActivation,
  }) => reply.requestId == requestId && identical(activeActivation, activation);
}
