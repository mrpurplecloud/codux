part of '../home_page.dart';

const int _terminalStaleOutputSeqTolerance = 8;

enum _TerminalViewportClaimIntent { auto, force }

/// Terminal session handling for [_CoduxHomePageState]: output/upload
/// effects, buffer-resync bookkeeping, viewport claim/release, resize, input
/// send, and terminal create/lookup. Split into a part + extension to keep the
/// State class navigable; behaviour is unchanged. Rebuilds route through
/// [_CoduxHomePageState._applyState] (`setState` is `@protected`).
extension _HomePageTerminal on HomeController {
  void _handleTerminalViewportState(RelayEnvelope message) {
    final sessionId = message.sessionId?.trim();
    if (sessionId == null || sessionId.isEmpty) return;
    final previousOwner =
        _terminalViewportController.ownerFor(sessionId)?.trim() ?? '';
    final applied = _terminalViewportController.applyRemoteState(message);
    final payload = message.payload is Map ? message.payload as Map : null;
    final rawOwner = payload?['owner']?.toString() ?? '';
    final rawOwnerLabel = payload?['ownerLabel']?.toString();
    final activeDeviceId = _activeDevice?.deviceId.trim() ?? '';
    final activeDeviceName = _activeDevice?.name.trim() ?? '';
    CoduxLog.info(
      '[DEBUG-takeover-label] viewport.state session=$sessionId '
      'rawOwner=$rawOwner rawOwnerLabel=${rawOwnerLabel ?? '<null>'} '
      'applied=$applied previousOwner=$previousOwner '
      'activeDeviceId=$activeDeviceId activeDeviceName=$activeDeviceName',
    );
    if (!applied) return;
    final owner = _terminalViewportController.ownerFor(sessionId) ?? '';
    final localOwner = _activeDevice == null
        ? ''
        : 'remote:${_activeDevice!.deviceId.trim()}';
    final becameLocalOwner =
        localOwner.isNotEmpty && owner == localOwner && previousOwner != owner;
    // Handoff: if any other owner (desktop or another remote device) took the
    // viewport, yield instead of reclaiming/resizing. The user explicitly taps
    // "Take over here" to move the PTY grid to this device.
    final handedAway =
        owner.isNotEmpty && (localOwner.isEmpty || owner != localOwner);
    CoduxLog.info(
      '[DEBUG-takeover-label] viewport.derived session=$sessionId '
      'owner=$owner localOwner=$localOwner handedAway=$handedAway '
      'interactive=${localOwner.isNotEmpty && owner == localOwner}',
    );
    final staleOutput =
        payload != null &&
        payload['staleOutput'] == true &&
        !handedAway &&
        _terminalViewportOutputLagRequiresResync(sessionId, payload);
    if (staleOutput) {
      _terminalBindingCoordinator.markSessionBaselineStale(sessionId);
    }
    final size = _terminalViewportController.reportedSize(sessionId);
    if (size != null && size.cols > 0 && size.rows > 0) {
      // Size the local cell screen to the host's ROW count but the phone's
      // COLUMN count -- BEFORE any handed-away return. Rows: the host keeps its
      // own (taller) row count for remote viewers, so a TUI anchors its input
      // box near the host's last row; feeding that into a screen clamped to the
      // phone's shorter viewport collapsed those rows onto each other
      // (overlapping text, a cursor stranded on the status line, the input box
      // clipped off-screen). A screen left at the phone's rows also clamps the
      // host's own frames and keyframes the same way while another device owns
      // the viewport, which is what strands a TUI's input row off-screen with no
      // local scroll able to reach it. Adopt the host rows and let the renderer
      // show the bottom window + scroll; the handoff placeholder covers the pane
      // until this device takes the viewport back. Cols: the phone DRIVES the
      // width (the host reflows to it), so keep our measured cols -- adopting
      // the host's transient desktop width (e.g. 111 while the desktop briefly
      // owns the viewport) would clip the grid horizontally. resizeScreen bumps
      // the render generation, so a static (non-generating) session repaints too.
      final cols = _terminalViewportController.pendingCols ?? size.cols;
      _terminalOutputController.resizeScreen(
        sessionId,
        cols: cols,
        rows: size.rows,
      );
      _terminalRepaint.tick();
    }
    if (sessionId == _sessionId) {
      final interactive = localOwner.isNotEmpty && owner == localOwner;
      final becameInteractive = interactive && !_terminalViewportInteractive;
      if (handedAway != _remoteHandedAway ||
          interactive != _terminalViewportInteractive) {
        _applyState(() {
          _remoteHandedAway = handedAway;
          _terminalViewportInteractive = interactive;
        });
      }
      // The local screen already mirrors the host's grid rows above, so the
      // placeholder only has to hide the pane until we take the viewport back.
      if (handedAway) return;
      if (becameInteractive) {
        _flushPendingTerminalResize(force: true);
      }
    }
    if (size == null || size.cols <= 0 || size.rows <= 0) return;
    if (becameLocalOwner && sessionId == _sessionId) {
      _requestTerminalViewportOwnerBaseline(sessionId);
    }
    if (staleOutput &&
        sessionId == _sessionId &&
        !_terminalOutputController.hasActiveBufferRequest(sessionId)) {
      if (!_terminalBaselineResyncAllowed(sessionId)) return;
      final requested = _terminalBindingCoordinator.subscribeSession(
        sessionId: sessionId,
        reason: 'stale-output-state',
        capability: _terminalBufferCapability,
        replaceActive: true,
      );
      if (requested) {
        _trackTerminalBaselineRequest(sessionId);
      }
    }
  }

