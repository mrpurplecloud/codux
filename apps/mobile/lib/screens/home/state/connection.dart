part of '../home_page.dart';

/// Transport connection lifecycle: route refresh, grace/probe timers,
/// host-info + protocol-ready gating, host-responsive tracking, latency probe,
/// runtime reset, connect/reconnect, initial requests and envelope send.
///
/// Split into a part + extension to keep the State class navigable; behaviour
/// is unchanged. Rebuilds route through [_CoduxHomePageState._applyState]
/// (`setState` is `@protected` and cannot be called from an extension).
extension _HomePageConnection on HomeController {
  void _startNetworkRouteRefresh() {
    _networkRouteRefreshController.start();
  }

  void _refreshTransportRoute({required String reason}) {
    final device = _activeDevice;
    if (device == null || !_shouldReconnect) return;
    // A route change is exactly the event a gave-up schedule was waiting for:
    // the addresses that were unreachable a moment ago may be reachable now.
    // Resume before the throttle below, which exists to damp repeated refreshes
    // on a live connection and must not swallow the one that revives a dead one.
    if (_reconnectPolicy.exhausted) {
      _resumeReconnect('route-$reason');
      return;
    }
    final now = DateTime.now();
    final lastRefresh = _lastTransportRefreshAt;
    if (lastRefresh != null &&
        now.difference(lastRefresh) < const Duration(seconds: 8)) {
      CoduxLog.info('[codux-flutter-remote] refresh skipped reason=$reason');
      return;
    }
    _lastTransportRefreshAt = now;
    CoduxLog.info('[codux-flutter-remote] refresh route reason=$reason');
    final transport = _activeTransport;
    if (!_transportConnected || transport == null) {
      _connect(device, true);
      return;
    }
    _sendHostInfoRequest(force: true);
  }

  void _clearConnectionGrace() {
    _connectionGraceTimer?.cancel();
    _connectionGraceTimer = null;
    _connectionGraceUntil = null;
  }

  void _startConnectionGrace({
    required String reason,
    Duration duration = const Duration(seconds: 8),
  }) {
    if (!_shouldReconnect || !_appInForeground) return;
    _connectionGraceTimer?.cancel();
    _connectionGraceUntil = DateTime.now().add(duration);
    CoduxLog.info(
      '[codux-flutter-remote] grace reason=$reason until=${_connectionGraceUntil!.toIso8601String()} transport=$_lastTransportState lastConnectedAt=${_lastConnectedAt?.toIso8601String() ?? 'null'}',
    );
    _connectionGraceTimer = Timer(duration, () {
      if (!mounted || _disposing) return;
      if (_connectionGraceUntil == null) return;
      if (DateTime.now().isBefore(_connectionGraceUntil!)) return;
      _applyState(() {
        _connectionGraceUntil = null;
      });
      CoduxLog.info('[codux-flutter-remote] grace expired reason=$reason');
    });
  }

  void _markTransportConnected(String transport) {
    _lastTransportState = transport;
    _lastConnectedAt = DateTime.now();
    _clearConnectionGrace();
  }

  void _cancelHostResponseProbe() {
    _hostResponseTimer?.cancel();
    _hostResponseTimer = null;
  }

  void _startHostResponseProbe({
    required String reason,
    Duration duration = _remoteStartupProbeTimeout,
    bool restart = true,
  }) {
    final device = _activeDevice;
    final generation = _transportGeneration;
    final startedAtSerial = _hostResponseSerial;
    if (!_transportConnected || device == null) return;
    if (!restart && _hostResponseTimer != null) return;
    _cancelHostResponseProbe();
    CoduxLog.info(
      '[codux-flutter-remote] host probe start reason=$reason timeoutMs=${duration.inMilliseconds}',
    );
    _hostResponseTimer = Timer(duration, () {
      if (!mounted || _disposing || !_appInForeground) return;
      if (_transportGeneration != generation ||
          !_transportConnected ||
          _hostResponseSerial != startedAtSerial) {
        return;
      }
      _failHostConnection(device, 'host_response_timeout:$reason');
    });
  }

  void _markTransportOpen({String? path}) {
    // Deliberately does NOT reset the reconnect schedule. A transport that is
    // merely open is not a connection that works: a host which accepts QUIC and
    // then drops the link would reset the schedule on every cycle and could
    // never exhaust it, which is the same never-give-up loop this policy exists
    // to bound. The reset belongs to the first evidence the link is usable —
    // the protocol `hello`, or the host answering.
    _applyState(() {
      _transportConnected = true;
      _hasShownTerminal = true;
      if (path != null) _connectionPath = path;
      if (!_backgroundConnect && !_transportReady) {
        _status = _t('app.connecting');
      }
    });
  }

