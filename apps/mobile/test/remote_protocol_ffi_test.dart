import 'dart:convert';

import 'package:codux_flutter/services/remote_protocol_service.dart';
import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_protocol_ffi/codux_protocol_ffi.dart'
    as codux_protocol_ffi;
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('Rust FFI protocol names match Dart compile-time constants', () {
    expect(codux_protocol_ffi.protocolVersion(), remoteProtocolVersion);
    expect(
      codux_protocol_ffi.messageType('resourceSubscribe'),
      RemoteMessageType.resourceSubscribe,
    );
    expect(
      codux_protocol_ffi.messageType('resourceUnsubscribe'),
      RemoteMessageType.resourceUnsubscribe,
    );
    expect(
      codux_protocol_ffi.resourceType('terminals'),
      RemoteResourceType.terminals,
    );
    expect(codux_protocol_ffi.transportKind('iroh'), RemoteTransportKind.iroh);
    expect(
      codux_protocol_ffi.messageType('terminalBuffer'),
      RemoteMessageType.terminalBuffer,
    );
    expect(
      codux_protocol_ffi.messageType('gitStatus'),
      RemoteMessageType.gitStatus,
    );
  });

  test('Rust FFI builds terminal resource subscribe envelope', () {
    final envelope = codux_protocol_ffi.resourceSubscribeEnvelope(
      resource: RemoteResourceType.terminals,
      projectId: 'project-1',
      baseline: true,
      maxChars: 65536,
      chunkChars: 16384,
    );

    expect(envelope['type'], RemoteMessageType.resourceSubscribe);
    expect(envelope['sessionId'], isNull);
    final payload = envelope['payload'] as Map;
    expect(payload['resource'], RemoteResourceType.terminals);
    expect(payload['projectId'], 'project-1');
    expect(payload['baseline'], isTrue);
    expect(payload['maxChars'], 65536);
    expect(payload['chunkChars'], 16384);
  });

  test('Rust FFI owns controller transport URL and selection rules', () {
    final relayPresets = codux_protocol_ffi.transportRelayPresets();
    final tencentRelayUrl = relayPresets.firstWhere(
      (preset) => preset['key'] == 'china-tencent',
    )['url'];
    final aliyunRelayUrl = relayPresets.firstWhere(
      (preset) => preset['key'] == 'china-aliyun',
    )['url'];

    expect(
      codux_protocol_ffi.transportRelayUrl('https://relay.example'),
      'https://relay.example',
    );
    expect(
      codux_protocol_ffi.transportRelayUrlForPreset(preset: 'china'),
      tencentRelayUrl,
    );
    expect(
      codux_protocol_ffi.transportRelayUrlForPreset(preset: 'china-aliyun'),
      aliyunRelayUrl,
    );
    expect(codux_protocol_ffi.transportRelayUrlForPreset(preset: 'global'), '');
    expect(
      relayPresets.map((preset) => preset['key']).toList(),
      containsAll(['global', 'china-tencent', 'china-aliyun', 'custom']),
    );
    final transports = [
      {
        'kind': RemoteTransportKind.iroh,
        'url': 'https://relay.example',
        'nodeId': 'node-1',
        'relayUrl': 'https://relay.example',
      },
    ];
    expect(
      codux_protocol_ffi.preferredTransportKind(transports, pairing: true),
      RemoteTransportKind.iroh,
    );
    expect(
      codux_protocol_ffi.preferredTransportKind(transports, pairing: false),
      RemoteTransportKind.iroh,
    );
    expect(
      codux_protocol_ffi.preferredTransportKind([
        {'kind': RemoteTransportKind.iroh, 'ticket': 'endpointabc'},
      ], pairing: false),
      RemoteTransportKind.iroh,
    );
  });

  test('Rust FFI summarizes controller transport config', () {
    final summary = codux_protocol_ffi.controllerTransportConfigSummary({
      'relayUrl': 'https://relay.example',
      'hostId': 'host-1',
      'deviceId': 'device-1',
      'deviceToken': 'token-1',
      'transports': [
        {
          'kind': RemoteTransportKind.iroh,
          'url': 'https://relay.example',
          'nodeId': 'node-1',
          'relayUrl': 'https://relay.example',
        },
      ],
    });

    expect(summary['relayUrl'], 'https://relay.example');
    expect(summary['hostId'], 'host-1');
    expect(summary['deviceId'], 'device-1');
    expect(summary['transportKind'], RemoteTransportKind.iroh);
    expect(summary['transportCount'], 1);
    expect(summary.containsKey('stunCount'), isFalse);
  });

  test('Rust FFI terminal input normalizes IME committed text', () {
    expect(codux_protocol_ffi.terminalTextInput('abc'), 'abc');
    expect(codux_protocol_ffi.terminalTextInput('你好かな한글'), '你好かな한글');
    expect(codux_protocol_ffi.terminalTextInput('\u0008'), '\u007f');
    expect(codux_protocol_ffi.terminalTextInput('\n'), '\r');
    expect(codux_protocol_ffi.terminalTextInput('a\u{f700}b'), 'ab');
    expect(codux_protocol_ffi.terminalInsertInput('\u007f'), '\u007f');
    expect(
      codux_protocol_ffi.terminalInsertInput('paste\ntext'),
      '\u001b[200~paste\ntext\u001b[201~',
    );
  });

  test('Rust FFI terminal input maps special keys and app cursor mode', () {
    expect(codux_protocol_ffi.terminalKeyInput(key: 'backspace'), '\u007f');
    expect(codux_protocol_ffi.terminalKeyInput(key: 'enter'), '\r');
    expect(codux_protocol_ffi.terminalKeyInput(key: 'up'), '\u001b[A');
    expect(
      codux_protocol_ffi.terminalKeyInput(key: 'up', applicationCursor: true),
      '\u001bOA',
    );
  });

  test('snapshot omits default cell fields and the decoder restores them', () {
    // The Rust snapshot leaves every default-valued cell field off the wire (a
    // full screen is thousands of cells, almost all of them plain). This is the
    // consumer half of that contract: an omitted field must decode to the
    // default, and a present one must survive. Getting a default wrong here
    // renders silently wrong -- nothing throws -- so it is pinned here rather
    // than left to the renderer's incidental null handling.
    //
    // The Rust half is `cell_snapshot_omits_exactly_the_default_fields` in
    // crates/codux-terminal-core/src/tests.rs.
    final router = codux_protocol_ffi.RemoteOutputRouter(
      maxBufferChars: 65536,
      maxCachedChars: 65536,
    );
    addTearDown(router.dispose);
    router.bindSession('session-1', requireBaseline: true);
    router.resizeScreen('session-1', cols: 20, rows: 4);
    router.accept(
      jsonEncode({
        'type': 'terminal.output',
        'sessionId': 'session-1',
        'payload': {
          'data': '',
          // "ab" plain, then bold red "CD", then a wide CJK glyph on row 2.
          'screenData':
              '\u001b[2J\u001b[Hab\u001b[1;31mCD\u001b[0m\u001b[2;1H字',
          'buffer': true,
          'offset': 0,
          'bufferLength': 0,
          'tail': true,
          'outputSeq': 1,
        },
      }),
      'session-1',
    );

    final snapshot = router.screenSnapshot('session-1');
    expect(snapshot, isNotNull);
    // The repaint string is not built for consumer screens, so it never arrives.
    expect(snapshot!.data, isEmpty);

    codux_protocol_ffi.TerminalScreenCell cellAt(int row, int col) =>
        snapshot.cells.firstWhere(
          (cell) => cell.row == row && cell.col == col,
          orElse: () => fail('no cell at row $row col $col'),
        );

    // Omitted fields decode to their defaults -- note width, which must be 1
    // and not the 0 a bare int-parse of null would give.
    final plain = cellAt(0, 0);
    expect(plain.text, 'a');
    expect(plain.width, 1);
    expect(plain.bold, isFalse);
    expect(plain.dim, isFalse);
    expect(plain.italic, isFalse);
    expect(plain.inverse, isFalse);
    expect(plain.hidden, isFalse);
    expect(plain.strikeout, isFalse);
    expect(plain.underline, codux_protocol_ffi.TerminalScreenUnderline.none);
    expect(plain.underlineColor, isNull);
    // An absent colour means the default colour.
    expect(plain.fg['kind'], anyOf(isNull, 'default'));
    expect(plain.bg['kind'], anyOf(isNull, 'default'));

    // Present fields still survive the trip.
    final styled = cellAt(0, 2);
    expect(styled.text, 'C');
    expect(styled.bold, isTrue);
    expect(styled.fg['kind'], 'indexed');

    final wide = cellAt(1, 0);
    expect(wide.text, '字');
    expect(wide.width, 2);
  });
}