  bool _terminalViewportOutputLagRequiresResync(String sessionId, Map payload) {
    if (_terminalOutputController.hasSequenceGap(sessionId)) return true;
    final hostSeq = _intFromPayload(payload['outputSeq']);
    if (hostSeq == null) return true;
    final localSeq = _terminalOutputController.outputSequence(sessionId);
    return hostSeq - localSeq > _terminalStaleOutputSeqTolerance;
  }

  void _requestTerminalViewportOwnerBaseline(String sessionId) {
    if (!_transportConnected || !_remoteProtocolReady) return;
    if (!_terminalViewportClaimable) return;
    if (_terminalOutputController.hasActiveBufferRequest(sessionId)) {
      _viewportOwnerRefreshAfterBaseline.add(sessionId);
      return;
    }
    if (!_terminalBaselineResyncAllowed(sessionId)) return;
    _viewportOwnerRefreshAfterBaseline.remove(sessionId);
    final requested = _terminalBindingCoordinator.subscribeSession(
      sessionId: sessionId,
      reason: 'viewport-owner-refresh',
      capability: _terminalBufferCapability,
      replaceActive: true,
    );
    if (requested) {
      _trackTerminalBaselineRequest(sessionId);
    }
  }

  ({int cols, int rows})? _terminalBaselineViewportSize(String sessionId) {
    final id = sessionId.trim();
    if (id.isEmpty) return null;
    final owner = _terminalViewportController.ownerFor(id)?.trim() ?? '';
    if (owner.isNotEmpty) {
      final localOwner = _activeDevice == null
          ? ''
          : 'remote:${_activeDevice!.deviceId.trim()}';
      if (localOwner.isEmpty || owner != localOwner) return null;
    }
    return _terminalViewportController.pendingSizeFor(id);
  }

  void _handleTerminalOutput(RelayEnvelope message) {
    final effects = _terminalOutputController.accept(
      message,
      activeSessionId: _sessionId,
    );
    _applyTerminalOutputEffects(effects);
  }

