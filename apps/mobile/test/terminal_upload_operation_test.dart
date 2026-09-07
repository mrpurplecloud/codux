import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/terminal_upload_operation.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('relay envelope keeps top-level requestId through model operations', () {
    final envelope = RelayEnvelope.fromJson({
      'type': 'terminal.uploaded',
      'requestId': 'upload-1',
      'sessionId': 'session-1',
      'deviceId': 'device-1',
    });

    expect(envelope.requestId, 'upload-1');
    expect(envelope.toJson()['requestId'], 'upload-1');
    expect(envelope.copyWith(type: 'copy').requestId, 'upload-1');
  });

  test('operation identity prevents A-B-A ownership revival', () {
    final transport = Object();
    final old = _operation('upload-old', transport);
    final replacement = _operation('upload-new', transport);

    expect(_owns(old, old, transport), isTrue);
    expect(_owns(old, replacement, transport), isFalse);
  });

  test('operation loses ownership with any captured context change', () {
    final transport = Object();
    final operation = _operation('upload-1', transport);

    expect(
      operation.ownsContext(
        activeOperation: operation,
        activeSessionId: 'session-2',
        activeDeviceId: 'device-1',
        activeTransport: transport,
        mounted: true,
        canUpload: true,
      ),
      isFalse,
    );
    expect(
      operation.ownsContext(
        activeOperation: operation,
        activeSessionId: 'session-1',
        activeDeviceId: 'device-2',
        activeTransport: transport,
        mounted: true,
        canUpload: true,
      ),
      isFalse,
    );
    expect(_owns(operation, operation, Object()), isFalse);
    expect(
      operation.ownsContext(
        activeOperation: operation,
        activeSessionId: 'session-1',
        activeDeviceId: 'device-1',
        activeTransport: transport,
        mounted: true,
        canUpload: false,
      ),
      isFalse,
    );
  });

  test('reply must match request, session, device, owner, and liveness', () {
    final transport = Object();
    final operation = _operation('upload-1', transport);
    final matchingReply = RelayEnvelope(
      type: 'terminal.uploaded',
      requestId: 'upload-1',
      sessionId: 'session-1',
      deviceId: 'device-1',
    );
    expect(_accepts(operation, matchingReply, transport), isFalse);
    operation.markSent();
    RelayEnvelope reply({
      String? requestId = 'upload-1',
      String sessionId = 'session-1',
      String deviceId = 'device-1',
    }) => RelayEnvelope(
      type: 'terminal.uploaded',
      requestId: requestId,
      sessionId: sessionId,
      deviceId: deviceId,
    );

    expect(_accepts(operation, reply(), transport), isTrue);
    expect(_accepts(operation, reply(requestId: null), transport), isFalse);
    expect(
      _accepts(operation, reply(requestId: 'upload-old'), transport),
      isFalse,
    );
    expect(
      _accepts(operation, reply(sessionId: 'session-2'), transport),
      isFalse,
    );
    expect(
      _accepts(operation, reply(deviceId: 'device-2'), transport),
      isFalse,
    );

    operation.complete();
    expect(_accepts(operation, reply(), transport), isFalse);
  });

  test('legacy host may omit request id for its sole live operation', () {
    final transport = Object();
    final operation = _operation(
      'upload-1',
      transport,
      requestIdSupported: false,
    );
    operation.markSent();

    expect(
      _accepts(
        operation,
        const RelayEnvelope(
          type: 'terminal.uploaded',
          sessionId: 'session-1',
          deviceId: 'device-1',
        ),
        transport,
      ),
      isTrue,
    );
  });

  test('upload failures are isolated from the generic error route', () {
    final transport = Object();
    final legacy = _operation(
      'upload-legacy',
      transport,
      requestIdSupported: false,
    )..markSent();
    final modern = _operation('upload-modern', transport)..markSent();

    expect(
      _classifyUploadError(
        legacy,
        const RelayEnvelope(
          type: 'error',
          sessionId: 'session-1',
          deviceId: 'device-1',
          payload: {'code': 'terminal_upload_failed'},
        ),
        transport,
      ),
      TerminalUploadErrorDisposition.matched,
    );
    expect(
      _classifyUploadError(
        modern,
        const RelayEnvelope(
          type: 'error',
          requestId: 'upload-modern',
          sessionId: 'session-1',
          deviceId: 'device-1',
          payload: {'code': 'terminal_upload_failed'},
        ),
        transport,
      ),
      TerminalUploadErrorDisposition.matched,
    );
    expect(
      _classifyUploadError(
        modern,
        const RelayEnvelope(
          type: 'error',
          requestId: 'upload-old',
          sessionId: 'session-1',
          deviceId: 'device-1',
          payload: {'code': 'terminal_upload_failed'},
        ),
        transport,
      ),
      TerminalUploadErrorDisposition.stale,
    );
    expect(
      _classifyUploadError(
        modern,
        const RelayEnvelope(
          type: 'error',
          requestId: 'upload-modern',
          sessionId: 'session-1',
          deviceId: 'device-1',
          payload: {'code': 'other'},
        ),
        transport,
      ),
      TerminalUploadErrorDisposition.notUploadError,
    );
  });

  test('timed out legacy operation poisons only its uncorrelated scope', () {
    final guard = TerminalUploadReplyGuard();
    final first = _operation(
      'upload-old',
      Object(),
      requestIdSupported: false,
      runtimeInstanceId: 'runtime-1',
    );
    first.markSent();
    first.complete();
    guard.retire(first);

    expect(guard.canStart(first.scope, requestIdSupported: false), isFalse);
    expect(
      guard.canStart(
        first.scope.copyWith(sessionId: 'session-2'),
        requestIdSupported: false,
      ),
      isTrue,
    );
    expect(guard.canStart(first.scope, requestIdSupported: true), isTrue);
    expect(
      guard.canStart(
        first.scope.copyWith(runtimeInstanceId: 'runtime-2'),
        requestIdSupported: false,
      ),
      isTrue,
    );

    guard.resetAfterHostRestart('host-1');
    expect(guard.canStart(first.scope, requestIdSupported: false), isTrue);
  });

  test('host restart clears only that host legacy ambiguity', () {
    final guard = TerminalUploadReplyGuard();
    final first = _operation('upload-1', Object(), requestIdSupported: false)
      ..markSent();
    final otherHost = TerminalUploadOperation<Object>(
      requestId: 'upload-2',
      hostId: 'host-2',
      sessionId: 'session-1',
      deviceId: 'device-1',
      transport: Object(),
      requestIdSupported: false,
      runtimeInstanceId: 'runtime-1',
    )..markSent();
    first.complete();
    otherHost.complete();
    guard.retire(first);
    guard.retire(otherHost);

    guard.resetAfterHostRestart('host-1');

    expect(guard.canStart(first.scope, requestIdSupported: false), isTrue);
    expect(guard.canStart(otherHost.scope, requestIdSupported: false), isFalse);
  });

  test('late legacy terminal reply releases its retired scope', () {
    for (final reply in const [
      RelayEnvelope(
        type: 'terminal.uploaded',
        sessionId: 'session-1',
        deviceId: 'device-1',
      ),
      RelayEnvelope(
        type: 'error',
        sessionId: 'session-1',
        deviceId: 'device-1',
        payload: {'code': 'terminal_upload_failed'},
      ),
    ]) {
      final guard = TerminalUploadReplyGuard();
      final operation = _operation(
        'upload-old',
        Object(),
        requestIdSupported: false,
      )..markSent();
      operation.complete();
      guard.retire(operation);

      expect(
        guard.consumeAmbiguousLegacyReply(
          reply,
          hostId: 'host-1',
          runtimeInstanceId: 'runtime-1',
        ),
        isTrue,
      );
      expect(
        guard.canStart(operation.scope, requestIdSupported: false),
        isTrue,
      );
    }
  });

  test('unrelated reply cannot release retired legacy scope', () {
    final guard = TerminalUploadReplyGuard();
    final operation = _operation(
      'upload-old',
      Object(),
      requestIdSupported: false,
    )..markSent();
    operation.complete();
    guard.retire(operation);

    bool consume({
      String type = 'terminal.uploaded',
      String? requestId,
      String hostId = 'host-1',
      String deviceId = 'device-1',
      String sessionId = 'session-1',
      String? runtimeInstanceId = 'runtime-1',
      String? errorCode,
    }) => guard.consumeAmbiguousLegacyReply(
      RelayEnvelope(
        type: type,
        requestId: requestId,
        sessionId: sessionId,
        deviceId: deviceId,
        payload: errorCode == null ? null : {'code': errorCode},
      ),
      hostId: hostId,
      runtimeInstanceId: runtimeInstanceId,
    );

    expect(consume(type: 'error', errorCode: 'other'), isFalse);
    expect(consume(requestId: 'upload-other'), isFalse);
    expect(consume(hostId: 'host-2'), isFalse);
    expect(consume(deviceId: 'device-2'), isFalse);
    expect(consume(sessionId: 'session-2'), isFalse);
    expect(consume(runtimeInstanceId: 'runtime-2'), isFalse);
    expect(
      guard.consumeAmbiguousLegacyReply(
        const RelayEnvelope(
          type: 'terminal.uploaded',
          sessionId: 'session-1',
          deviceId: 'device-1',
        ),
        hostId: 'host-1',
        runtimeInstanceId: 'runtime-1',
      ),
      isTrue,
    );
    expect(guard.canStart(operation.scope, requestIdSupported: false), isTrue);
  });

  test('observed legacy reply leaves its scope reusable', () {
    final guard = TerminalUploadReplyGuard();
    final operation = _operation(
      'upload-1',
      Object(),
      requestIdSupported: false,
    );
    operation.markSent();
    operation.completeFromReply();
    guard.retire(operation);

    expect(guard.canStart(operation.scope, requestIdSupported: false), isTrue);
  });
}

