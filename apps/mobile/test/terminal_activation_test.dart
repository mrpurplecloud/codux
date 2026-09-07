import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/terminal_activation.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('stale pane resize cannot authorize readiness or mount', () {
    final current = TerminalActivation('session-a');
    TerminalActivation? active = current;
    var terminalReady = false;
    var scheduledMounts = 0;

    void acceptResize(TerminalActivation? paneActivation) {
      if (!isCurrentTerminalPaneActivation(
        activeActivation: active,
        paneActivation: paneActivation,
      )) {
        return;
      }
      terminalReady = true;
      scheduledMounts += 1;
    }

    acceptResize(TerminalActivation('session-a'));
    expect(terminalReady, isFalse);
    expect(scheduledMounts, 0);

    active = null;
    acceptResize(null);
    expect(terminalReady, isTrue);
    expect(scheduledMounts, 1);
  });

  test('restore intent requires both request and activation ownership', () {
    final firstA = TerminalActivation('session-a');
    final intent = TerminalRestoreIntent(
      requestId: 'restore-1',
      activation: firstA,
    );
    const reply = RelayEnvelope(
      type: 'ai.session.result',
      requestId: 'restore-1',
    );

    expect(intent.accepts(reply, activeActivation: firstA), isTrue);
    expect(
      intent.accepts(reply, activeActivation: TerminalActivation('session-a')),
      isFalse,
    );
    expect(
      intent.accepts(
        const RelayEnvelope(
          type: 'ai.session.result',
          requestId: 'restore-old',
        ),
        activeActivation: firstA,
      ),
      isFalse,
    );
  });
}