  void _applyTerminalOutputEffects(List<RemoteTerminalOutputEffect> effects) {
    for (final effect in effects) {
      switch (effect.kind) {
        case RemoteTerminalOutputEffectKind.loading:
          // A `receiving` loading tick means a baseline chunk just assembled --
          // proof the transfer is advancing. Tell the retry coordinator so a
          // slow but live high-latency transfer is not mistaken for a stall and
          // wiped. (The effect carries no session id; it is only emitted for the
          // active session, so the pending id is the active one.)
          if (effect.loading &&
              effect.phase == RemoteTerminalBufferPhase.receiving) {
            _terminalBufferRetry.noteProgress(_sessionId);
          }
          if (mounted) {
            _applyState(
              () => _setTerminalBufferLoading(
                effect.loading,
                progress: effect.progress,
                phase: effect.phase ?? RemoteTerminalBufferPhase.requesting,
              ),
            );
          } else {
            _setTerminalBufferLoading(
              effect.loading,
              progress: effect.progress,
              phase: effect.phase ?? RemoteTerminalBufferPhase.requesting,
            );
          }
        case RemoteTerminalOutputEffectKind.ack:
          final sessionId = effect.sessionId;
          if (sessionId != null) {
            _ackTerminalOutputIfNeeded(
              sessionId,
              effect.outputSeq,
              effect.bufferLength,
            );
          }
        case RemoteTerminalOutputEffectKind.markBufferReceived:
          _terminalBindingCoordinator.clearSessionBaselineStale(
            effect.sessionId,
          );
          final sessionId = effect.sessionId;
          if (sessionId != null) {
            _terminalBaselineResyncRequestedAt.remove(sessionId);
          }
          _markTerminalBufferReceived(sessionId);
          if (sessionId != null && effect.baselineScreenKeyframe == false) {
            _requestPendingViewportOwnerRefresh(sessionId);
          } else if (sessionId != null) {
            _viewportOwnerRefreshAfterBaseline.remove(sessionId);
          }
        case RemoteTerminalOutputEffectKind.sessionUpdated:
          // The self-drawn renderer reads the Rust cell snapshot directly; tick
          // the shared notifier so it repaints only that subtree (no full-page
          // setState / keyboard-inset / layout recompute per live frame).
          _terminalRepaint.tick();
        case RemoteTerminalOutputEffectKind.requestBaselineResync:
          final sessionId = effect.sessionId;
          if (sessionId != null) {
            _requestTerminalGapResync(sessionId);
          }
      }
    }
  }

  /// A live-output sequence gap was detected for [sessionId]: lost frames can
  /// only be repaired by re-requesting the baseline. Inactive sessions stay
  /// marked in the output controller and resync when they are next bound.
  void _requestTerminalGapResync(String sessionId) {
    if (!mounted || _disposing) return;
    if (sessionId != _sessionId) return;
    if (_terminalOutputController.hasActiveBufferRequest(sessionId)) return;
    if (!_terminalBaselineResyncAllowed(sessionId)) return;
    CoduxLog.warn(
      '[codux-flutter-terminal] sequence gap resync session=$sessionId',
    );
    final requested = _terminalBindingCoordinator.subscribeSession(
      sessionId: sessionId,
      reason: 'sequence-gap',
      capability: _terminalBufferCapability,
      replaceActive: true,
    );
    if (requested) {
      _trackTerminalBaselineRequest(sessionId);
    }
  }

  bool _terminalBaselineResyncAllowed(String sessionId) {
    final now = DateTime.now();
    final previous = _terminalBaselineResyncRequestedAt[sessionId];
    if (previous != null &&
        now.difference(previous) < _terminalBaselineResyncBackoff) {
      return false;
    }
    _terminalBaselineResyncRequestedAt[sessionId] = now;
    return true;
  }

  void _removeTerminalSessionState(String sessionId) {
    final id = sessionId.trim();
    if (id.isEmpty) return;
    if (_sessionId == id) {
      _cancelTerminalBaselineRearm();
    }
    _terminalOutputController.removeSession(id);
    _terminalBufferRetry.resetSession(id);
    _terminalInputSender.clearSession(id);
    _terminalBindingCoordinator.clearSessionBaselineStale(id);
    _terminalViewportController.removeSession(id);
    _terminalBaselineResyncRequestedAt.remove(id);
    _viewportOwnerRefreshAfterBaseline.remove(id);
    _terminalOutputAckSeqBySession.remove(id);
    _terminalOutputAckAtBySession.remove(id);
    _terminalRepaint.tick();
  }