TerminalUploadOperation<Object> _operation(
  String requestId,
  Object transport, {
  bool requestIdSupported = true,
  String? runtimeInstanceId = 'runtime-1',
}) => TerminalUploadOperation<Object>(
  requestId: requestId,
  hostId: 'host-1',
  sessionId: 'session-1',
  deviceId: 'device-1',
  transport: transport,
  requestIdSupported: requestIdSupported,
  runtimeInstanceId: runtimeInstanceId,
);

bool _owns(
  TerminalUploadOperation<Object> operation,
  TerminalUploadOperation<Object> active,
  Object transport,
) => operation.ownsContext(
  activeOperation: active,
  activeSessionId: 'session-1',
  activeDeviceId: 'device-1',
  activeTransport: transport,
  mounted: true,
  canUpload: true,
);

bool _accepts(
  TerminalUploadOperation<Object> operation,
  RelayEnvelope message,
  Object transport,
) => operation.acceptsReply(
  message,
  activeOperation: operation,
  activeSessionId: 'session-1',
  activeDeviceId: 'device-1',
  activeTransport: transport,
  mounted: true,
  canUpload: true,
);

TerminalUploadErrorDisposition _classifyUploadError(
  TerminalUploadOperation<Object> operation,
  RelayEnvelope message,
  Object transport,
) => classifyTerminalUploadError(
  message,
  activeOperation: operation,
  activeSessionId: 'session-1',
  activeDeviceId: 'device-1',
  activeTransport: transport,
  mounted: true,
  canUpload: true,
);