  void _markTransportPathDetail(
    String path, {
    String? endpoint,
    String? relayUrl,
  }) {
    final connected = path != 'none';
    final cleanedEndpoint = cleanRemoteTransportEndpoint(endpoint ?? '');
    _applyState(() {
      _transportConnected = connected;
      if (!connected) {
        _transportReady = false;
        _connectionEndpoint = '';
        _connectionRelayUrl = '';
      }
      if (connected) {
        _hasShownTerminal = true;
        if (!_backgroundConnect && !_transportReady) {
          _status = _t('app.connecting');
        }
        if (cleanedEndpoint.isNotEmpty) {
          _connectionEndpoint = cleanedEndpoint;
        } else if (path != _connectionPath) {
          _connectionEndpoint = '';
        }
        final cleanedRelayUrl = cleanRemoteTransportEndpoint(relayUrl ?? '');
        if (cleanedRelayUrl.isNotEmpty) {
          _connectionRelayUrl = cleanedRelayUrl;
        } else if (path != 'relay' && path != _connectionPath) {
          _connectionRelayUrl = '';
        }
      }
      _connectionPath = path;
    });
    // After the rebuild, not inside it: this can write to disk, and a state
    // callback is no place to start IO.
    //
    // Only a direct path yields an address worth dialling again. A relay path's
    // address is the relay's own — already named by the ticket, and silent about
    // where the host actually is.
    if (connected && path == 'direct' && cleanedEndpoint.isNotEmpty) {
      _rememberDirectAddr(cleanedEndpoint);
    }
  }

  /// Adopt the dial candidates the host just reported for itself, so the stored
  /// ones track where it actually is rather than where it was at pairing time.
  ///
  /// Silently does nothing when the host reported none — an older host, or one
  /// with nothing to say. Keeping what we have is strictly safer than
  /// discarding a working candidate set on the strength of silence.
  void _refreshStoredTransports(StoredDevice target, Object? value) {
    final reported = remoteTransportCandidatesFromJson(value);
    if (reported.isEmpty) return;
    // Ask the dial path — the one authority on what is dialable — whether this
    // report is usable before letting it replace a set that demonstrably is.
    // A host that reports something we could never dial has effectively told us
    // nothing, and adopting it would strand the device until it is re-paired.
    final current = _activeDevice?.deviceId == target.deviceId
        ? _activeDevice!
        : target;
    if (!_hasDialableTransport(current, reported)) {
      CoduxLog.warn(
        '[codux-flutter-remote] ignored undialable host transports device=${target.deviceId} count=${reported.length}',
      );
      return;
    }
    unawaited(_rememberReportedTransports(target.deviceId, reported));
  }

  Future<void> _rememberReportedTransports(
    String deviceId,
    List<RemoteTransportCandidate> transports,
  ) async {
    final snapshot = await _applyBackgroundDeviceMutation(
      DeviceTransportsReplaced(deviceId, transports),
      reason: 'host-transports',
    );
    if (snapshot == null) return;
    CoduxLog.info(
      '[codux-flutter-remote] host transports refreshed device=$deviceId count=${transports.length}',
    );
  }

  /// Whether any of [candidates] is one the dial path would actually accept.
  ///
  /// Answered by the native config parser rather than re-implemented here: it
  /// applies the same rule the dialer does, so this cannot drift from it. The
  /// summary reports the count that survived that filter.
  bool _hasDialableTransport(
    StoredDevice target,
    List<RemoteTransportCandidate> candidates,
  ) => hasDialableControllerTransport(target.copyWith(transports: candidates));

  /// Remember the address a live direct path is using, so a later dial can try
  /// it even if the host has moved away from what the pairing ticket recorded.
  ///
  /// Called on every path report, so the common case is "already the most
  /// recent" — the controller reports no update then and nothing is written.
  void _rememberDirectAddr(String addr) {
    final active = _activeDevice;
    if (active == null) return;
    unawaited(_rememberActiveDeviceDirectAddr(active.deviceId, addr));
  }