  void _requestPendingViewportOwnerRefresh(String sessionId) {
    if (!_viewportOwnerRefreshAfterBaseline.remove(sessionId)) return;
    if (sessionId != _sessionId) return;
    final owner = _terminalViewportController.ownerFor(sessionId)?.trim() ?? '';
    if (_activeDevice == null) return;
    if (owner != 'remote:${_activeDevice!.deviceId.trim()}') return;
    _requestTerminalViewportOwnerBaseline(sessionId);
  }

  void _trackTerminalBaselineRequest(String sessionId) {
    _terminalBufferRetry.trackWhilePending(
      sessionId,
      send: _retryTerminalBaseline,
      hasPendingRequest: _terminalOutputController.hasActiveBufferRequest,
    );
  }

  void _handleTerminalUploaded(RelayEnvelope message) {
    if (_terminalUploadReplyGuard.consumeAmbiguousLegacyReply(
      message,
      hostId: _activeDevice?.hostId,
      runtimeInstanceId: _hostRuntimeInstanceId,
    )) {
      CoduxLog.info(
        '[codux-flutter-upload] resolve retired legacy reply session=${message.sessionId ?? ''} device=${message.deviceId ?? ''}',
      );
      return;
    }
    final operation = _terminalUploadOperation;
    if (operation == null ||
        !operation.acceptsReply(
          message,
          activeOperation: _terminalUploadOperation,
          activeSessionId: _sessionId,
          activeDeviceId: _activeDevice?.deviceId,
          activeTransport: _activeTransport,
          mounted: mounted,
          canUpload: _canUploadOverCurrentPath,
        )) {
      CoduxLog.info(
        '[codux-flutter-upload] drop unmatched reply request=${message.requestId ?? ''} session=${message.sessionId ?? ''} device=${message.deviceId ?? ''}',
      );
      return;
    }
    final payload = message.payload;
    if (payload is Map && payload['path'] != null) {
      final inserted = payload['inserted'] == true;
      final mode = payload['mode']?.toString();
      final tool = payload['tool']?.toString();
      final kind = payload['kind']?.toString();
      if (!inserted) {
        final path = '${payload['path']}';
        final activation = _terminalActivation;
        if (activation != null && activation.sessionId == operation.sessionId) {
          _insertTerminalText('$path ', activation: activation);
        }
      }
      _applyState(() {
        _terminalUploadLoading = false;
        _terminalUploadStatus = '';
        _status = kind == 'file'
            ? _t('upload.fileSentPath')
            : mode == 'clipboard'
            ? _t(
                'upload.imageSentTool',
                params: {'tool': tool ?? _t('upload.aiTool')},
              )
            : _t('upload.imageSentPath');
      });
      operation.completeFromReply();
    }
  }

  void _updateDevice(String deviceId, {String? hostName}) {
    if (hostName == null) return;
    unawaited(
      _applyBackgroundDeviceMutation(
        DeviceHostNameChanged(deviceId, hostName),
        reason: 'host-name',
      ),
    );
  }

  bool _retryTerminalBaseline(String sessionId) {
    if (!mounted || _sessionId != sessionId) return false;
    CoduxLog.info('[codux-flutter-terminal] baseline retry session=$sessionId');
    return _terminalBindingCoordinator.subscribeSession(
      sessionId: sessionId,
      reason: 'baseline-retry',
      capability: _terminalBufferCapability,
      replaceActive: true,
    );
  }

  /// Slow heartbeat after the fast retry burst gave up. Re-requests the baseline
  /// for the still-visible session; if it lands the retry coordinator stops, if
  /// it gives up again this re-arms, so a merely-slow (still connected) link
  /// heals on its own without a manual project switch / reconnect.
  void _scheduleTerminalBaselineRearm(String sessionId) {
    _terminalBaselineRearmTimer?.cancel();
    _terminalBaselineRearmTimer = Timer(const Duration(seconds: 10), () {
      _terminalBaselineRearmTimer = null;
      if (!mounted || _disposing) return;
      if (_sessionId != sessionId || !_transportConnected) return;
      final requested = _terminalBindingCoordinator.subscribeSession(
        sessionId: sessionId,
        reason: 'baseline-rearm',
        capability: _terminalBufferCapability,
        replaceActive: true,
      );
      if (requested) {
        _trackTerminalBaselineRequest(sessionId);
      } else {
        // Transport busy right now -- check back on the next heartbeat.
        _scheduleTerminalBaselineRearm(sessionId);
      }
    });
  }

