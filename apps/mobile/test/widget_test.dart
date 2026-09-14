import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:codux_protocol_ffi/codux_protocol_ffi.dart';
import 'package:codux_flutter/main.dart';
import 'package:codux_flutter/i18n.dart';
import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/log_service.dart';
import 'package:codux_flutter/services/remote_protocol.dart';
import 'package:codux_flutter/services/remote_protocol_service.dart';
import 'package:codux_flutter/services/remote_transport.dart';
import 'package:codux_flutter/theme/app_theme.dart';
import 'package:codux_flutter/widgets/components/device_home_screen.dart';
import 'package:codux_flutter/widgets/components/remote_terminal_pane.dart';
import 'package:codux_flutter/widgets/components/self_drawn_terminal_view.dart';
import 'package:uuid/uuid.dart';

void main() {
  testWidgets('Codux app boots', (WidgetTester tester) async {
    await tester.pumpWidget(const CoduxFlutterApp());
    await tester.pump();
    expect(find.byType(MaterialApp), findsOneWidget);
  });

  testWidgets('device row shows saved relay endpoint without changing header', (
    WidgetTester tester,
  ) async {
    final device = await _fakeDevice();

    await tester.pumpWidget(
      MaterialApp(
        theme: buildAppTheme(),
        home: Scaffold(
          body: AppPreferences(
            accent: AccentChoices.cyan,
            locale: LocaleChoices.english,
            themeMode: ThemeMode.dark,
            child: Directionality(
              textDirection: TextDirection.ltr,
              child: DeviceHomeScreen(
                devices: [device],
                activeDeviceId: device.deviceId,
                ready: false,
                status: 'Off',
                latencyMs: null,
                deviceSubtitle: (_) => 'Relay https://relay.example',
                topInset: 0,
                bottomInset: 0,
                onOpen: (_) {},
                onConnect: (_) {},
                onAdd: () {},
                onEdit: (_) {},
                onDelete: (_) {},
                onRefresh: () async {},
                onSettings: () {},
                onLogs: () {},
                onCheckUpdate: () {},
                onAbout: () {},
              ),
            ),
          ),
        ),
      ),
    );
    await tester.pump();

    expect(find.textContaining('https://relay.example'), findsOneWidget);
    expect(
      find.text('Choose a connected computer to enter terminal'),
      findsOneWidget,
    );
  });

  testWidgets('device row shows global network when endpoint is unavailable', (
    WidgetTester tester,
  ) async {
    final device = StoredDevice(
      server: '',
      hostId: 'host-1',
      deviceId: 'device-1',
      token: 'token',
      name: 'Mac',
      hostName: 'Mac',
    );

    await tester.pumpWidget(
      MaterialApp(
        theme: buildAppTheme(),
        home: Scaffold(
          body: AppPreferences(
            accent: AccentChoices.cyan,
            locale: LocaleChoices.zhCN,
            themeMode: ThemeMode.dark,
            child: Directionality(
              textDirection: TextDirection.ltr,
              child: DeviceHomeScreen(
                devices: [device],
                activeDeviceId: device.deviceId,
                ready: false,
                status: '未连',
                latencyMs: null,
                deviceSubtitle: (_) => tr('device.globalNetwork', 'zh-CN'),
                topInset: 0,
                bottomInset: 0,
                onOpen: (_) {},
                onConnect: (_) {},
                onAdd: () {},
                onEdit: (_) {},
                onDelete: (_) {},
                onRefresh: () async {},
                onSettings: () {},
                onLogs: () {},
                onCheckUpdate: () {},
                onAbout: () {},
              ),
            ),
          ),
        ),
      ),
    );
    await tester.pump();

    expect(find.text('全球网络'), findsOneWidget);
    expect(find.text('Iroh'), findsNothing);
  });

  testWidgets('device row follows current direct endpoint', (
    WidgetTester tester,
  ) async {
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      initialPath: 'relay',
      onSent: (transport, envelope) {
        if (envelope['type'] == RemoteMessageType.hostInfo) {
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    fake.emitState('connected:path=direct;addr=10.0.0.2:51515');
    await tester.pump();

    expect(find.textContaining('10.0.0.2:51515'), findsOneWidget);
    expect(find.text('Codux'), findsOneWidget);
  });

  testWidgets('device row follows current relay url from Iroh path state', (
    WidgetTester tester,
  ) async {
    final tencentRelayUrl = remoteTransportRelayPresets().firstWhere(
      (preset) => preset['key'] == 'china-tencent',
    )['url'];
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      initialPath: 'relay',
      onSent: (transport, envelope) {
        if (envelope['type'] == RemoteMessageType.hostInfo) {
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    fake.emitState(
      'connected:path=relay;addr=https://relay.example;relayUrl=$tencentRelayUrl',
    );
    await tester.pump();

    expect(find.text('China (Tencent Cloud)'), findsOneWidget);
    expect(find.textContaining('$tencentRelayUrl'), findsNothing);
    expect(find.text('Codux'), findsOneWidget);
  });

  testWidgets('device row maps Alibaba relay url to preset name', (
    WidgetTester tester,
  ) async {
    final aliyunRelayUrl = remoteTransportRelayPresets().firstWhere(
      (preset) => preset['key'] == 'china-aliyun',
    )['url'];
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      initialPath: 'relay',
      onSent: (transport, envelope) {
        if (envelope['type'] == RemoteMessageType.hostInfo) {
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    fake.emitState(
      'connected:path=relay;addr=https://relay.example;relayUrl=$aliyunRelayUrl',
    );
    await tester.pump();

    expect(find.text('China (Alibaba Cloud)'), findsOneWidget);
    expect(find.textContaining('$aliyunRelayUrl'), findsNothing);
  });

  testWidgets(
    'opening terminal after list sync asks host to bind missing project terminal',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          sent.add(envelope);
          if (type == 'host.info' || type == 'terminal.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {'terminals': []},
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'project.select') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
            );
            return;
          }
          if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {'project-1': 'session-1'})
                : '${envelope['sessionId'] ?? 'session-1'}';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: const {
                  'data': 'ready',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.pump(const Duration(milliseconds: 200));
      await tester.pumpAndSettle();

      expect(CoduxLog.snapshotText(), contains('terminal.list count=0'));
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      final log = CoduxLog.snapshotText();
      expect(
        log,
        contains(
          'send project.select reason=missing-terminal project=project-1',
        ),
      );
      expect(log, contains('bind session=session-1 project=project-1'));
      expect(log, isNot(contains('request terminal.buffer session=session-1')));
      final subscribePayload = _lastTerminalBaselineSubscribePayload(
        sent,
        sessionId: 'session-1',
      );
      expect(subscribePayload?['resource'], RemoteResourceType.terminals);
      expect(subscribePayload?['baseline'], isTrue);
      expect(subscribePayload?['maxChars'], isA<int>());
      expect(subscribePayload?['chunkChars'], isA<int>());
      expect(
        _lastTerminalProjectSubscribePayload(sent, 'project-1'),
        isNotNull,
      );
      // The self-drawn terminal view reports its viewport size on layout, so a
      // resize is expected on open; the meaningful guarantee (bind via
      // subscription baseline, no terminal.buffer request) is asserted above.
    },
  );

  testWidgets(
    'opening terminal binds the host selected project terminal immediately',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sentTypes = <String>[];
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          sentTypes.add(type);
          sent.add(envelope);
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-2',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-2',
                      'title': 'Terminal',
                      'projectId': 'project-2',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {'project-2': 'session-2'})
                : '${envelope['sessionId'] ?? 'session-2'}';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: const {
                  'data': 'ready',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                },
              ),
            );
            return;
          }
          if (type == RemoteMessageType.terminalViewportClaim) {
            final payload = envelope['payload'];
            if (payload is Map && payload['intent'] == 'force') {
              transport.emitEncrypted(
                const RelayEnvelope(
                  type: 'terminal.viewport.state',
                  sessionId: 'session-2',
                  payload: {
                    'owner': 'remote:device-1',
                    'cols': 100,
                    'rows': 32,
                    'generation': 2,
                  },
                ),
              );
            }
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));
      if (find
          .byKey(const ValueKey('remote-terminal-body'))
          .evaluate()
          .isEmpty) {
        await tester.tap(find.text('Mac').first);
        await tester.pumpAndSettle(const Duration(milliseconds: 300));
        await tester.pump(const Duration(milliseconds: 300));
      }

      final log = CoduxLog.snapshotText();
      expect(log, contains('project.list count=2 selected=project-2'));
      expect(log, contains('bind session=session-2 project=project-2'));
      expect(log, isNot(contains('request terminal.buffer session=session-2')));
      expect(sentTypes.where((type) => type == 'project.select'), isEmpty);
      expect(sent.where(_isTerminalSubscribe).length, 2);
      expect(
        _lastTerminalProjectSubscribePayload(sent, 'project-2'),
        isNotNull,
      );
      final subscribePayload = _lastTerminalBaselineSubscribePayload(
        sent,
        sessionId: 'session-2',
      );
      expect(subscribePayload?['resource'], RemoteResourceType.terminals);
      expect(subscribePayload?['baseline'], isTrue);
      expect(subscribePayload?['maxChars'], isA<int>());
      expect(subscribePayload?['chunkChars'], isA<int>());
      expect(
        _lastPayloadOf(
          sent,
          RemoteMessageType.terminalViewportClaim,
        )?['intent'],
        'auto',
      );
      expect(
        sent
            .where(
              (envelope) =>
                  envelope['type'] == RemoteMessageType.terminalViewportClaim,
            )
            .length,
        1,
      );
      expect(
        sent.where(
          (envelope) =>
              envelope['type'] == RemoteMessageType.terminalViewportResize,
        ),
        isEmpty,
      );

      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'terminal.viewport.state',
          sessionId: 'session-2',
          payload: {
            'owner': 'desktop',
            'cols': 100,
            'rows': 32,
            'generation': 1,
          },
        ),
      );
      await tester.pumpAndSettle();
      const takeOverKey = ValueKey('terminal-take-over');
      expect(find.byKey(takeOverKey), findsOneWidget);

      sent.clear();
      await tester.tap(find.byKey(takeOverKey));
      await tester.pump();
      expect(
        _lastPayloadOf(
          sent,
          RemoteMessageType.terminalViewportClaim,
        )?['intent'],
        'force',
      );
      expect(
        sent.where(
          (envelope) =>
              envelope['type'] == RemoteMessageType.terminalViewportResize,
        ),
        isNotEmpty,
      );
    },
  );

  testWidgets(
    'a handed-away terminal keeps the host grid so TUI frames keep their input row',
    (WidgetTester tester) async {
      // The local cell screen must mirror the grid the host's frames are drawn
      // for, even while another device owns the viewport: a screen that stayed
      // at the phone's shorter row count clamps the TUI's bottom rows onto the
      // last one, so its input row is replaced by the status line below it and
      // no local scroll can bring it back.
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
            // Cold baseline: the raw history is drawn at the host grid, exactly
            // like the host's own screen keyframe.
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: 'session-1',
                payload: {
                  'data': _tuiFrames(_hostGridRows),
                  'screenData': _tuiFrames(_hostGridRows),
                  'buffer': true,
                  'tail': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                },
              ),
            );
            return;
          }
          if (type != 'host.info') return;
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));
      if (find
          .byKey(const ValueKey('remote-terminal-body'))
          .evaluate()
          .isEmpty) {
        await tester.tap(find.text('Mac').first);
        await tester.pumpAndSettle(const Duration(milliseconds: 300));
        await tester.pump(const Duration(milliseconds: 300));
      }

      // The host grid is taller than the phone's own viewport, and the desktop
      // holds the viewport: this phone is only a viewer here.
      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'terminal.viewport.state',
          sessionId: 'session-1',
          payload: {
            'owner': 'desktop',
            'cols': 100,
            'rows': _hostGridRows,
            'generation': 2,
          },
        ),
      );
      await tester.pumpAndSettle();

      // A full redraw at that grid, with the TUI's input box on its last rows.
      fake.emitEncrypted(
        RelayEnvelope(
          type: 'terminal.output',
          sessionId: 'session-1',
          payload: {'data': _tuiFrames(_hostGridRows), 'outputSeq': 3},
        ),
      );
      await tester.pumpAndSettle();

      final pane = tester.widget<RemoteTerminalPane>(
        find.byType(RemoteTerminalPane),
      );
      final snapshot = pane.outputController.screenSnapshot(pane.sessionId!);
      expect(snapshot, isNotNull);
      expect(
        _snapshotGridText(snapshot!),
        contains('> input box'),
        reason:
            'frames drawn for the host grid must survive in the local screen',
      );
      expect(snapshot.rows, _hostGridRows);
    },
  );

  for (final scenario in ['immediate', 'delayed', 'handoff while pending']) {
    testWidgets(
      'baseline before first viewport resize is refreshed at measured mobile size: $scenario',
      (WidgetTester tester) async {
        final device = await _fakeDevice();
        final baselineRows = <int>[];
        final sent = <Map<String, dynamic>>[];
        final pendingBaselines = <RelayEnvelope>[];
        int? measuredRows;
        var viewportGeneration = 1;
        final fake = _FakeRemoteTransport(
          device: device,
          onSent: (transport, envelope) {
            sent.add(envelope);
            final type = '${envelope['type'] ?? ''}';
            if (type == 'host.info') {
              transport.emitEncrypted(
                const RelayEnvelope(
                  type: 'project.list',
                  payload: {
                    'selectedProjectId': 'project-1',
                    'projects': [
                      {
                        'id': 'project-1',
                        'name': 'Project 1',
                        'path': '/tmp/p1',
                      },
                    ],
                  },
                ),
              );
              transport.emitEncrypted(
                const RelayEnvelope(
                  type: 'terminal.list',
                  payload: {
                    'active': 'session-1',
                    'terminals': [
                      {
                        'id': 'session-1',
                        'title': 'Terminal',
                        'projectId': 'project-1',
                      },
                    ],
                  },
                ),
              );
              // Host state can arrive before the pane's first layout callback.
              transport.emitEncrypted(
                const RelayEnvelope(
                  type: 'terminal.viewport.state',
                  sessionId: 'session-1',
                  payload: {
                    'owner': 'remote:device-1',
                    'cols': 100,
                    'rows': _hostGridRows,
                    'generation': 1,
                  },
                ),
              );
              transport.emitEncrypted(
                RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
              );
              return;
            }
            if (_isTerminalBaselineSubscribe(envelope) &&
                envelope['sessionId'] == 'session-1') {
              final payload = envelope['payload'];
              final requestedRows = payload is Map
                  ? payload['viewportRows']
                  : null;
              final rows = requestedRows is int && requestedRows > 0
                  ? requestedRows
                  : _hostGridRows;
              baselineRows.add(rows);
              final baseline = RelayEnvelope(
                type: 'terminal.output',
                sessionId: 'session-1',
                payload: {
                  'data': _tuiFrames(rows),
                  'screenData': _tuiFrames(rows),
                  'buffer': true,
                  'tail': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': baselineRows.length,
                  'requestId': (payload as Map)['requestId'],
                },
              );
              if (scenario != 'immediate' && measuredRows == null) {
                pendingBaselines.add(baseline);
              } else {
                transport.emitEncrypted(baseline);
              }
              return;
            }
            if (type == RemoteMessageType.terminalViewportClaim &&
                (envelope['payload'] as Map?)?['intent'] == 'force') {
              transport.emitEncrypted(
                RelayEnvelope(
                  type: RemoteMessageType.terminalViewportState,
                  sessionId: 'session-1',
                  payload: {
                    'owner': 'remote:device-1',
                    'cols': 100,
                    'rows': _hostGridRows,
                    'generation': ++viewportGeneration,
                  },
                ),
              );
            }
            if (type == RemoteMessageType.terminalViewportResize) {
              final payload = envelope['payload'];
              final rows = payload is Map && payload['rows'] is int
                  ? payload['rows'] as int
                  : _hostGridRows;
              measuredRows = rows;
              viewportGeneration++;
              transport.emitEncrypted(
                RelayEnvelope(
                  type: 'terminal.viewport.state',
                  sessionId: 'session-1',
                  payload: {
                    'owner': 'remote:device-1',
                    'cols': (payload as Map)['cols'],
                    'rows': rows,
                    'generation': viewportGeneration,
                  },
                ),
              );
            }
          },
        );

        await tester.pumpWidget(
          CoduxFlutterApp(
            initialDevices: [device],
            transportFactory: (_) => fake,
          ),
        );
        await tester.pumpAndSettle();
        await tester.tap(find.text('Mac'));
        await tester.pumpAndSettle(const Duration(milliseconds: 300));
        await tester.pump(const Duration(milliseconds: 300));
        if (find
            .byKey(const ValueKey('remote-terminal-body'))
            .evaluate()
            .isEmpty) {
          await tester.tap(find.text('Mac').first);
          await tester.pumpAndSettle(const Duration(milliseconds: 300));
          await tester.pump(const Duration(milliseconds: 300));
        }

        expect(measuredRows, isNotNull);
        expect(measuredRows!, lessThan(_hostGridRows));
        expect(baselineRows, contains(_hostGridRows));
        if (scenario != 'immediate') {
          expect(pendingBaselines, hasLength(1));
          expect(
            baselineRows,
            hasLength(1),
            reason: 'keep the in-flight baseline',
          );
          if (scenario == 'handoff while pending') {
            fake.emitEncrypted(
              RelayEnvelope(
                type: RemoteMessageType.terminalViewportState,
                sessionId: 'session-1',
                payload: {
                  'owner': 'desktop',
                  'cols': 100,
                  'rows': _hostGridRows,
                  'generation': ++viewportGeneration,
                },
              ),
            );
            await tester.pumpAndSettle();
          }
          fake.emitEncrypted(pendingBaselines.single);
          await tester.pumpAndSettle();
          if (scenario == 'handoff while pending') {
            expect(
              baselineRows,
              hasLength(1),
              reason: 'completion must not take the new owner away',
            );
            expect(
              tester
                  .widget<RemoteTerminalPane>(find.byType(RemoteTerminalPane))
                  .handedAway,
              isTrue,
            );
            await tester.tap(find.byKey(const ValueKey('terminal-take-over')));
            await tester.pumpAndSettle();
          }
        }
        expect(baselineRows, contains(measuredRows));
        expect(
          baselineRows.where((rows) => rows == measuredRows),
          hasLength(1),
          reason:
              'matching keyframes do not trigger repeated baseline downloads',
        );

        final pane = tester.widget<RemoteTerminalPane>(
          find.byType(RemoteTerminalPane),
        );
        final snapshot = pane.outputController.screenSnapshot(pane.sessionId!);
        expect(snapshot, isNotNull);
        expect(
          _snapshotGridText(snapshot!),
          contains('> input box'),
          reason: 'measured viewport must receive a matching TUI keyframe',
        );
        expect(snapshot.rows, measuredRows);
        pane.onSendKey('x');
        await tester.pumpAndSettle();
        expect(
          sent.where(
            (e) =>
                e['type'] == RemoteMessageType.terminalInput &&
                e['sessionId'] == 'session-1' &&
                (e['payload'] as Map?)?['data'] == 'x',
          ),
          isNotEmpty,
        );
      },
    );
  }

  testWidgets('new terminal uses one id for create and active selection', (
    WidgetTester tester,
  ) async {
    final sent = <Map<String, dynamic>>[];
    final device = await _fakeDevice();
    late final _FakeRemoteTransport fake;
    fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {
        sent.add(envelope);
        final type = '${envelope['type'] ?? ''}';
        if (type == 'host.info') {
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                    'layoutOrder': 0,
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
          return;
        }
        if (type == RemoteMessageType.terminalCreate) {
          final payload = Map<String, dynamic>.from(envelope['payload'] as Map);
          final terminalId = '${payload['terminalId'] ?? ''}';
          transport.emitEncrypted(
            RelayEnvelope(
              type: RemoteMessageType.terminalCreated,
              sessionId: terminalId,
              payload: {
                'id': terminalId,
                'title': 'Terminal',
                'projectId': 'project-1',
                'layoutOrder': 1,
              },
            ),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
    if (find.byKey(const ValueKey('remote-terminal-body')).evaluate().isEmpty) {
      await tester.tap(find.text('Mac').first);
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
    }
    await tester.tap(find.byIcon(Icons.grid_view_rounded));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const ValueKey('terminal-switcher-add')));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    final createPayload = _lastPayloadOf(
      sent,
      RemoteMessageType.terminalCreate,
    );
    final terminalId = '${createPayload?['terminalId'] ?? ''}';
    expect(Uuid.isValidUUID(fromString: terminalId), isTrue);
    expect(
      find.byKey(ValueKey('terminal-switcher-terminal-$terminalId')),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: find.byKey(ValueKey('terminal-switcher-terminal-$terminalId')),
        matching: find.byIcon(Icons.check_rounded),
      ),
      findsOneWidget,
    );
    expect(
      _lastTerminalBaselineSubscribePayload(sent, sessionId: terminalId),
      isNotNull,
    );
  });

  testWidgets(
    'switching projects remounts cached terminal from the local pty pool',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sentTypes = <String>[];
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          sentTypes.add(type);
          sent.add(envelope);
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal 1',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'project.select') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-2',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal 1',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Terminal 2',
                      'projectId': 'project-2',
                    },
                  ],
                },
              ),
            );
            return;
          }
          if (type == 'terminal.buffer' ||
              _isTerminalBaselineSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {
                    'project-1': 'session-1',
                    'project-2': 'session-2',
                  })
                : '${envelope['sessionId'] ?? 'session-2'}';
            final payload = envelope['payload'];
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: {
                  'data': 'ready',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                  if (payload is Map && payload['requestId'] != null)
                    'requestId': payload['requestId'],
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'terminal.output',
          sessionId: 'session-2',
          payload: {
            'data': 'cached-before-switch',
            'buffer': true,
            'offset': 0,
            'bufferLength': 20,
            'outputSeq': 1,
          },
        ),
      );
      await tester.pump(const Duration(milliseconds: 300));
      sent.clear();
      await _tapProjectTab(tester, 'Project 2');
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      final projectSelectCount = sentTypes
          .where((type) => type == 'project.select')
          .length;
      final log = CoduxLog.snapshotText();
      expect(projectSelectCount, 1);
      expect(
        log,
        contains('send project.select reason=user-select project=project-2'),
      );
      expect(
        log,
        isNot(
          contains(
            'send project.select reason=missing-terminal project=project-2',
          ),
        ),
      );
      expect(log, contains('bind session=session-2 project=project-2'));
      expect(log, isNot(contains('request terminal.buffer session=session-2')));
      final subscribePayload = _lastTerminalSubscribePayload(
        sent,
        sessionId: 'session-2',
      );
      expect(subscribePayload?['resource'], RemoteResourceType.terminals);
      expect(subscribePayload?.containsKey('baseline'), isFalse);
      expect(
        _lastTerminalProjectSubscribePayload(sent, 'project-2'),
        isNotNull,
      );
      // The self-drawn terminal view reports its viewport size on layout, so a
      // resize is expected on remount; the meaningful guarantee (remount from
      // the local pty pool, no terminal.buffer request) is asserted above.
    },
  );

  testWidgets(
    'project switch rehydrates incomplete terminal history before scrolling',
    (WidgetTester tester) async {
      final device = await _fakeDevice();
      final fullHistory = <String>[
        'history-start',
        ...List.generate(80, (index) => 'history-line-$index'),
        'history-end',
      ].join('\r\n');
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal 1',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'project.select') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-2',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal 1',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Terminal 2',
                      'projectId': 'project-2',
                    },
                  ],
                },
              ),
            );
            return;
          }
          if (type == 'terminal.buffer' ||
              _isTerminalBaselineSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {
                    'project-1': 'session-1',
                    'project-2': 'session-2',
                  })
                : '${envelope['sessionId'] ?? 'session-1'}';
            final requestPayload = envelope['payload'];
            final data = sessionId == 'session-2' ? fullHistory : 'ready';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: {
                  'data': data,
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': data.length,
                  'outputSeq': 2,
                  if (requestPayload is Map &&
                      requestPayload['requestId'] != null)
                    'requestId': requestPayload['requestId'],
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'terminal.output',
          sessionId: 'session-2',
          payload: {'data': 'history-end', 'outputSeq': 1},
        ),
      );
      await tester.pump(const Duration(milliseconds: 300));
      await _tapProjectTab(tester, 'Project 2');
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      final viewFinder = find.byType(SelfDrawnTerminalView);
      final view = tester.widget<SelfDrawnTerminalView>(viewFinder);
      final issues = <String>[];
      final cached = view.controller.cachedOutput('session-2') ?? '';
      if (!cached.contains('history-start') ||
          !cached.contains('history-end')) {
        issues.add('terminal history stayed partial');
      }
      final before = view.controller.screenSnapshot('session-2');
      if (before == null || before.totalLines <= before.rows) {
        issues.add('terminal has no scrollback');
      }
      if (before != null) {
        await tester.drag(viewFinder, const Offset(0, 1200));
        await tester.pumpAndSettle();
        final after = view.controller.screenSnapshot('session-2');
        if (after == null || after.displayOffset <= before.displayOffset) {
          issues.add('terminal drag did not move through scrollback');
        }
      }

      expect(issues, isEmpty);
    },
  );

  testWidgets(
    'project tab switch sends host select and immediately binds known terminal',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info' || type == 'project.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                    {'id': 'project-3', 'name': 'Project 3', 'path': '/tmp/p3'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'One',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Two',
                      'projectId': 'project-2',
                    },
                    {
                      'id': 'session-3',
                      'title': 'Three',
                      'projectId': 'project-3',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'terminal.buffer' ||
              _isTerminalBaselineSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {
                    'project-1': 'session-1',
                    'project-2': 'session-2',
                    'project-3': 'session-3',
                  })
                : '${envelope['sessionId'] ?? 'session-1'}';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: const {
                  'data': 'ready',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      sent.clear();

      await _tapProjectTab(tester, 'Project 2');
      await tester.pump(const Duration(milliseconds: 300));

      final log = CoduxLog.snapshotText();
      expect(log, contains('user select project=project-2 previous=project-1'));
      expect(
        log,
        contains('send project.select reason=user-select project=project-2'),
      );
      expect(log, contains('bind session=session-2 project=project-2'));
      expect(
        sent.where((envelope) => envelope['type'] == 'project.select').length,
        1,
      );
      expect(
        sent.where((envelope) {
          return _isTerminalBaselineSubscribe(envelope) &&
              envelope['sessionId'] == 'session-2';
        }).length,
        1,
      );
      expect(
        _lastTerminalProjectSubscribePayload(sent, 'project-2'),
        isNotNull,
      );
    },
  );

  testWidgets(
    'stale project selected ack is ignored during fast project switching',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info' || type == 'project.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                    {'id': 'project-3', 'name': 'Project 3', 'path': '/tmp/p3'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'One',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Two',
                      'projectId': 'project-2',
                    },
                    {
                      'id': 'session-3',
                      'title': 'Three',
                      'projectId': 'project-3',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      sent.clear();

      await _tapProjectTab(tester, 'Project 2');
      await tester.pump(const Duration(milliseconds: 80));
      await tester.tap(find.text('Project 3'));
      await tester.pump(const Duration(milliseconds: 80));
      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'project.selected',
          payload: {'projectId': 'project-2'},
        ),
      );
      fake.emitEncrypted(
        const RelayEnvelope(
          type: 'project.list',
          payload: {
            'selectedProjectId': 'project-2',
            'projects': [
              {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
              {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
              {'id': 'project-3', 'name': 'Project 3', 'path': '/tmp/p3'},
            ],
          },
        ),
      );
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      final log = CoduxLog.snapshotText();
      expect(log, contains('user select project=project-3 previous=project-2'));
      expect(
        log,
        contains(
          'project.selected project=project-2 worktree= current=project-3',
        ),
      );
      expect(log, contains('bind session=session-3 project=project-3'));
      final staleAckOffset = log.indexOf(
        'project.selected project=project-2 worktree= current=project-3',
      );
      expect(staleAckOffset, isNonNegative);
      expect(
        log.substring(staleAckOffset),
        isNot(contains('bind session=session-2 project=project-2')),
      );
      expect(
        sent.where((envelope) => envelope['type'] == 'project.select').length,
        greaterThanOrEqualTo(2),
      );
    },
  );

  testWidgets(
    'accepts out of order encrypted project and terminal list messages',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
              seq: 34,
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                  ],
                },
              ),
              seq: 33,
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
              seq: 35,
            );
            return;
          }
          if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? _sessionIdForSubscribe(envelope, {'project-1': 'session-1'})
                : '${envelope['sessionId'] ?? 'session-1'}';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: const {
                  'data': 'ready',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 5,
                  'outputSeq': 1,
                },
              ),
              seq: 36,
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      final log = CoduxLog.snapshotText();
      expect(log, contains('project.list count=1 selected=project-1'));
      expect(log, contains('terminal.list count=1'));
      expect(log, contains('bind session=session-1 project=project-1'));
    },
  );

  testWidgets('foreground recovery resumes cached remote pty incrementally', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final sent = <Map<String, dynamic>>[];
    final device = await _fakeDevice();
    var terminalBufferCharacters = 5;
    void emitLists(_FakeRemoteTransport transport) {
      transport.emitEncrypted(
        const RelayEnvelope(
          type: 'project.list',
          payload: {
            'selectedProjectId': 'project-1',
            'projects': [
              {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
            ],
          },
        ),
      );
      transport.emitEncrypted(
        RelayEnvelope(
          type: 'terminal.list',
          payload: {
            'terminals': [
              {
                'id': 'session-1',
                'title': 'Terminal',
                'projectId': 'project-1',
                'bufferCharacters': terminalBufferCharacters,
              },
            ],
          },
        ),
      );
    }

    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {
        final type = '${envelope['type'] ?? ''}';
        sent.add(envelope);
        if (type == 'host.info') {
          emitLists(transport);
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
          return;
        }
        if (type == 'terminal.list') {
          emitLists(transport);
          return;
        }
        if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
          final sessionId = _isTerminalSubscribe(envelope)
              ? _sessionIdForSubscribe(envelope, {'project-1': 'session-1'})
              : '${envelope['sessionId'] ?? 'session-1'}';
          transport.emitEncrypted(
            RelayEnvelope(
              type: 'terminal.output',
              sessionId: sessionId,
              payload: {
                'data': 'ready',
                'buffer': true,
                'offset': 0,
                'bufferLength': 5,
                'outputSeq': 1,
                if (envelope['payload'] is Map &&
                    (envelope['payload'] as Map)['requestId'] != null)
                  'requestId': (envelope['payload'] as Map)['requestId'],
              },
            ),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
    await tester.pump(const Duration(milliseconds: 300));
    if (find.byKey(const ValueKey('remote-terminal-body')).evaluate().isEmpty) {
      await tester.tap(find.text('Mac').first);
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));
    }
    sent.clear();

    terminalBufferCharacters = 8;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
    await tester.pump(const Duration(milliseconds: 300));

    expect(_sentTypes(sent), isNot(contains('terminal.buffer')));
    final subscribePayload = _lastTerminalBaselineSubscribePayload(sent);
    expect(subscribePayload?['resource'], RemoteResourceType.terminals);
    expect(subscribePayload?['baseline'], isTrue);
  });

  testWidgets('cached terminal remount does not request a ui buffer', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final sent = <Map<String, dynamic>>[];
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {
        final type = '${envelope['type'] ?? ''}';
        sent.add(envelope);
        if (type == 'host.info') {
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                    'bufferCharacters': 10,
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
          return;
        }
        if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
          final sessionId = _isTerminalSubscribe(envelope)
              ? _sessionIdForSubscribe(envelope, {'project-1': 'session-1'})
              : '${envelope['sessionId'] ?? 'session-1'}';
          final payload = envelope['payload'];
          transport.emitEncrypted(
            RelayEnvelope(
              type: 'terminal.output',
              sessionId: sessionId,
              payload: {
                'data': 'cached',
                'buffer': true,
                'offset': 0,
                'bufferLength': 6,
                'outputSeq': 1,
                if (payload is Map && payload['requestId'] != null)
                  'requestId': payload['requestId'],
              },
            ),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();

    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
    await tester.pump(const Duration(milliseconds: 300));
    if (find.byKey(const ValueKey('remote-terminal-body')).evaluate().isEmpty) {
      await tester.tap(find.text('Mac').first);
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));
    }
    sent.clear();

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
    await tester.pump(const Duration(milliseconds: 300));

    expect(_sentTypes(sent), isNot(contains('terminal.buffer')));
    final subscribePayload = _lastTerminalBaselineSubscribePayload(sent);
    expect(subscribePayload?['resource'], RemoteResourceType.terminals);
    expect(subscribePayload?['baseline'], isTrue);
    expect(
      CoduxLog.snapshotText(),
      isNot(
        contains(
          'request terminal.buffer session=session-1 full=true tail=true',
        ),
      ),
    );
    expect(find.text('terminal.loadingHistory'), findsNothing);
  });

  testWidgets(
    'opening terminal with an empty pool refreshes subscription baseline instead of ui buffer',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          sent.add(envelope);
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      sent.clear();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      final sentTypes = _sentTypes(sent);
      expect(sentTypes, contains(RemoteMessageType.resourceSubscribe));
      expect(sentTypes, isNot(contains(RemoteMessageType.terminalBuffer)));
      final subscribePayload = _lastTerminalBaselineSubscribePayload(
        sent,
        sessionId: 'session-1',
      );
      expect(subscribePayload?['resource'], RemoteResourceType.terminals);
      expect(subscribePayload?['baseline'], isTrue);
      expect(
        CoduxLog.snapshotText(),
        contains('bind session=session-1 project=project-1 cached=false'),
      );
    },
  );

  testWidgets(
    'transport health events degrade direct to relay without clearing runtime',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final device = await _fakeDevice();
      final sent = <Map<String, dynamic>>[];
      final fake = _FakeRemoteTransport(
        device: device,
        initialPath: 'direct',
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'Terminal',
                      'projectId': 'project-1',
                    },
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      final baselineSubscribesBeforeTimeout = sent
          .where(_isTerminalBaselineSubscribe)
          .length;
      final projectListRequestsBeforeTimeout = sent
          .where(
            (envelope) => envelope['type'] == RemoteMessageType.projectList,
          )
          .length;
      final terminalListRequestsBeforeTimeout = sent
          .where(
            (envelope) => envelope['type'] == RemoteMessageType.terminalList,
          )
          .length;
      fake.emitState('latency:timeout=1;path=direct');
      fake.emitState('connected:path=relay');
      await tester.pump();

      expect(
        sent.where(_isTerminalBaselineSubscribe).length,
        baselineSubscribesBeforeTimeout,
      );
      expect(
        sent
            .where(
              (envelope) => envelope['type'] == RemoteMessageType.projectList,
            )
            .length,
        projectListRequestsBeforeTimeout,
      );
      expect(
        sent
            .where(
              (envelope) => envelope['type'] == RemoteMessageType.terminalList,
            )
            .length,
        terminalListRequestsBeforeTimeout,
      );
      final log = CoduxLog.snapshotText();
      expect(log, contains('latency timeout=1;path=direct'));
      expect(log, contains('state=connected detail=path=relay'));
      expect(
        log,
        isNot(contains('reset runtime reason=transport_ping_timeout')),
      );
    },
  );

  testWidgets('latency timeout and background keep last visible rtt', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final device = await _fakeDevice();
    String? latencyPingId;
    final fake = _FakeRemoteTransport(
      device: device,
      initialPath: 'direct',
      onSent: (transport, envelope) {
        final type = '${envelope['type'] ?? ''}';
        if (type == RemoteMessageType.transportPing) {
          final payload = envelope['payload'];
          if (payload is Map) latencyPingId = '${payload['id'] ?? ''}';
          return;
        }
        if (type == 'host.info') {
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    await tester.tap(find.text('Mac'));
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    expect(latencyPingId, isNotNull);
    await tester.pump(const Duration(milliseconds: 17));
    fake.emitEncrypted(
      RelayEnvelope(
        type: RemoteMessageType.transportPong,
        payload: {'id': latencyPingId},
      ),
    );
    await tester.pumpAndSettle();
    final latencyTextFinder = find.byWidgetPredicate((widget) {
      return widget is Text &&
          widget.data != null &&
          RegExp(r'^\d+ms$').hasMatch(widget.data!);
    });
    expect(latencyTextFinder, findsWidgets);
    final visibleLatency = tester
        .widgetList<Text>(latencyTextFinder)
        .map((widget) => widget.data)
        .firstWhere((value) => value != null)!;

    fake.emitState('latency:timeout=1;path=direct');
    await tester.pump();
    expect(find.text(visibleLatency), findsWidgets);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.hidden);
    await tester.pump();
    expect(find.text(visibleLatency), findsWidgets);
  });

  testWidgets(
    'pending project select is not resent after direct path degrades',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      var selectedProjectId = 'project-1';
      final fake = _FakeRemoteTransport(
        device: device,
        initialPath: 'direct',
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info' || type == 'project.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'terminal.list') {
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': _terminalListForProject(selectedProjectId),
                },
              ),
            );
            return;
          }
          if (type == 'project.select') {
            selectedProjectId = 'project-2';
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.selected',
                payload: {'projectId': 'project-2'},
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'One',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Two',
                      'projectId': 'project-2',
                    },
                  ],
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      sent.clear();

      await _tapProjectTab(tester, 'Project 2');
      await tester.pump(const Duration(milliseconds: 300));
      fake.emitState('latency:timeout=1;path=direct');
      fake.emitState('connected:path=relay');
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      expect(
        sent.where((envelope) => envelope['type'] == 'project.select').length,
        1,
      );
      final log = CoduxLog.snapshotText();
      expect(
        log,
        contains('send project.select reason=user-select project=project-2'),
      );
      expect(
        log,
        isNot(
          contains(
            'send project.select reason=path-direct-relay project=project-2',
          ),
        ),
      );
      expect(log, contains('bind session=session-2 project=project-2'));
    },
  );

  testWidgets('transport failure leaves terminal page for device list', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {
        final type = '${envelope['type'] ?? ''}';
        if (type == 'host.info') {
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    for (var attempt = 0; attempt < 2; attempt += 1) {
      if (find
          .byKey(const ValueKey('remote-terminal-body'))
          .evaluate()
          .isNotEmpty) {
        break;
      }
      await tester.tap(find.text('Mac').first);
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
    }

    expect(find.byKey(const ValueKey('remote-terminal-body')), findsOneWidget);

    fake.emitState('failed:network');
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('remote-terminal-body')), findsNothing);
    expect(find.text('Mac'), findsWidgets);
  });

  testWidgets('none transport path leaves terminal page for device list', (
    WidgetTester tester,
  ) async {
    final device = await _fakeDevice();
    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {
        final type = '${envelope['type'] ?? ''}';
        if (type == 'host.info') {
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'project.list',
              payload: {
                'selectedProjectId': 'project-1',
                'projects': [
                  {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                ],
              },
            ),
          );
          transport.emitEncrypted(
            const RelayEnvelope(
              type: 'terminal.list',
              payload: {
                'terminals': [
                  {
                    'id': 'session-1',
                    'title': 'Terminal',
                    'projectId': 'project-1',
                  },
                ],
              },
            ),
          );
          transport.emitEncrypted(
            RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
          );
        }
      },
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pumpAndSettle();
    for (var attempt = 0; attempt < 2; attempt += 1) {
      if (find
          .byKey(const ValueKey('remote-terminal-body'))
          .evaluate()
          .isNotEmpty) {
        break;
      }
      await tester.tap(find.text('Mac').first);
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
    }

    expect(find.byKey(const ValueKey('remote-terminal-body')), findsOneWidget);

    fake.emitState('path:path=none');
    await tester.pumpAndSettle(const Duration(milliseconds: 300));

    expect(find.byKey(const ValueKey('remote-terminal-body')), findsNothing);
    expect(find.text('Mac'), findsWidgets);
  });

  testWidgets(
    'pending project select ack timeout retries project select and refreshes terminal list',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      var selectedProjectId = 'project-1';
      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info' || type == 'project.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'terminal.list') {
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': _terminalListForProject(selectedProjectId),
                },
              ),
            );
            return;
          }
          if (type == 'project.select') {
            return;
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      sent.clear();

      await _tapProjectTab(tester, 'Project 2');
      await tester.pump(const Duration(seconds: 4));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      expect(
        sent.where((envelope) => envelope['type'] == 'project.select').length,
        2,
      );
      final log = CoduxLog.snapshotText();
      expect(log, contains('project.select ack timeout project=project-2'));
      expect(
        log,
        contains('send project.select reason=ack-timeout project=project-2'),
      );
      expect(
        sent.where((envelope) => envelope['type'] == 'terminal.list').length,
        greaterThan(0),
      );
    },
  );

  testWidgets(
    'rejected project select closes bad transport and keeps pending selection',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sent = <Map<String, dynamic>>[];
      final device = await _fakeDevice();
      var selectedProjectId = 'project-1';
      var rejectProjectSelect = true;
      var rejectedProjectSelectDelivery = false;
      final fake = _FakeRemoteTransport(
        device: device,
        onBeforeSend: (envelope) {
          if (envelope['type'] == 'project.select' && rejectProjectSelect) {
            rejectProjectSelect = false;
            rejectedProjectSelectDelivery = true;
            return false;
          }
          return true;
        },
        onSent: (transport, envelope) {
          sent.add(envelope);
          final type = '${envelope['type'] ?? ''}';
          if (type == 'host.info' || type == 'project.list') {
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.list',
                payload: {
                  'selectedProjectId': 'project-1',
                  'projects': [
                    {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
                    {'id': 'project-2', 'name': 'Project 2', 'path': '/tmp/p2'},
                  ],
                },
              ),
            );
            transport.emitEncrypted(
              RelayEnvelope(type: 'host.info', payload: _hostInfoPayload()),
            );
            return;
          }
          if (type == 'terminal.list') {
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': _terminalListForProject(selectedProjectId),
                },
              ),
            );
            return;
          }
          if (type == 'project.select') {
            if (rejectedProjectSelectDelivery) {
              rejectedProjectSelectDelivery = false;
              return;
            }
            selectedProjectId = 'project-2';
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'project.selected',
                payload: {'projectId': 'project-2'},
              ),
            );
            transport.emitEncrypted(
              const RelayEnvelope(
                type: 'terminal.list',
                payload: {
                  'terminals': [
                    {
                      'id': 'session-1',
                      'title': 'One',
                      'projectId': 'project-1',
                    },
                    {
                      'id': 'session-2',
                      'title': 'Two',
                      'projectId': 'project-2',
                    },
                  ],
                },
              ),
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      sent.clear();

      await _tapProjectTab(tester, 'Project 2');
      await tester.pump(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 900));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));

      expect(
        sent.where((envelope) => envelope['type'] == 'project.select').length,
        2,
      );
      final log = CoduxLog.snapshotText();
      expect(log, contains('send result type=project.select'));
      expect(log, contains('result=rejected'));
      expect(
        log,
        contains(
          'project.select delivery failed reason=user-select project=project-2 result=rejected',
        ),
      );
      expect(
        log,
        contains(
          'host unavailable reason=send_rejected:project.select host=host-1 device=device-1',
        ),
      );
      expect(log, contains('send result type=project.select'));
      expect(log, contains('result=delivered'));
      expect(log, contains('bind session=session-2 project=project-2'));
    },
  );

  testWidgets(
    'host runtime instance change clears stale terminal cache and resyncs',
    (WidgetTester tester) async {
      CoduxLog.setLevelName('debug');
      CoduxLog.clear();
      final sentTypes = <String>[];
      final device = await _fakeDevice();
      var hostInfoCount = 0;
      var runtimeId = 'runtime-1';
      void emitCurrentLists(_FakeRemoteTransport transport, int seqBase) {
        final suffix = runtimeId == 'runtime-1' ? 'old' : 'new';
        transport.emitEncrypted(
          const RelayEnvelope(
            type: 'project.list',
            payload: {
              'selectedProjectId': 'project-1',
              'projects': [
                {'id': 'project-1', 'name': 'Project 1', 'path': '/tmp/p1'},
              ],
            },
          ),
          seq: seqBase,
        );
        transport.emitEncrypted(
          RelayEnvelope(
            type: 'terminal.list',
            payload: {
              'terminals': [
                {
                  'id': 'session-$suffix',
                  'title': 'Terminal',
                  'projectId': 'project-1',
                },
              ],
            },
          ),
          seq: seqBase + 1,
        );
      }

      final fake = _FakeRemoteTransport(
        device: device,
        onSent: (transport, envelope) {
          final type = '${envelope['type'] ?? ''}';
          sentTypes.add(type);
          if (type == 'host.info') {
            hostInfoCount += 1;
            runtimeId = hostInfoCount < 3 ? 'runtime-1' : 'runtime-2';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'host.info',
                payload: _hostInfoPayload(runtimeInstanceId: runtimeId),
              ),
              seq: 10 + hostInfoCount,
            );
            if (hostInfoCount == 1 || hostInfoCount == 3) {
              emitCurrentLists(transport, 20 + hostInfoCount);
            }
            return;
          }
          if (type == 'project.list') {
            emitCurrentLists(transport, 60 + sentTypes.length * 2);
            return;
          }
          if (type == 'terminal.list') {
            emitCurrentLists(transport, 80 + sentTypes.length * 2);
            return;
          }
          if (type == 'terminal.buffer' || _isTerminalSubscribe(envelope)) {
            final sessionId = _isTerminalSubscribe(envelope)
                ? (runtimeId == 'runtime-1' ? 'session-old' : 'session-new')
                : '${envelope['sessionId'] ?? ''}';
            transport.emitEncrypted(
              RelayEnvelope(
                type: 'terminal.output',
                sessionId: sessionId,
                payload: {
                  'data': sessionId == 'session-new' ? 'new' : 'old',
                  'buffer': true,
                  'offset': 0,
                  'bufferLength': 3,
                  'outputSeq': 1,
                },
              ),
              seq: 40 + sentTypes.length,
            );
          }
        },
      );

      await tester.pumpWidget(
        CoduxFlutterApp(
          initialDevices: [device],
          transportFactory: (_) => fake,
        ),
      );
      await tester.pumpAndSettle();

      await tester.tap(find.text('Mac'));
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      runtimeId = 'runtime-2';
      fake.emitEncrypted(
        RelayEnvelope(
          type: 'host.info',
          payload: _hostInfoPayload(runtimeInstanceId: 'runtime-2'),
        ),
        seq: 100,
      );
      await tester.pumpAndSettle(const Duration(milliseconds: 300));
      await tester.pump(const Duration(milliseconds: 300));

      final log = CoduxLog.snapshotText();
      expect(
        log,
        contains(
          'reset runtime reason=host-runtime-instance-changed:runtime-1->runtime-2',
        ),
      );
      expect(log, contains('bind session=session-new project=project-1'));
      expect(
        log,
        isNot(contains('request terminal.buffer session=session-new')),
      );
      expect(
        log,
        isNot(
          contains('bind session=session-old project=project-1 cached=true'),
        ),
      );
    },
  );

  testWidgets('unauthorized error stops reconnect loop and prompts repair', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final sent = <Map<String, dynamic>>[];
    final device = await _fakeDevice();
    late final _FakeRemoteTransport fake;
    fake = _FakeRemoteTransport(
      device: device,
      onSent: (_, envelope) => sent.add(envelope),
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pump(const Duration(milliseconds: 200));
    await tester.pumpAndSettle();
    expect(CoduxLog.snapshotText(), contains('request host.info'));

    fake.emit(
      const RelayEnvelope(
        type: 'error',
        payload: {'code': 'device_unauthorized'},
      ),
    );
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pump(const Duration(seconds: 20));

    final log = CoduxLog.snapshotText();
    expect(log, contains('authorization failed code=device_unauthorized'));
    expect(log, isNot(contains('reconnect scheduled')));
    expect(
      log,
      isNot(contains('host unavailable reason=host_response_timeout')),
    );
  });

  testWidgets('startup auto connect waits until after first frame', (
    WidgetTester tester,
  ) async {
    CoduxLog.setLevelName('debug');
    CoduxLog.clear();
    final device = await _fakeDevice();
    var connectCount = 0;
    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (transport, envelope) {},
      onConnect: (_) => connectCount += 1,
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pump();

    expect(connectCount, 0);
    expect(CoduxLog.snapshotText(), isNot(contains('connect start')));

    await tester.pump(const Duration(milliseconds: 200));

    expect(connectCount, 1);
    expect(CoduxLog.snapshotText(), contains('connect start'));
  });

  testWidgets('resume does not restart an in-flight connection', (
    WidgetTester tester,
  ) async {
    final device = await _fakeDevice();
    var connectCount = 0;
    final pending = Completer<void>();
    final fake = _FakeRemoteTransport(
      device: device,
      onSent: (_, _) {},
      onConnect: (_) => connectCount += 1,
      connectFuture: pending.future,
    );

    await tester.pumpWidget(
      CoduxFlutterApp(initialDevices: [device], transportFactory: (_) => fake),
    );
    await tester.pump(const Duration(milliseconds: 200));
    expect(connectCount, 1);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();

    expect(connectCount, 1);
    pending.complete();
  });

  test('mobile languages match Mac language count', () {
    expect(LocaleChoices.all.length, 11);
    expect(LocaleChoices.byId('zh-CN').id, 'simplifiedChinese');
    expect(LocaleChoices.byId('en-US').id, 'english');
    expect(tr('settings.title', 'traditionalChinese'), '設定');
    expect(tr('settings.title', 'japanese'), '設定');
  });

  test('all mobile locales cover the English string catalog', () {
    final keys = _englishI18nKeys();
    expect(keys.length, greaterThan(250));
    for (final locale in LocaleChoices.all.where(
      (item) => item.id != 'system',
    )) {
      for (final key in keys) {
        expect(
          tr(key, locale.id),
          isNot(key),
          reason: 'missing $key for ${locale.id}',
        );
      }
    }
  });

  test('visible strings resolve through i18n fallback', () {
    const keys = [
      'app.connected',
      'app.notConnected',
      'app.about',
      'app.removeDevice',
      'toolbar.upload',
      'toolbar.enter',
      'toolbar.keyboard',
      'project.edit',
      'project.add',
      'project.rebuildTerminal',
      'terminal.loadingHistory',
      'device.homeHint',
      'device.globalNetwork',
      'pair.confirmTitle',
      'update.checking',
      'stats.aiTitle',
      'remote.qrInvalid',
    ];

    for (final locale in LocaleChoices.all.where(
      (item) => item.id != 'system',
    )) {
      for (final key in keys) {
        expect(tr(key, locale.id), isNot(key));
      }
    }
  });
}