  Future<void> _rememberActiveDeviceDirectAddr(
    String deviceId,
    String addr,
  ) async {
    final snapshot = await _applyBackgroundDeviceMutation(
      DeviceDirectAddrRemembered(deviceId, addr),
      reason: 'direct-addr',
    );
    if (snapshot == null) return;
    CoduxLog.info(
      '[codux-flutter-remote] remembered direct addr=$addr device=$deviceId',
    );
  }

  bool _isCompatibleRemoteProtocol(Object? payload) {
    if (payload is! Map) return false;
    return payload['protocolVersion'] == _remoteProtocolVersion;
  }

  void _markRemoteProtocolReady({bool force = false}) {
    if (!_remoteSyncController.markProtocolReady(force: force)) return;
    CoduxLog.info('[codux-flutter-remote] protocol ready force=$force');
    _sendInitialTransportRequests(force: force);
    _ensureTerminalForSelectedProject();
    _bindActiveTerminalAfterProtocolReady(reason: 'protocol-ready');
    _drivePendingProjectSelect(reason: 'protocol-ready');
  }

  void _failRemoteProtocol(StoredDevice target, Object? payload) {
    final version = payload is Map ? '${payload['protocolVersion'] ?? ''}' : '';
    CoduxLog.warn(
      '[codux-flutter-remote] incompatible protocol expected=$_remoteProtocolVersion received=$version host=${target.hostId} device=${target.deviceId}',
    );
    _shouldReconnect = false;
    final shouldPrompt = _protocolBlockedHostIds.add(target.hostId);
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _cancelHostResponseProbe();
    _clearConnectionGrace();
    _clearLatencyProbe();
    _transportConnected = false;
    unawaited(_closeActiveTransport());
    _terminalInputBatcher.reset();
    _terminalInputSender.clear();
    _resetRemoteSubscriptions();
    _viewportOwnerRefreshAfterBaseline.clear();
    _terminalOutputAckSeqBySession.clear();
    _terminalOutputAckAtBySession.clear();
    final message = _t('connection.upgradeRequired');
    _applyState(() {
      _transportReady = false;
      _remoteSyncController.resetProtocolReady();
      _hostResponsive = false;
      _backgroundConnect = false;
      _showTerminal = false;
      _workspaceMode = WorkspaceMode.terminal;
      _resetRemoteSyncState();
      _showTerminalSwitcher = false;
      _status = message;
      _terminalBufferRetry.reset();
      _terminalOutputController.resetTransient();
      _setTerminalBufferLoading(false);
    });
    if (shouldPrompt) {
      _showToast(message);
    }
  }

  void _stopRemoteConnectionForAuthChange() {
    _shouldReconnect = false;
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _connectInFlight = false;
    _connectInFlightKey = null;
    _cancelHostResponseProbe();
    _cancelRemoteSyncTimers();
    _clearConnectionGrace();
    _clearLatencyProbe();
    _transportConnected = false;
    unawaited(_closeActiveTransport());
    _terminalInputBatcher.reset();
    _terminalInputSender.clear();
    _resetRemoteSubscriptions();
    _viewportOwnerRefreshAfterBaseline.clear();
    _terminalOutputAckSeqBySession.clear();
    _terminalOutputAckAtBySession.clear();
    _terminalBufferRetry.reset();
    if (_remoteRuntime.cancelTerminalCreate()) {
      _syncRuntimeViewState();
    }
  }

  void _requireRepairPairing(Object? payload) {
    final code = payload is Map ? '${payload['code'] ?? ''}' : '';
    CoduxLog.warn('[codux-flutter-remote] authorization failed code=$code');
    _stopRemoteConnectionForAuthChange();
    _applyState(() {
      _transportReady = false;
      _remoteSyncController.resetProtocolReady();
      _hostResponsive = false;
      _backgroundConnect = false;
      _leaveTerminalUi();
      _status = _t('pair.repairRequired');
      _terminalOutputController.resetTransient();
      _setTerminalBufferLoading(false);
    });
  }