  void _cancelTerminalBaselineRearm() {
    _terminalBaselineRearmTimer?.cancel();
    _terminalBaselineRearmTimer = null;
  }

  String _nextTerminalBufferRequestId(String sessionId) {
    _terminalBufferRequestCounter += 1;
    return '${DateTime.now().microsecondsSinceEpoch}-$_terminalBufferRequestCounter-$sessionId';
  }

  void _markTerminalBufferReceived(String? sessionId) {
    _terminalBufferRetry.markReceived(
      sessionId: sessionId,
      activeSessionId: _sessionId,
    );
    // A complete baseline landed: stop the slow heal heartbeat for it.
    if (sessionId == null || sessionId == _sessionId) {
      _cancelTerminalBaselineRearm();
    }
    if (_terminalBufferLoading && mounted) {
      _applyState(() => _setTerminalBufferLoading(false));
    }
    CoduxLog.info(
      '[codux-flutter-terminal] terminal.buffer received session=${sessionId ?? ''}',
    );
    if (sessionId != null) {
      _closeTerminalSwitcherAfterPendingWorktreeBuffer(sessionId);
    }
  }

  void _closeTerminalSwitcherAfterPendingWorktreeBuffer(String sessionId) {
    if (sessionId != _sessionId) return;
    _closeTerminalSwitcherIfPendingWorktreeReady();
  }

  void _closeTerminalSwitcherIfPendingWorktreeReady() {
    if (!_showTerminalSwitcher || !_pendingWorktreeSwitchHasActiveTerminal()) {
      return;
    }
    _pendingWorktreeSwitch = null;
    _closeTerminalSwitcher();
  }

  bool _pendingWorktreeSwitchHasActiveTerminal() {
    final pending = _pendingWorktreeSwitch;
    if (pending == null) return false;
    if (_selectedProjectId != pending.projectId ||
        _selectedWorktreeId != pending.worktreeId) {
      return false;
    }
    final active = _remoteRuntime.activeTerminal();
    if (active == null || active.projectId != pending.projectId) {
      return false;
    }
    return _terminalWorktreeId(active) == pending.worktreeId;
  }

  String _terminalWorktreeId(TerminalInfo terminal) {
    final worktreeId = terminal.worktreeId?.trim();
    if (worktreeId != null && worktreeId.isNotEmpty) return worktreeId;
    return terminal.projectId;
  }

  void _clearTerminal() {
    _terminalSelectedText = null;
    if (mounted) _applyState(() {});
  }

  void _sendOwnedTerminalResize(
    int cols,
    int rows, {
    required TerminalActivation activation,
  }) {
    // Handed off to another device: stay silent (claiming/resizing would steal
    // it back). The view already cached this measurement for a later takeover.
    if (_remoteHandedAway) return;
    final owner = activation;
    final id = owner.sessionId;
    final resize = _terminalViewportController.resize(
      sessionId: id,
      cols: cols,
      rows: rows,
      keyboardVisible: _keyboardVisible,
    );
    // The self-drawn terminal renders the host's grid, so the host PTY must
    // match the mobile viewport. Claim the viewport when the terminal is the
    // active view (rather than waiting for explicit input) so this resize
    // actually reaches the host; otherwise a repaint/TUI app keeps painting at
    // the host's old row count and leaves the bottom of the screen blank.
    if (!_terminalViewportInteractive && _terminalViewportClaimable) {
      _claimTerminalViewport(activation: owner);
    }
    if (!_terminalViewportClaimable || !_terminalViewportInteractive) return;
    final terminal = _terminalById(id);
    if (!_canResizeTerminal(terminal)) return;
    if (resize == null) {
      CoduxLog.debug(
        '[codux-flutter-terminal] resize skip duplicate measured=${cols}x$rows keyboard=$_keyboardVisible session=$id',
      );
      return;
    }
    CoduxLog.info(
      '[codux-flutter-terminal] send viewport.resize size=${resize.cols}x${resize.rows} measured=${cols}x$rows keyboard=$_keyboardVisible session=$id',
    );
    _sendTerminalEnvelope(
      RelayEnvelope(
        type: RemoteMessageType.terminalViewportResize,
        sessionId: id,
        payload: {'cols': resize.cols, 'rows': resize.rows},
      ),
      terminal: terminal,
    );
    _terminalViewportController.markSent(id, resize);
  }