Set<String> _englishI18nKeys() {
  final file = File('lib/i18n.dart');
  final source = file.readAsStringSync();
  final match = RegExp(
    r"const Map<String, String> _en = \{([\s\S]*?)\n\};",
  ).firstMatch(source);
  expect(match, isNotNull);
  return RegExp(
    r"^\s+'([^']+)':",
    multiLine: true,
  ).allMatches(match!.group(1)!).map((item) => item.group(1)!).toSet();
}

typedef _FakeEnvelopeHandler =
    void Function(
      _FakeRemoteTransport transport,
      Map<String, dynamic> envelope,
    );
typedef _FakeSendDecision = bool Function(Map<String, dynamic> envelope);
typedef _FakeConnectHandler = void Function(StoredDevice device);

Future<StoredDevice> _fakeDevice() async {
  return const StoredDevice(
    server: 'https://relay.example',
    hostId: 'host-1',
    deviceId: 'device-1',
    token: 'token-1',
    name: 'Mac',
    transports: [
      RemoteTransportCandidate(
        kind: RemoteTransportKind.iroh,
        url: 'https://relay.example',
        nodeId:
            'd75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a',
        relayUrl: 'https://relay.example',
      ),
    ],
  );
}

final class _FakeRemoteTransport implements RemoteTransport {
  _FakeRemoteTransport({
    required this.device,
    required this.onSent,
    this.initialPath = 'relay',
    this.onBeforeSend,
    this.onConnect,
    this.connectFuture,
  });