  void _markHostResponsive(String source, {String? transport}) {
    final wasResponsive = _hostResponsive;
    // The strongest evidence the link works: the host answered. Whatever the
    // previous attempts were struggling with is over.
    _reconnectPolicy.reset();
    if (mounted && !_disposing) {
      _applyState(() {
        _transportConnected = true;
        _transportReady = true;
        _hostResponsive = true;
        _connectInFlight = false;
        _connectInFlightKey = null;
        if (!_backgroundConnect) _status = _t('app.connected');
      });
    } else {
      _transportConnected = true;
      _transportReady = true;
      _hostResponsive = true;
      _connectInFlight = false;
      _connectInFlightKey = null;
    }
    _hostResponseSerial += 1;
    _cancelHostResponseProbe();
    _markTransportConnected(transport ?? _deviceTransportKind(_activeDevice));
    if (!wasResponsive && _settings.keepConnectionAlive) {
      unawaited(_backgroundKeepAlive.requestBackgroundExecutionSettings());
    }
    if (!wasResponsive) {
      CoduxLog.info('[codux-flutter-remote] host responsive source=$source');
    }
  }

  String _deviceTransportKind(StoredDevice? device) {
    if (device == null) return RemoteTransportKind.iroh;
    final kind = remotePreferredTransportKind(
      device.transports,
      pairing: false,
    );
    return kind.isEmpty ? RemoteTransportKind.iroh : kind;
  }

  bool _hasConnectableTransport(StoredDevice device) =>
      hasDialableControllerTransport(device);

  void _clearLatencyProbe() {
    _latencyProbeTimer?.cancel();
    _latencyProbeTimer = null;
    _latencyProbeSentAt.clear();
    _latencyProbeCounter = 0;
    if (_latencyMs == null) return;
    _latencyMs = null;
  }

  void _pauseLatencyProbe() {
    // App lifecycle pauses the ping loop but does not mean the route is gone.
    // Keep the last measured RTT visible until the transport explicitly closes
    // or a new measurement replaces it.
    _latencyProbeTimer?.cancel();
    _latencyProbeTimer = null;
  }

  void _startLatencyProbe() {
    if (_latencyProbeTimer != null || !_transportConnected) return;
    _sendLatencyProbe();
    _latencyProbeTimer = Timer.periodic(
      _remoteLatencyProbeInterval,
      (_) => _sendLatencyProbe(),
    );
  }

  void _sendLatencyProbe() {
    final transport = _activeTransport;
    final device = _activeDevice;
    if (transport == null || device == null || !_transportConnected) return;
    final now = DateTime.now();
    _latencyProbeSentAt.removeWhere(
      (_, sentAt) => now.difference(sentAt) > _remoteLatencyProbeTimeout,
    );
    final id = '${now.microsecondsSinceEpoch}-${++_latencyProbeCounter}';
    _latencyProbeSentAt[id] = now;
    unawaited(
      transport.send(
        RelayEnvelope(
          type: RemoteMessageType.transportPing,
          deviceId: device.deviceId,
          payload: {'id': id},
        ).toJson(),
      ),
    );
  }

  void _handleTransportPong(RelayEnvelope message) {
    final payload = message.payload;
    final id = payload is Map ? '${payload['id'] ?? ''}' : '';
    if (id.isEmpty) return;
    final sentAt = _latencyProbeSentAt.remove(id);
    if (sentAt == null) return;
    final rtt = DateTime.now().difference(sentAt).inMilliseconds;
    CoduxLog.debug(
      '[codux-flutter-remote] app latency rtt=${rtt}ms path=$_connectionPath',
    );
    if (!mounted || _disposing || _latencyMs == rtt) return;
    _applyState(() => _latencyMs = rtt);
  }

  void _sendHostInfoRequest({bool force = false}) {
    if (!_remoteSyncController.shouldSendHostInfo(
      transportReady: _transportReady,
      transportConnected: _transportConnected,
      force: force,
    )) {
      return;
    }
    CoduxLog.info('[codux-flutter-remote] request host.info');
    _send(
      RelayEnvelope(type: RemoteMessageType.hostInfo),
      onResult: (_, result) {
        if (result == RemoteEnvelopeSendResult.delivered) {
          _remoteSyncController.markHostInfoSent();
        }
      },
    );
  }

  void _failHostConnection(StoredDevice target, String reason) {
    CoduxLog.warn(
      '[codux-flutter-remote] host unavailable reason=$reason host=${target.hostId} device=${target.deviceId}',
    );
    _remoteRuntimeEpoch += 1;
    _disconnectTransport(
      status: _t('connection.failedRetry'),
      closeTerminal: true,
      notifyHost: false,
    );
    if (_appSuspended || !_appInForeground) {
      CoduxLog.info(
        '[codux-flutter-remote] reconnect deferred reason=$reason appSuspended=$_appSuspended',
      );
      return;
    }
    _scheduleReconnect(target);
  }

