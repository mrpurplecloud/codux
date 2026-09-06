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

    Widget buildView() => MaterialApp(
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
            // Ownership is asked at gesture time, not captured at build time.
            canForwardScroll: () => owned,
            onSendScroll: (data) => forwarded.add('scroll:$data'),
          ),
        ),
      ),
    );
    await tester.pumpWidget(buildView());
    await tester.pump();
    await tester.pump();

    // Not the viewport owner: the gesture must not reach the app (taking the
    // session to deliver it reflows the host grid) and must scroll locally
    // instead of being swallowed.
    await tester.drag(find.byType(SelfDrawnTerminalView), const Offset(0, 120));
    await tester.pump();
    expect(forwarded, isEmpty);
    expect(
      controller.screenSnapshot('session-1')!.displayOffset,
      greaterThan(0),
    );

    // Ownership is regained without a rebuild: the view must ask the sender,
    // not a bool captured when it was last built.
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

  testWidgets('a sub-cell scroll refused by the viewport owner stays local', (
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
    controller.resizeScreen('session-1', cols: 80, rows: 4);

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
              fontSize: 40,
              canForwardScroll: () => false,
              onSendScroll: forwarded.add,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump();

    final origin = tester.getCenter(find.byType(SelfDrawnTerminalView));
    final gesture = await tester.startGesture(origin);
    await gesture.moveBy(const Offset(0, 25));
    await tester.pump();
    await gesture.moveBy(const Offset(0, 10));
    await tester.pump();

    expect(forwarded, isEmpty);
    expect(
      controller.screenSnapshot('session-1')!.scrollPixelOffset,
      greaterThan(0),
    );
    await gesture.up();
  });

  testWidgets('losing the viewport mid-gesture drops the accumulated drag', (
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
    var owned = true;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SizedBox(
            width: 320,
            height: 480,
            child: SelfDrawnTerminalView(
              sessionId: 'session-1',
              controller: controller,
              // 14 * 1.3 = one 18.2px cell.
              repaintSignal: signal,
              fontSize: 14,
              canForwardScroll: () => owned,
              onSendScroll: forwarded.add,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
    await tester.pump();

    // The app owns scrolling, so a full-cell drag would be forwarded.
    expect(
      controller.screenSnapshot('session-1')!.inputMode.mouseTracking,
      isTrue,
    );

    final origin = tester.getCenter(find.byType(SelfDrawnTerminalView));
    final gesture = await tester.startGesture(origin);
    // First move is eaten by the touch slop; the second is owned but under one
    // cell, so it is held for the app until it makes a full tick.
    await gesture.moveBy(const Offset(0, 30));
    await tester.pump();
    await gesture.moveBy(const Offset(0, 12));
    await tester.pump();
    expect(forwarded, isEmpty);
    // The viewport is taken away mid-gesture; this part scrolls locally.
    owned = false;
    await gesture.moveBy(const Offset(0, 20));
    await tester.pump();
    // Owned again: what the local buffer consumed must not be replayed at the
    // app, so this sub-cell movement still owes a tick on its own — with the
    // earlier 12px kept, 12 + 10 would cross the 18.2px cell and fire.
    owned = true;
    await gesture.moveBy(const Offset(0, 10));
    await tester.pump();
    await gesture.up();

    expect(forwarded, isEmpty);
  });
}

/// Mouse tracking on (an app that owns scrolling) plus enough history for the
/// local buffer to have somewhere to scroll.
final String _mouseTrackingHistory =
    '\u001b[?1000h${List.generate(120, (index) => 'line-${index.toString().padLeft(4, '0')}').join('\r\n')}';