  final StoredDevice device;
  final _FakeEnvelopeHandler onSent;
  final String initialPath;
  final _FakeSendDecision? onBeforeSend;
  final _FakeConnectHandler? onConnect;
  final Future<void>? connectFuture;
  RemoteTransportStateHandler? _onState;
  RemoteTransportEnvelopeHandler? _onEnvelope;

  @override
  String get kind => RemoteTransportKind.iroh;

  @override
  set onState(RemoteTransportStateHandler? handler) => _onState = handler;

  @override
  set onEnvelope(RemoteTransportEnvelopeHandler? handler) =>
      _onEnvelope = handler;

  @override
  Future<void> connect(StoredDevice device) async {
    onConnect?.call(device);
    _onState?.call('connecting');
    final future = connectFuture;
    if (future != null) await future;
    _onState?.call('connected:path=$initialPath');
    emit(const RelayEnvelope(type: 'hello'));
  }

  @override
  Future<bool> send(Map<String, dynamic> envelope) async {
    final accepted = onBeforeSend?.call(envelope) ?? true;
    onSent(this, envelope);
    if (!accepted) return false;
    return true;
  }

  @override
  Future<bool> sendTerminal(Map<String, dynamic> envelope) async {
    final accepted = onBeforeSend?.call(envelope) ?? true;
    onSent(this, envelope);
    if (!accepted) return false;
    return true;
  }