  void _resetRemoteSyncState() {
    _remoteRuntimeEpoch += 1;
    _terminalRenameSupported = false;
    _cancelRemoteSyncTimers();
    _remoteSyncController.resetSyncForCurrentGeneration();
    _remoteRuntime.reset();
    _resetRemoteSubscriptions();
    _terminalBaselineResyncRequestedAt.clear();
    _viewportOwnerRefreshAfterBaseline.clear();
    _terminalOutputAckSeqBySession.clear();
    _terminalOutputAckAtBySession.clear();
    _terminalViewportController.resetSizes();
    _terminalViewportInteractive = false;
    _syncRuntimeViewState();
  }

  void _resetRemoteSubscriptions() {
    _terminalBindingCoordinator.reset();
    _resourceSubscriptionCoordinator.reset();
  }

  void _resetRemoteRuntime({bool keepProjects = false}) {
    _remoteRuntimeEpoch += 1;
    _remoteRuntime.reset(keepProjects: keepProjects);
    _resetRemoteSubscriptions();
    _terminalBaselineResyncRequestedAt.clear();
    _viewportOwnerRefreshAfterBaseline.clear();
    _terminalOutputAckSeqBySession.clear();
    _terminalOutputAckAtBySession.clear();
    _terminalViewportController.resetSizes();
    // Drop version watermarks: a new connection / host restart restarts the
    // host's per-key version sequence, so an old high watermark would wrongly
    // reject the fresh snapshots.
    _remoteStateVersions.reset();
    _terminalViewportInteractive = false;
    _syncRuntimeViewState();
  }

  void _resetRemoteRuntimeAfterHostRestart(String reason) {
    CoduxLog.info('[codux-flutter-remote] reset runtime reason=$reason');
    _remoteRuntimeEpoch += 1;
    _cancelRemoteSyncTimers();
    _remoteSyncController.resetSyncForCurrentGeneration();
    _remoteSyncController.resetProtocolReady();
    _resetRemoteSubscriptions();
    _terminalBaselineResyncRequestedAt.clear();
    _viewportOwnerRefreshAfterBaseline.clear();
    _terminalOutputAckSeqBySession.clear();
    _terminalOutputAckAtBySession.clear();
    _remoteStateVersions.reset();
    _terminalInputBatcher.reset();
    _terminalInputSender.clear();
    _terminalBufferRetry.reset();
    _terminalOutputController.resetAll();
    _terminalViewportController.resetSizes();
    _terminalRepaint.tick();
    _terminalViewportInteractive = false;
    _receiveSequenceGuard.reset();
    _receiveChain = Future<void>.value();
    _hostResponsive = false;
    _remoteRuntime.reset(keepProjects: true);
    _syncRuntimeViewState();
    _setTerminalBufferLoading(false);
    _clearTerminal();
  }

  bool _recordHostRuntimeInstance(Object? payload, {required String hostId}) {
    if (payload is! Map) return false;
    final next = payload['runtimeInstanceId']?.toString().trim();
    if (next == null || next.isEmpty) return false;
    final previous = _hostRuntimeInstanceId;
    _hostRuntimeInstanceId = next;
    if (previous == null || previous == next) return false;
    _resetRemoteRuntimeAfterHostRestart(
      'host-runtime-instance-changed:$previous->$next',
    );
    _terminalUploadReplyGuard.resetAfterHostRestart(hostId);
    return true;
  }

  void _cancelRemoteSyncTimers() {
    _projectListRetryTimer?.cancel();
    _projectListRetryTimer = null;
    _terminalListRetryTimer?.cancel();
    _terminalListRetryTimer = null;
    for (final timer in _projectSelectAckTimers.values) {
      timer.cancel();
    }
    _projectSelectAckTimers.clear();
  }

  void _leaveTerminalUi() {
    _showTerminal = false;
    _workspaceMode = WorkspaceMode.terminal;
    _showTerminalSwitcher = false;
    _keyboardRequested = false;
    _keyboardRequestSerial += 1;
    _keyboardShownSinceRequest = false;
    _keyboardVisible = false;
  }

