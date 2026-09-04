import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/remote_terminal_output_controller.dart';
import 'package:codux_flutter/widgets/components/self_drawn_terminal_view.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('renders the Rust cell snapshot and reports a sane grid size', (
    tester,
  ) async {
    final controller = RemoteTerminalOutputController();
    addTearDown(controller.dispose);
    controller.bindSession('session-1', requireBaseline: true);
    controller.accept(
      const RelayEnvelope(
        type: 'terminal.output',
        sessionId: 'session-1',
        payload: {
          'data': 'hello world',
          'screenData': '[2J[Hhello world',
          'buffer': true,
          'offset': 0,
          'bufferLength': 11,
          'tail': true,
          'outputSeq': 1,
        },
      ),
      activeSessionId: 'session-1',
    );

    final signal = ValueNotifier<int>(0);
    addTearDown(signal.dispose);
    int? reportedCols;
    int? reportedRows;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 320,
            height: 480,
            child: SelfDrawnTerminalView(
              sessionId: 'session-1',
              controller: controller,
              repaintSignal: signal,
              fontSize: 14,
              onResize: (cols, rows) {
                reportedCols = cols;
                reportedRows = rows;
              },
            ),
          ),
        ),
      ),
    );

    // Drain the post-frame resize + snapshot refresh callbacks.
    await tester.pump();
    await tester.pump();

    expect(find.byType(CustomPaint), findsWidgets);
    expect(reportedCols, isNotNull);
    expect(reportedRows, isNotNull);
    expect(reportedCols!, greaterThan(0));
    expect(reportedRows!, greaterThan(0));

    // A new output signal must re-read the snapshot and repaint without error.
    signal.value = 1;
    await tester.pump();
    expect(tester.takeException(), isNull);
  });

  testWidgets('long-press then drag selects text and reports it', (
    tester,
  ) async {
    final controller = RemoteTerminalOutputController();
    addTearDown(controller.dispose);
    controller.bindSession('session-1', requireBaseline: true);
    controller.accept(
      const RelayEnvelope(
        type: 'terminal.output',
        sessionId: 'session-1',
        payload: {
          'data': 'hello world',
          'buffer': true,
          'offset': 0,
          'bufferLength': 11,
          'tail': true,
          'outputSeq': 1,
        },
      ),
      activeSessionId: 'session-1',
    );

    final signal = ValueNotifier<int>(0);
    addTearDown(signal.dispose);
    String? selected = '';

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 320,
            height: 480,
            child: SelfDrawnTerminalView(
              sessionId: 'session-1',
              controller: controller,
              repaintSignal: signal,
              fontSize: 14,
              onSelectionChanged: (text) => selected = text,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump();

    final origin = tester.getTopLeft(find.byType(SelfDrawnTerminalView));
    final gesture = await tester.startGesture(origin + const Offset(6, 8));
    await tester.pump(const Duration(milliseconds: 600)); // long-press fires
    await gesture.moveBy(const Offset(90, 0)); // extend across the first line
    await tester.pump();
    await gesture.up();
    await tester.pump();

    expect(tester.takeException(), isNull);
    expect(selected, isNotNull);
    expect(selected, isNotEmpty);
  });

  testWidgets('renders terminal built-in box drawing cells', (tester) async {
    final controller = RemoteTerminalOutputController();
    addTearDown(controller.dispose);
    controller.bindSession('session-1', requireBaseline: true);
    controller.accept(
      const RelayEnvelope(
        type: 'terminal.output',
        sessionId: 'session-1',
        payload: {
          'data': '┌─┐\n│█│\n└─┘',
          'screenData': '[2J[H┌─┐\r\n│█│\r\n└─┘',
          'buffer': true,
          'offset': 0,
          'bufferLength': 11,
          'tail': true,
          'outputSeq': 1,
        },
      ),
      activeSessionId: 'session-1',
    );

    final signal = ValueNotifier<int>(0);
    addTearDown(signal.dispose);

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 320,
            height: 480,
            child: SelfDrawnTerminalView(
              sessionId: 'session-1',
              controller: controller,
              repaintSignal: signal,
              fontSize: 14,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump();

    expect(find.byType(CustomPaint), findsWidgets);
    expect(tester.takeException(), isNull);
  });

  testWidgets('a scroll the viewport owner refuses stays local', (
    tester,
  ) async {
    final controller = RemoteTerminalOutputController();
    addTearDown(controller.dispose);
    controller.bindSession('session-1', requireBaseline: true);
    controller.accept(
      RelayEnvelope(
        type: 'terminal.output',
        sessionId: 'session-1',
        payload: {
          // Mouse tracking on: an app that owns scrolling (Claude Code, vim).
          'data': _mouseTrackingHistory,
          'screenData': '\u001b[2J\u001b[H\u001b[?1000hline-0119',
          'buffer': true,
          'offset': 0,
          'bufferLength': 1200,
          'tail': true,
          'outputSeq': 1,
        },
      ),
      activeSessionId: 'session-1',
    );

    final signal = ValueNotifier<int>(0);
    addTearDown(signal.dispose);
    final forwarded = <String>[];
    var owned = false;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 320,
            height: 480,
            child: SelfDrawnTerminalView(
              sessionId: 'session-1',
              controller: controller,
              repaintSignal: signal,
              fontSize: 14,
              onSendKey: (data) => forwarded.add('key:$data'),
              onSendScroll: (data) {
                if (!owned) return false;
                forwarded.add('scroll:$data');
                return true;
              },
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump();

    // Not the viewport owner: the gesture must not reach the app (taking the
    // session to deliver it reflows the host grid) and must scroll locally
    // instead of being swallowed.
    await tester.drag(find.byType(SelfDrawnTerminalView), const Offset(0, 120));
    await tester.pump();
    expect(forwarded, isEmpty);
    expect(controller.screenSnapshot('session-1')!.displayOffset, greaterThan(0));

    owned = true;
    final localOffset = controller.screenSnapshot('session-1')!.displayOffset;
    await tester.drag(find.byType(SelfDrawnTerminalView), const Offset(0, 120));
    await tester.pump();
    expect(forwarded, isNotEmpty);
    expect(forwarded.every((entry) => entry.startsWith('scroll:')), isTrue);
    expect(
      controller.screenSnapshot('session-1')!.displayOffset,
      localOffset,
      reason: 'a forwarded scroll belongs to the app, not the local buffer',
    );
    expect(tester.takeException(), isNull);
  });
}

/// Mouse tracking on (an app that owns scrolling) plus enough history for the
/// local buffer to have somewhere to scroll.
final String _mouseTrackingHistory =
    '\u001b[?1000h${List.generate(120, (index) => 'line-${index.toString().padLeft(4, '0')}').join('\r\n')}';