  @override
  bool sendTerminalUpload({
    required String deviceId,
    required String sessionId,
    required String requestId,
    required String name,
    required String mime,
    required String kind,
    required Uint8List bytes,
  }) {
    return true;
  }

  @override
  Future<void> close() async {}

  void emit(RelayEnvelope envelope) {
    final json = envelope.toJson();
    _onEnvelope?.call(json, jsonEncode(json));
  }

  void emitState(String state) {
    _onState?.call(state);
  }

  void emitEncrypted(RelayEnvelope envelope, {int? seq}) {
    emit(envelope.copyWith(seq: seq ?? DateTime.now().microsecondsSinceEpoch));
  }
}

Map? _lastPayloadOf(List<Map<String, dynamic>> sent, String type) {
  for (final envelope in sent.reversed) {
    if (envelope['type'] == type) return envelope['payload'] as Map?;
  }
  return null;
}

Map? _lastTerminalBaselineSubscribePayload(
  List<Map<String, dynamic>> sent, {
  String? sessionId,
}) {
  for (final envelope in sent.reversed) {
    if (!_isTerminalBaselineSubscribe(envelope)) continue;
    final payload = envelope['payload'];
    if (payload is! Map) continue;
    if (sessionId != null && envelope['sessionId'] != sessionId) continue;
    return payload;
  }
  return null;
}