  void _disconnectTransport({
    required String status,
    bool closeTerminal = false,
    bool notifyHost = true,
    bool resetRuntime = false,
  }) {
    if (notifyHost && _transportConnected) {
      _notifyHostBeforeTransportClose();
    }
    _cancelHostResponseProbe();
    _clearConnectionGrace();
    _lastConnectedAt = null;
    _clearLatencyProbe();
    _transportConnected = false;
    unawaited(_closeActiveTransport());
    _terminalInputBatcher.reset();
    _terminalInputSender.clear();
    final terminalCreateCancelled = _remoteRuntime.cancelTerminalCreate();
    if (resetRuntime) {
      _terminalOutputController.resetAll();
      _terminalRepaint.tick();
      _resetRemoteSubscriptions();
      _terminalBaselineResyncRequestedAt.clear();
      _viewportOwnerRefreshAfterBaseline.clear();
      _terminalOutputAckSeqBySession.clear();
      _terminalOutputAckAtBySession.clear();
      _terminalViewportController.resetSizes();
    }
    _applyState(() {
      if (terminalCreateCancelled) _syncRuntimeViewState();
      _transportReady = false;
      if (resetRuntime) {
        _remoteSyncController.resetProtocolReady();
      }
      _hostResponsive = false;
      _backgroundConnect = false;
      if (closeTerminal) {
        _leaveTerminalUi();
      }
      _status = status;
      _terminalBufferRetry.reset();
      _setTerminalBufferLoading(false);
    });
    if (resetRuntime || closeTerminal) {
      _clearTerminal();
    }
  }

  void _recoverForegroundState() {
    if (!_transportReady) {
      final device = _activeDevice;
      if (device != null) _connect(device, true);
      return;
    }
    _backgroundConnect = false;
    _requestProjectList(resetRetry: true);
    _requestTerminalList(resetRetry: true);
    _sendHostInfoRequest();
    _mountVisibleTerminal(reason: 'foreground');
    _flushTerminalInput();
  }

  Future<void> _connect([StoredDevice? device, bool background = false]) async {
    final target = device ?? _activeDevice;
    if (target == null) {
      _applyState(() => _showScanner = true);
      return;
    }
    final connectKey = '${target.hostId}:${target.deviceId}';
    if (background &&
        _activeDevice?.hostId == target.hostId &&
        _activeDevice?.deviceId == target.deviceId &&
        _transportConnected &&
        _remoteProtocolReady) {
      CoduxLog.info(
        '[codux-flutter-remote] connect skipped reason=already-ready host=${target.hostId} device=${target.deviceId}',
      );
      return;
    }
    if (_connectInFlight &&
        _connectInFlightKey == connectKey &&
        !_remoteProtocolReady) {
      CoduxLog.info(
        '[codux-flutter-remote] connect skipped reason=in-flight host=${target.hostId} device=${target.deviceId}',
      );
      return;
    }
    if (_protocolBlockedHostIds.contains(target.hostId)) {
      if (!background) {
        _applyState(() => _status = _t('connection.upgradeRequired'));
      }
      return;
    }
    _shouldReconnect = true;
    // A connect the user asked for outranks a schedule that gave up: they can
    // see something we cannot, and being told "no" by a policy they did not set
    // is not acceptable. Background retries deliberately do not reset — that
    // would make the schedule unable to terminate.
    if (!background) _reconnectPolicy.reset();
    _backgroundConnect = background;
    _connectInFlight = true;
    _connectInFlightKey = connectKey;
    final previousDevice = _activeDevice;
    final switchingDevice =
        previousDevice == null ||
        previousDevice.hostId != target.hostId ||
        previousDevice.deviceId != target.deviceId;
    _cancelRemoteSyncTimers();
    final generation = _remoteSyncController.beginConnectionGeneration();
    _terminalUploadCapability = TerminalUploadCapability.fallback;
    if (switchingDevice) {
      _hostRuntimeInstanceId = null;
      _resetRemoteRuntime(keepProjects: false);
      _terminalOutputController.resetAll();
      _terminalRepaint.tick();
    } else {
      _resetRemoteSubscriptions();
    }
    CoduxLog.info(
      '[codux-flutter-remote] connect start gen=$generation background=$background host=${target.hostId} device=${target.deviceId} transport=${_deviceTransportKind(target)} relay=${_savedDeviceRelayEndpoint(target)}',
    );
    _cancelHostResponseProbe();
    _reconnectTimer?.cancel();
    _clearLatencyProbe();
    unawaited(_closeActiveTransport());
    _transportConnected = false;
    _sendQueue.reset(seed: DateTime.now().microsecondsSinceEpoch);
    _receiveSequenceGuard.reset();
    _receiveChain = Future<void>.value();
    if (background && _lastConnectedAt != null) {
      _startConnectionGrace(reason: 'background_connect');
    }
    if (!background) _clearTerminal();
    if (!background) _terminalInputBatcher.reset();
    _applyState(() {
      _transportReady = false;
      _remoteSyncController.resetProtocolReady();
      _hostResponsive = false;
      _connectionPath = 'unknown';
      _connectionEndpoint = '';
      _connectionRelayUrl = '';
      _latencyMs = null;
      if (!background) {
        _status = _t('app.connecting');
        _showTerminalSwitcher = false;
        _terminalBufferRetry.reset();
        _terminalOutputController.resetTransient();
        _setTerminalBufferLoading(false);
      }
      _activeDevice = target;
    });
    await _restoreCachedProjects(target);
    if (!mounted ||
        _disposing ||
        generation != _transportGeneration ||
        !_shouldReconnect) {
      return;
    }
    if (!_hasConnectableTransport(target)) {
      _applyState(() => _status = _t('pair.repairRequired'));
      return;
    }
    final transport = (widget.transportFactory ?? createRemoteTransport)(
      target,
    );
    transport
      ..onState = (rawState) {
        if (generation != _transportGeneration ||
            !identical(_activeTransport, transport)) {
          CoduxLog.debug(
            '[codux-flutter-remote] drop stale transport state gen=$generation current=$_transportGeneration state=$rawState',
          );
          return;
        }
        _handleTransportState(rawState);
      }
      ..onEnvelope = (envelope, raw) {
        if (generation != _transportGeneration ||
            !identical(_activeTransport, transport)) {
          CoduxLog.debug(
            '[codux-flutter-remote] drop stale transport envelope gen=$generation current=$_transportGeneration type=${envelope['type'] ?? ''}',
          );
          return;
        }
        _handleTransportEnvelopeQueued(
          RelayEnvelope.fromJson(envelope, rawJson: raw),
          generation: generation,
          transport: transport,
        );
      };
    _activeTransport = transport;
    transport.connect(target).catchError((Object error) {
      CoduxLog.warn(
        '[codux-flutter-remote] connect failed gen=$generation error=$error',
      );
      if (generation != _transportGeneration) return;
      _connectInFlight = false;
      _connectInFlightKey = null;
      if (!_backgroundConnect && mounted) {
        _applyState(() => _status = _t('connection.failedRetry'));
      }
      _handleTransportClosed('connect_failed');
    });
  }