  void _flushPendingTerminalResize({
    bool force = false,
    TerminalActivation? activation,
  }) {
    if (_remoteHandedAway) return;
    final owner = activation ?? _terminalActivation;
    if (!_ownsTerminalActivation(owner)) return;
    final id = owner!.sessionId;
    if (!_terminalViewportClaimable) return;
    if (!_terminalViewportInteractive) return;
    final terminal = _terminalById(id);
    if (!_canResizeTerminal(terminal)) return;
    final resize = _terminalViewportController.flushPending(
      sessionId: id,
      force: force,
    );
    if (resize == null) return;
    CoduxLog.info(
      '[codux-flutter-terminal] flush viewport.resize size=${resize.cols}x${resize.rows} force=$force session=$id',
    );
    _sendTerminalEnvelope(
      RelayEnvelope(
        type: RemoteMessageType.terminalViewportResize,
        sessionId: id,
        payload: {'cols': resize.cols, 'rows': resize.rows},
      ),
      terminal: terminal,
    );
    _terminalViewportController.markSent(id, resize);
  }

  void _claimTerminalViewport({
    TerminalActivation? activation,
    bool renewOnly = false,
    _TerminalViewportClaimIntent intent = _TerminalViewportClaimIntent.auto,
  }) {
    final owner = activation ?? _terminalActivation;
    if (!_ownsTerminalActivation(owner)) return;
    final id = owner!.sessionId;
    if (!_terminalViewportClaimable) return;
    final terminal = _terminalById(id);
    if (terminal == null || !_canResizeTerminal(terminal)) return;
    if (!renewOnly &&
        intent == _TerminalViewportClaimIntent.auto &&
        _lastViewportClaimSession == id &&
        _lastViewportClaimAt != null &&
        DateTime.now().difference(_lastViewportClaimAt!) <
            _viewportClaimThrottle) {
      return;
    }
    CoduxLog.info(
      '[DEBUG-takeover-label] viewport.claim session=$id '
      'intent=${intent.name} renewOnly=$renewOnly '
      'activeDeviceId=${_activeDevice?.deviceId.trim() ?? ''} '
      'activeDeviceName=${_activeDevice?.name.trim() ?? ''}',
    );
    final sent = _sendTerminalEnvelope(
      RelayEnvelope(
        type: RemoteMessageType.terminalViewportClaim,
        sessionId: id,
        payload: renewOnly
            ? const {'renewOnly': true}
            : {'intent': intent.name},
      ),
      terminal: terminal,
    );
    // Every claim waits for the host's authoritative owner state before resize.
    if (sent && !renewOnly) {
      _lastViewportClaimAt = DateTime.now();
      _lastViewportClaimSession = id;
    }
  }

  void _releaseTerminalViewport({String? sessionId}) {
    final id = sessionId ?? _sessionId;
    if (id == null || id.trim().isEmpty) return;
    final terminal = _terminalById(id);
    if (terminal == null || !_canResizeTerminal(terminal)) return;
    _terminalViewportInteractive = false;
    if (_lastViewportClaimSession == id) {
      _lastViewportClaimAt = null;
      _lastViewportClaimSession = null;
    }
    _sendTerminalEnvelope(
      RelayEnvelope(
        type: RemoteMessageType.terminalViewportRelease,
        sessionId: id,
      ),
      terminal: terminal,
    );
  }

  // Handoff: request ownership from whoever holds it now. The host's viewport
  // state response clears the placeholder and triggers the resize.
  void _takeOverTerminalViewport({required TerminalActivation activation}) {
    if (!_ownsTerminalActivation(activation)) return;
    _claimTerminalViewport(
      activation: activation,
      intent: _TerminalViewportClaimIntent.force,
    );
  }