Map? _lastTerminalSubscribePayload(
  List<Map<String, dynamic>> sent, {
  String? sessionId,
}) {
  for (final envelope in sent.reversed) {
    if (!_isTerminalSubscribe(envelope)) continue;
    final payload = envelope['payload'];
    if (payload is! Map) continue;
    if (sessionId != null && envelope['sessionId'] != sessionId) continue;
    return payload;
  }
  return null;
}

Map? _lastTerminalProjectSubscribePayload(
  List<Map<String, dynamic>> sent,
  String projectId,
) {
  for (final envelope in sent.reversed) {
    if (!_isTerminalSubscribe(envelope) || envelope['sessionId'] != null) {
      continue;
    }
    final payload = envelope['payload'];
    if (payload is Map && payload['projectId'] == projectId) return payload;
  }
  return null;
}

List<String> _sentTypes(List<Map<String, dynamic>> sent) =>
    sent.map((item) => '${item['type'] ?? ''}').toList();

List<Map<String, Object?>> _terminalListForProject(String projectId) {
  final terminals = <Map<String, Object?>>[
    {'id': 'session-1', 'title': 'One', 'projectId': 'project-1'},
  ];
  if (projectId == 'project-2') {
    terminals.add({
      'id': 'session-2',
      'title': 'Two',
      'projectId': 'project-2',
    });
  }
  if (projectId == 'project-3') {
    terminals.add({
      'id': 'session-3',
      'title': 'Three',
      'projectId': 'project-3',
    });
  }
  return terminals;
}