  void _scheduleReconnect(StoredDevice target) {
    if (!_shouldReconnect) return;
    final delay = _reconnectPolicy.recordFailure(
      generation: _transportGeneration,
    );
    if (delay == null) {
      if (_reconnectPolicy.exhausted) _enterReconnectExhausted(target);
      // Otherwise this was the second report of a failure already scheduled, or
      // a superseded connect reporting in late. Either way the live schedule is
      // already correct and must not be disturbed.
      return;
    }
    _reconnectTimer?.cancel();
    CoduxLog.info(
      '[codux-flutter-remote] reconnect scheduled host=${target.hostId} device=${target.deviceId} attempt=${_reconnectPolicy.attempt} delayMs=${delay.inMilliseconds}',
    );
    _reconnectTimer = Timer(delay, () => _connect(target, true));
  }

  /// Stop automatic retries and say so, rather than re-dialling an unreachable
  /// host forever at the ceiling interval. Recovery is not dead-ended: a network
  /// route change, a return to the foreground, or the user picking the device
  /// again all call [_resumeReconnect].
  void _enterReconnectExhausted(StoredDevice target) {
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    CoduxLog.warn(
      '[codux-flutter-remote] reconnect exhausted host=${target.hostId} device=${target.deviceId} attempts=${_reconnectPolicy.attempt}',
    );
    if (!mounted || _disposing) return;
    _applyState(() {
      // Not `failedRetry`: that string promises a background retry, and there
      // is no longer one pending. Saying so would leave the user waiting for
      // something that will never happen.
      _status = _t('connection.failedExhausted');
    });
  }

  /// Clear an exhausted schedule and try again now, because something changed
  /// that the previous attempts could not have accounted for.
  void _resumeReconnect(String reason) {
    if (!_reconnectPolicy.exhausted) return;
    final target = _activeDevice;
    if (target == null || !_shouldReconnect) return;
    CoduxLog.info('[codux-flutter-remote] reconnect resumed reason=$reason');
    _reconnectPolicy.reset();
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _connect(target, true);
  }