  bool _ownsTerminalActivation(TerminalActivation? activation) =>
      activation != null && identical(_terminalActivation, activation);

  void _queueTerminalTyping(
    String data, {
    required TerminalActivation activation,
  }) {
    if (data.isEmpty || !_ownsTerminalActivation(activation)) return;
    _terminalInputBatcher.add(activation: activation, data: data);
  }

  void _sendTerminalKey(String data, {required TerminalActivation activation}) {
    if (data.isEmpty || !_ownsTerminalActivation(activation)) return;
    _terminalInputBatcher.flush(activation: activation);
    _sendInputNow(data, activation: activation, source: 'key');
  }

  /// Whether this device may forward scrolling to the app right now — the only
  /// authority on that, read at gesture time because ownership is released (and
  /// taken away by another device) between frames.
  bool _ownsTerminalViewport(TerminalActivation? activation) =>
      _terminalViewportInteractive && _ownsTerminalActivation(activation);

  /// Forward a scroll gesture to the app that owns scrolling. Unlike a key,
  /// scrolling is a read: it must not claim the viewport, because the handoff
  /// resizes the host PTY to this device's grid and the reflow costs the top of
  /// the scrollback.
  void _sendTerminalScroll(
    String data, {
    required TerminalActivation activation,
  }) {
    if (data.isEmpty || !_ownsTerminalViewport(activation)) return;
    _terminalInputBatcher.flush(activation: activation);
    _sendInputNow(
      data,
      activation: activation,
      source: 'scroll',
      claimViewport: false,
    );
  }

  void _insertTerminalText(
    String text, {
    required TerminalActivation activation,
  }) {
    if (text.isEmpty || !_ownsTerminalActivation(activation)) return;
    _terminalInputBatcher.flush(activation: activation);
    _sendInputNow(
      codux_terminal_core.terminalInsertInput(text),
      activation: activation,
      source: 'insert',
    );
  }

  void _flushTerminalInput() {
    final activation = _terminalActivation;
    if (activation != null) {
      _terminalInputBatcher.flush(activation: activation);
    }
  }

  void _sendInputNow(
    String data, {
    required TerminalActivation activation,
    required String source,
    bool claimViewport = true,
  }) {
    if (data.isEmpty) return;
    if (!_ownsTerminalActivation(activation)) {
      CoduxLog.info(
        '[codux-flutter-input] drop stale producer=${activation.sessionId} current=${_sessionId ?? ''}',
      );
      return;
    }
    if (claimViewport) {
      _claimTerminalViewport(activation: activation);
      _flushPendingTerminalResize(force: true, activation: activation);
    }
    _terminalInputSender.send(
      activation: activation,
      data: data,
      source: source,
      retry: data != '\u0003',
    );
  }

  void _sendTerminalOutputAck(
    String sessionId,
    int outputSeq,
    int? bufferLength,
  ) {
    final payload = <String, Object>{'outputSeq': outputSeq};
    if (bufferLength != null) {
      payload['bufferLength'] = bufferLength;
    }
    _sendTerminalEnvelope(
      RelayEnvelope(
        type: RemoteMessageType.terminalOutputAck,
        sessionId: sessionId,
        payload: payload,
      ),
    );
  }

  void _ackTerminalOutputIfNeeded(
    String sessionId,
    int? outputSeq,
    int? bufferLength,
  ) {
    if (outputSeq == null) return;
    final lastSeq = _terminalOutputAckSeqBySession[sessionId];
    if (lastSeq != null && outputSeq <= lastSeq) return;
    final lastAt = _terminalOutputAckAtBySession[sessionId];
    final now = DateTime.now();
    final seqLag = lastSeq == null
        ? _terminalOutputAckSeqInterval
        : outputSeq - lastSeq;
    final timeDue =
        lastAt == null ||
        now.difference(lastAt) >= _terminalOutputAckMinInterval;
    if (lastSeq != null && seqLag < _terminalOutputAckSeqInterval && !timeDue) {
      return;
    }
    _terminalOutputAckSeqBySession[sessionId] = outputSeq;
    _terminalOutputAckAtBySession[sessionId] = now;
    _sendTerminalOutputAck(sessionId, outputSeq, bufferLength);
  }