String _sessionIdForSubscribe(
  Map<String, dynamic> envelope,
  Map<String, String> sessionIdByProject,
) {
  final envelopeSessionId = '${envelope['sessionId'] ?? ''}';
  if (envelopeSessionId.isNotEmpty) return envelopeSessionId;
  final payload = envelope['payload'];
  if (payload is Map) {
    final sessionId = '${payload['sessionId'] ?? ''}';
    if (sessionId.isNotEmpty) return sessionId;
  }
  final projectId = payload is Map ? '${payload['projectId'] ?? ''}' : '';
  return sessionIdByProject[projectId] ?? sessionIdByProject.values.first;
}

bool _isTerminalSubscribe(Map<String, dynamic> envelope) {
  if (envelope['type'] != RemoteMessageType.resourceSubscribe) return false;
  final payload = envelope['payload'];
  return payload is Map && payload['resource'] == RemoteResourceType.terminals;
}

bool _isTerminalBaselineSubscribe(Map<String, dynamic> envelope) {
  if (!_isTerminalSubscribe(envelope)) return false;
  final payload = envelope['payload'];
  return payload is Map && payload['baseline'] == true;
}

Future<void> _tapProjectTab(WidgetTester tester, String label) async {
  await tester.pumpAndSettle(const Duration(milliseconds: 300));
  if (find.text(label).evaluate().isEmpty &&
      find.text('Mac').evaluate().isNotEmpty) {
    await tester.tap(find.text('Mac').first);
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
  }
  final finder = find.text(label);
  if (finder.evaluate().isEmpty) {
    await tester.pumpAndSettle(const Duration(milliseconds: 300));
  }
  if (finder.evaluate().isEmpty) {
    try {
      await tester.scrollUntilVisible(
        finder,
        120,
        scrollable: find.byType(Scrollable).first,
        duration: const Duration(milliseconds: 16),
      );
    } catch (_) {
      // Fall through to the final tap so the test failure keeps the standard
      // finder diagnostics.
    }
  }
  await tester.tap(finder);
}