  void _sendInitialTransportRequests({bool force = false}) {
    final plan = _remoteSyncController.initialSyncPlan(
      transportReady: _transportReady,
      transportConnected: _transportConnected,
      force: force,
    );
    if (!plan.hasWork) {
      return;
    }
    if (plan.resetTerminalBufferRetry) {
      _terminalBufferRetry.reset();
    }
    CoduxLog.info('[codux-flutter-remote] request initial sync force=$force');
    if (plan.sendDeviceInfo) {
      _sendDeviceInfo(force: force);
    }
    if (plan.requestProjectList) {
      _requestProjectList(resetRetry: force);
    }
    if (plan.requestTerminalList) {
      _requestTerminalList(resetRetry: force);
    }
  }

  void _sendDeviceInfo({bool force = false}) {
    if (!_remoteSyncController.shouldSendDeviceInfo(force: force)) return;
    final target = _activeDevice;
    _send(
      RelayEnvelope(
        type: 'device.info',
        payload: {
          'name': _settings.localName.isNotEmpty
              ? _settings.localName
              : (target?.name ?? _detectedDeviceName),
        },
      ),
      onResult: (_, result) {
        if (result == RemoteEnvelopeSendResult.delivered) {
          _remoteSyncController.markDeviceInfoSent();
        }
      },
    );
  }

  bool _send(
    RelayEnvelope message, {
    bool sendTerminal = false,
    RemoteSendResultHandler? onResult,
  }) {
    if (!_transportConnected) {
      _applyState(() => _status = _t('app.remoteNotConnected'));
      CoduxLog.warn(
        '[codux-flutter-remote] drop type=${message.type} reason=not_ready',
      );
      return false;
    }
    final transport = _activeTransport;
    if (transport == null) {
      CoduxLog.warn(
        '[codux-flutter-remote] drop type=${message.type} reason=no_transport',
      );
      return false;
    }
    CoduxLog.debug(
      '[codux-flutter-remote] send type=${message.type} session=${message.sessionId ?? ''}',
    );
    unawaited(
      _sendQueue.send(
        message: message,
        transport: transport,
        connected: () => _transportConnected,
        activeDevice: _activeDevice,
        terminalStream: sendTerminal,
        onResult: (sentMessage, result) {
          if (sentMessage.type == RemoteMessageType.hostInfo ||
              sentMessage.type == RemoteMessageType.projectSelect ||
              result != RemoteEnvelopeSendResult.delivered) {
            CoduxLog.info(
              '[codux-flutter-remote] send result type=${sentMessage.type} session=${sentMessage.sessionId ?? ''} result=${result.name} connected=$_transportConnected ready=$_transportReady path=$_connectionPath',
            );
          }
          if (result == RemoteEnvelopeSendResult.rejected) {
            _handleRejectedTransportSend(sentMessage);
          }
          onResult?.call(sentMessage, result);
        },
        onError: (error) {
          CoduxLog.error('[codux-flutter-remote] send failed: $error');
        },
      ),
    );
    return true;
  }

  void _handleRejectedTransportSend(RelayEnvelope message) {
    if (!mounted || _disposing || !_transportConnected) return;
    if (message.type == 'device.disconnected') return;
    final target = _activeDevice;
    if (target == null) {
      _handleTransportClosed('send_rejected:${message.type}');
      return;
    }
    _failHostConnection(target, 'send_rejected:${message.type}');
  }

  bool _sendTerminalEnvelope(RelayEnvelope message, {TerminalInfo? terminal}) {
    final scoped = _scopeTerminalEnvelope(message, terminal: terminal);
    if (scoped == null) return false;
    if (_isTerminalStreamEnvelope(scoped)) {
      return _send(scoped, sendTerminal: true);
    }
    return _send(scoped);
  }

  bool _isTerminalStreamEnvelope(RelayEnvelope message) {
    // The host classifies the terminal-stream lane through `codux-protocol`;
    // route the controller through the same FFI predicate so neither end keeps
    // its own copy of the message-class list (and they can never disagree on
    // which frames take the low-latency stream path).
    return remoteIsTerminalStreamMessage(message.type);
  }

  /// `setState` is `@protected`, so the protocol handlers in the
  /// `home_page_protocol.part.dart` extension route their rebuilds through this
  /// in-class shim instead of calling `setState` directly.
}