  void _createTerminal([String? projectId]) {
    final target =
        projectId ??
        _selectedProjectId ??
        (_projects.isNotEmpty ? _projects.first.id : null);
    if (target == null) {
      _applyState(() => _status = _t('project.noAvailable'));
      return;
    }
    if (_creatingTerminalProjectId == target) return;
    final scope = _remoteRuntime.terminalScopeForProject(target);
    final terminalId = const Uuid().v4();
    _remoteRuntime.beginTerminalCreate(
      terminalId: terminalId,
      projectId: target,
      worktreeId: scope?.worktreeId,
    );
    _applyState(_syncRuntimeViewState);
    // Spawn the host PTY at the phone's measured grid (when known) so the shell
    // draws its prompt once at the final width. Without this the host spawns at
    // its 100x32 default, prints the prompt, then the phone's first
    // viewport.resize triggers a SIGWINCH redraw -> a duplicate/ghost first
    // prompt line. The follow-up resize then matches the spawn size and the
    // host short-circuits it (no redraw).
    final spawnCols = _terminalViewportController.pendingCols;
    final spawnRows = _terminalViewportController.pendingRows;
    final sent = _send(
      RelayEnvelope(
        type: RemoteMessageType.terminalCreate,
        sessionId: terminalId,
        payload: {
          'terminalId': terminalId,
          'projectId': target,
          if (scope?.worktreeId != null && scope!.worktreeId!.trim().isNotEmpty)
            'worktreeId': scope.worktreeId!,
          if (scope?.projectPath != null &&
              scope!.projectPath!.trim().isNotEmpty)
            'projectPath': scope.projectPath!,
          'command': '',
          if (spawnCols != null &&
              spawnRows != null &&
              spawnCols > 0 &&
              spawnRows > 0) ...{
            'cols': spawnCols,
            'rows': spawnRows,
          },
        },
      ),
      onResult: (_, result) {
        if (result == RemoteEnvelopeSendResult.delivered) return;
        if (_remoteRuntime.cancelTerminalCreate(terminalId) &&
            mounted &&
            !_disposing) {
          _applyState(_syncRuntimeViewState);
        }
      },
    );
    if (!sent && _remoteRuntime.cancelTerminalCreate(terminalId)) {
      _applyState(_syncRuntimeViewState);
    }
  }

  bool _isAccessibleTerminal(TerminalInfo terminal) {
    return RemoteRuntimeStore.isAccessibleTerminal(terminal);
  }

  TerminalInfo? _currentTerminal() {
    return _remoteRuntime.activeTerminal();
  }

  TerminalInfo? _terminalById(String sessionId) {
    for (final terminal in _terminals) {
      if (terminal.id == sessionId) return terminal;
    }
    return null;
  }

  RemoteTerminalScope? _terminalScopeForSession(
    String sessionId, {
    TerminalInfo? terminal,
  }) {
    return _remoteRuntime.terminalScopeForSession(
      sessionId,
      terminal: terminal,
    );
  }

  RelayEnvelope? _scopeTerminalEnvelope(
    RelayEnvelope message, {
    TerminalInfo? terminal,
  }) {
    final sessionId = message.sessionId?.trim();
    if (sessionId == null || sessionId.isEmpty) return message;
    final scope = _terminalScopeForSession(sessionId, terminal: terminal);
    if (scope == null) {
      CoduxLog.warn(
        '[codux-flutter-terminal] drop ${message.type} reason=missing-scope session=$sessionId',
      );
      return null;
    }
    return scopedTerminalEnvelope(message, scope);
  }

  bool _canResizeTerminal(TerminalInfo? terminal) {
    return terminal != null && _isAccessibleTerminal(terminal);
  }

  List<TerminalInfo> _currentProjectTerminals() {
    return _remoteRuntime.currentProjectTerminals();
  }
}

int? _intFromPayload(Object? value) {
  if (value is int) return value;
  if (value is num) return value.toInt();
  return int.tryParse('${value ?? ''}');
}