/// Host grid rows for the handed-away grid test: taller than the phone's own
/// measured viewport in the test surface, so a local screen that stayed at the
/// phone's rows must clamp the TUI's bottom rows away.
const _hostGridRows = 32;

/// A full-screen TUI redraw for a [rows]-row grid: alternate screen, mouse
/// tracking, a transcript, and the input box + status line on the last rows --
/// the shape whose bottom rows a shorter local screen clamps away.
String _tuiFrames(int rows) {
  final buffer = StringBuffer('\x1b[?1049h\x1b[?1000h\x1b[H\x1b[2J');
  for (var line = 1; line <= rows - 3; line++) {
    buffer.write('transcript line $line\r\n');
  }
  buffer.write('\x1b[${rows - 1};1H> input box');
  buffer.write('\x1b[$rows;1Hstatus: model-x  42%');
  return buffer.toString();
}

/// Visible text of a cell snapshot, rebuilt row by row the way the renderer
/// does (blank default cells are omitted from the snapshot on purpose).
String _snapshotGridText(TerminalScreenSnapshot snapshot) {
  final rows = <int, Map<int, (String, int)>>{};
  for (final cell in snapshot.cells) {
    rows.putIfAbsent(cell.row, () => {})[cell.col] = (
      cell.text,
      cell.width < 1 ? 1 : cell.width,
    );
  }
  final keys = rows.keys.toList()..sort();
  return keys
      .map((row) {
        final line = StringBuffer();
        var next = 0;
        final columns = rows[row]!.keys.toList()..sort();
        for (final column in columns) {
          final (text, width) = rows[row]![column]!;
          while (next < column) {
            line.write(' ');
            next++;
          }
          line.write(text);
          next += width;
        }
        return line.toString().trimRight();
      })
      .join('\n');
}

Map<String, Object?> _hostInfoPayload({
  String runtimeInstanceId = 'runtime-1',
}) => {
  'protocolVersion': remoteProtocolVersion,
  'runtimeInstanceId': runtimeInstanceId,
  'capabilities': {
    'terminalBuffer': {
      'chunking': true,
      'maxChars': 200000,
      'chunkChars': 16384,
      'requestId': true,
    },
    'terminalViewport': {'ownership': true, 'scroll': true},
  },
};
