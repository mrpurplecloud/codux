//! Consumer-side terminal output orchestration.
//!
//! This is the state machine that decides, for each incoming `terminal.output`
//! envelope, whether it is a baseline, a duplicate, a sequence gap, or live
//! output to append, and which effects the UI must run (ack, loading,
//! buffer-received, session-updated, baseline-resync). It owns the per-session
//! remote PTY state, the output sequencer, the chunk assembler, and the buffer-
//! request bookkeeping.
//!
//! Previously this lived in the mobile app (Dart); it is ported here so the
//! shared core is the single home for remote PTY restore logic. Held live
//! frames are stored directly as the raw envelope JSON (`RemotePtySession<String>`),
//! so there is no token indirection.

use std::collections::{HashMap, HashSet};

use base64::{Engine as _, engine::general_purpose};
use serde_json::{Value, json};

use crate::{RemotePtySession, TerminalBufferAssembler, TerminalOutputSequencer, TerminalSequence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTerminalBufferPhase {
    Idle,
    Requesting,
    Receiving,
    Rendering,
}

impl RemoteTerminalBufferPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Requesting => "requesting",
            Self::Receiving => "receiving",
            Self::Rendering => "rendering",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTerminalOutputEffectKind {
    Loading,
    Ack,
    MarkBufferReceived,
    SessionUpdated,
    RequestBaselineResync,
}

impl RemoteTerminalOutputEffectKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ack => "ack",
            Self::MarkBufferReceived => "markBufferReceived",
            Self::SessionUpdated => "sessionUpdated",
            Self::RequestBaselineResync => "requestBaselineResync",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTerminalOutputEffect {
    pub kind: RemoteTerminalOutputEffectKind,
    pub session_id: Option<String>,
    pub output_seq: Option<TerminalSequence>,
    pub buffer_length: Option<i64>,
    pub progress: Option<f64>,
    pub phase: Option<RemoteTerminalBufferPhase>,
    pub loading: bool,
    pub baseline_screen_keyframe: Option<bool>,
}

/// Exact mutation a second terminal emulator must apply after routing one
/// wire frame. Baselines replace state atomically; accepted live bytes append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTerminalEmulatorUpdate {
    Append(Vec<u8>),
    Replace {
        history: Vec<u8>,
        screen: Vec<u8>,
        cols: usize,
        rows: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTerminalEmulatorResult {
    pub effects: Vec<RemoteTerminalOutputEffect>,
    pub update: Option<RemoteTerminalEmulatorUpdate>,
}

#[derive(Default)]
struct EmulatorUpdateCapture {
    appended: Vec<u8>,
    replace: bool,
}

impl EmulatorUpdateCapture {
    fn append(&mut self, bytes: &[u8]) {
        if !self.replace {
            self.appended.extend_from_slice(bytes);
        }
    }

    fn replace(&mut self) {
        self.replace = true;
        self.appended.clear();
    }
}

impl RemoteTerminalOutputEffect {
    fn loading(loading: bool, phase: RemoteTerminalBufferPhase, progress: Option<f64>) -> Self {
        Self {
            kind: RemoteTerminalOutputEffectKind::Loading,
            session_id: None,
            output_seq: None,
            buffer_length: None,
            progress,
            phase: Some(phase),
            loading,
            baseline_screen_keyframe: None,
        }
    }

    fn ack(
        session_id: &str,
        output_seq: Option<TerminalSequence>,
        buffer_length: Option<i64>,
    ) -> Self {
        Self {
            kind: RemoteTerminalOutputEffectKind::Ack,
            session_id: Some(session_id.to_string()),
            output_seq,
            buffer_length,
            progress: None,
            phase: None,
            loading: false,
            baseline_screen_keyframe: None,
        }
    }

    fn simple(kind: RemoteTerminalOutputEffectKind, session_id: &str) -> Self {
        Self {
            kind,
            session_id: Some(session_id.to_string()),
            output_seq: None,
            buffer_length: None,
            progress: None,
            phase: None,
            loading: false,
            baseline_screen_keyframe: None,
        }
    }

    fn mark_buffer_received(session_id: &str, baseline_screen_keyframe: bool) -> Self {
        Self {
            kind: RemoteTerminalOutputEffectKind::MarkBufferReceived,
            session_id: Some(session_id.to_string()),
            output_seq: None,
            buffer_length: None,
            progress: None,
            phase: None,
            loading: false,
            baseline_screen_keyframe: Some(baseline_screen_keyframe),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "sessionId": self.session_id,
            "outputSeq": self.output_seq,
            "bufferLength": self.buffer_length,
            "progress": self.progress,
            "phase": self.phase.map(RemoteTerminalBufferPhase::as_str),
            "loading": self.loading,
            "baselineScreenKeyframe": self.baseline_screen_keyframe,
        })
    }
}

struct DecodedPayload {
    data: String,
    is_buffer: bool,
    screen_data: Option<String>,
    screen_wrapped_rows: Option<Vec<bool>>,
    offset: Option<i64>,
    buffer_length: Option<i64>,
    buffer_end: Option<i64>,
    tail: bool,
    baseline_failed: bool,
}

pub struct RemoteTerminalOutputRouter {
    sessions: HashMap<String, RemotePtySession<String>>,
    /// Most-recently-bound session id is last; used for LRU eviction.
    order: Vec<String>,
    max_cached_chars: usize,
    sequencer: TerminalOutputSequencer,
    assembler: TerminalBufferAssembler,
    active_buffer_request_by_session: HashMap<String, String>,
    restore_buffer_request_ids: HashSet<String>,
    /// Sessions that completed a history baseline; live-only tail output does
    /// not make a cache safe to restore after a project switch.
    complete_baseline_sessions: HashSet<String>,
    gap_sessions: HashSet<String>,
    /// Per-session render generation, bumped whenever the screen could change.
    /// The UI caches the decoded screen snapshot keyed by this so it only
    /// re-decodes after a real mutation.
    render_gen: HashMap<String, u64>,
}

impl RemoteTerminalOutputRouter {
    pub fn new(max_buffer_chars: usize, max_cached_chars: usize) -> Self {
        Self {
            sessions: HashMap::new(),
            order: Vec::new(),
            max_cached_chars,
            sequencer: TerminalOutputSequencer::new(),
            assembler: TerminalBufferAssembler::new(max_buffer_chars),
            active_buffer_request_by_session: HashMap::new(),
            restore_buffer_request_ids: HashSet::new(),
            complete_baseline_sessions: HashSet::new(),
            gap_sessions: HashSet::new(),
            render_gen: HashMap::new(),
        }
    }

    fn bump_render(&mut self, session_id: &str) {
        *self.render_gen.entry(session_id.to_string()).or_insert(0) += 1;
    }

    pub fn render_generation(&self, session_id: &str) -> u64 {
        self.render_gen.get(session_id).copied().unwrap_or(0)
    }

    // ---- render path (screen operations on the owned sessions) ----------

    pub fn screen_snapshot_json(&self, session_id: &str) -> Option<String> {
        let session = self.sessions.get(session_id)?;
        serde_json::to_string(&session.screen_snapshot()).ok()
    }

    pub fn resize_screen(&mut self, session_id: &str, cols: usize, rows: usize) {
        self.ensure_session(session_id).resize_screen(cols, rows);
        self.bump_render(session_id);
    }

    pub fn scroll_screen_pixels(&mut self, session_id: &str, pixels: f64, cell_height: f64) {
        self.ensure_session(session_id)
            .scroll_screen_pixels(pixels, cell_height);
        self.bump_render(session_id);
    }

    pub fn settle_screen_pixel_scroll(&mut self, session_id: &str) {
        self.ensure_session(session_id).settle_screen_pixel_scroll();
        self.bump_render(session_id);
    }

    // ---- session access -------------------------------------------------

    /// The only writer of `order`. Removing any existing entry before pushing
    /// keeps a session listed exactly once, so callers can touch freely without
    /// having to know whether the session already exists.
    fn touch(&mut self, session_id: &str) {
        self.order.retain(|id| id != session_id);
        self.order.push(session_id.to_string());
    }

    fn session(&mut self, session_id: &str) -> &mut RemotePtySession<String> {
        if !self.sessions.contains_key(session_id) {
            self.sessions.insert(
                session_id.to_string(),
                RemotePtySession::new(self.max_cached_chars),
            );
            self.touch(session_id);
        }
        self.sessions.get_mut(session_id).expect("session exists")
    }

    pub fn content(&self, session_id: &str) -> Option<&str> {
        self.sessions
            .get(session_id)
            .map(|session| session.content())
            .filter(|content| !content.is_empty())
    }

    pub fn has_cached_output(&self, session_id: &str) -> bool {
        self.content(session_id).is_some()
    }

    pub fn has_complete_baseline(&self, session_id: &str) -> bool {
        self.complete_baseline_sessions.contains(session_id)
    }

    pub fn buffer_offset(&self, session_id: &str) -> usize {
        self.sessions
            .get(session_id)
            .map(|session| session.buffer_length())
            .unwrap_or(0)
    }

    pub fn has_sequence_gap(&self, session_id: &str) -> bool {
        self.gap_sessions.contains(session_id)
    }

    pub fn output_sequence(&self, session_id: &str) -> TerminalSequence {
        let session_sequence = self
            .sessions
            .get(session_id)
            .map(|session| session.sequence())
            .unwrap_or(0);
        session_sequence.max(self.sequencer.sequence_for(session_id))
    }

    pub fn session_ref(&self, session_id: &str) -> Option<&RemotePtySession<String>> {
        self.sessions.get(session_id)
    }

    pub fn session_mut(&mut self, session_id: &str) -> Option<&mut RemotePtySession<String>> {
        self.sessions.get_mut(session_id)
    }

    /// Create-if-absent and return a mutable handle (for the render path:
    /// resize/scroll/snapshot on the active session).
    pub fn ensure_session(&mut self, session_id: &str) -> &mut RemotePtySession<String> {
        self.touch(session_id);
        self.session(session_id)
    }

    // ---- lifecycle ------------------------------------------------------

    pub fn start_buffer_request(
        &mut self,
        session_id: &str,
        request_id: &str,
        require_baseline: bool,
        reset_assembler: bool,
        replace_active: bool,
    ) -> bool {
        if session_id.trim().is_empty() || request_id.trim().is_empty() {
            return false;
        }
        if let Some(active) = self
            .active_buffer_request_by_session
            .get(session_id)
            .cloned()
            && active != request_id
        {
            if !replace_active {
                return false;
            }
            self.restore_buffer_request_ids
                .remove(&buffer_request_key(session_id, &active));
        }
        self.active_buffer_request_by_session
            .insert(session_id.to_string(), request_id.to_string());
        let key = buffer_request_key(session_id, request_id);
        if require_baseline {
            self.restore_buffer_request_ids.insert(key);
        } else {
            self.restore_buffer_request_ids.remove(&key);
        }
        if reset_assembler {
            self.assembler.remove(session_id);
        }
        if require_baseline {
            self.complete_baseline_sessions.remove(session_id);
            self.session(session_id).require_baseline();
        }
        true
    }

    pub fn bind_session(&mut self, session_id: &str, require_baseline: bool) {
        if session_id.trim().is_empty() {
            return;
        }
        self.sequencer.remove(session_id);
        self.gap_sessions.remove(session_id);
        self.assembler.remove(session_id);
        if let Some(stale) = self.active_buffer_request_by_session.remove(session_id) {
            self.restore_buffer_request_ids
                .remove(&buffer_request_key(session_id, &stale));
        }
        if require_baseline {
            self.complete_baseline_sessions.remove(session_id);
        }
        let session = self.session(session_id);
        if require_baseline {
            session.require_baseline();
        } else {
            session.reset_transient(false);
        }
        self.touch(session_id);
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
        self.order.retain(|id| id != session_id);
        self.active_buffer_request_by_session.remove(session_id);
        self.restore_buffer_request_ids
            .retain(|key| !key.starts_with(&format!("{session_id}:")));
        self.complete_baseline_sessions.remove(session_id);
        self.assembler.remove(session_id);
        self.sequencer.remove(session_id);
        self.gap_sessions.remove(session_id);
        self.render_gen.remove(session_id);
    }

    /// Evict least-recently-bound inactive sessions beyond `max_sessions`,
    /// never evicting `active_session_id`. Returns the evicted ids.
    pub fn evict_inactive_sessions(
        &mut self,
        active_session_id: &str,
        max_sessions: usize,
    ) -> Vec<String> {
        if active_session_id.trim().is_empty() {
            return Vec::new();
        }
        self.touch(active_session_id);
        let max_sessions = max_sessions.max(1);
        let mut evicted = Vec::new();
        while self.sessions.len() > max_sessions {
            let victim = self
                .order
                .iter()
                .find(|id| id.as_str() != active_session_id)
                .cloned();
            let Some(victim) = victim else { break };
            self.remove_session(&victim);
            evicted.push(victim);
        }
        evicted
    }

    pub fn reset_transient(&mut self) {
        self.assembler.reset();
        self.active_buffer_request_by_session.clear();
        self.restore_buffer_request_ids.clear();
        for session in self.sessions.values_mut() {
            session.reset_transient(false);
        }
    }

    pub fn reset_session_transient(&mut self, session_id: &str, reset_sequence: bool) {
        self.assembler.remove(session_id);
        if reset_sequence {
            self.sequencer.remove(session_id);
            self.gap_sessions.remove(session_id);
        }
        self.session(session_id).reset_transient(reset_sequence);
    }

    pub fn reset_all(&mut self) {
        self.sessions.clear();
        self.order.clear();
        self.active_buffer_request_by_session.clear();
        self.restore_buffer_request_ids.clear();
        self.complete_baseline_sessions.clear();
        self.gap_sessions.clear();
        self.assembler.reset();
        self.sequencer.reset();
        self.render_gen.clear();
    }

    pub fn active_buffer_request_id(&self, session_id: &str) -> Option<&str> {
        self.active_buffer_request_by_session
            .get(session_id)
            .map(String::as_str)
    }

    pub fn has_active_buffer_request(&self, session_id: &str) -> bool {
        self.active_buffer_request_by_session
            .contains_key(session_id)
    }

    /// Cancel only the named request and replay live output parked behind it.
    /// `None` means the request was not current; a successful cancellation can
    /// contain no emulator update when no live frame was parked.
    pub fn cancel_buffer_request(
        &mut self,
        session_id: &str,
        request_id: &str,
        active_session_id: Option<&str>,
    ) -> Option<RemoteTerminalEmulatorResult> {
        if self.active_buffer_request_id(session_id) != Some(request_id) {
            return None;
        }
        self.active_buffer_request_by_session.remove(session_id);
        self.restore_buffer_request_ids
            .remove(&buffer_request_key(session_id, request_id));
        self.assembler.remove(session_id);
        let held_live = self.session(session_id).abort_baseline();
        let mut emulator = Some(EmulatorUpdateCapture::default());
        let mut effects = Vec::new();
        self.replay_held(
            held_live,
            active_session_id,
            &mut effects,
            &mut emulator,
        );
        let capture = emulator.expect("emulator capture remains present");
        Some(self.emulator_result(Some(session_id), effects, capture))
    }

    // ---- accept ---------------------------------------------------------

    pub fn accept(
        &mut self,
        message: &Value,
        active_session_id: Option<&str>,
    ) -> Vec<RemoteTerminalOutputEffect> {
        let mut emulator = None;
        self.accept_inner(message, active_session_id, false, &mut emulator)
    }

    /// Route one frame and return the exact update needed by a separate
    /// emulator. The regular [`Self::accept`] path stays allocation-free for
    /// render output beyond work already required by the shared router.
    pub fn accept_for_emulator(
        &mut self,
        message: &Value,
        active_session_id: Option<&str>,
    ) -> RemoteTerminalEmulatorResult {
        let mut emulator = Some(EmulatorUpdateCapture::default());
        let effects = self.accept_inner(message, active_session_id, false, &mut emulator);
        let capture = emulator.expect("emulator capture remains present");
        let session_id = message.get("sessionId").and_then(Value::as_str);
        self.emulator_result(session_id, effects, capture)
    }

    fn emulator_result(
        &self,
        session_id: Option<&str>,
        effects: Vec<RemoteTerminalOutputEffect>,
        capture: EmulatorUpdateCapture,
    ) -> RemoteTerminalEmulatorResult {
        let update = if capture.replace {
            session_id.and_then(|session_id| {
                let session = self.sessions.get(session_id)?;
                let history = session.content().as_bytes().to_vec();
                let snapshot = session.screen_replay_snapshot();
                Some(RemoteTerminalEmulatorUpdate::Replace {
                    history,
                    screen: snapshot.data.into_bytes(),
                    cols: snapshot.cols,
                    rows: snapshot.rows,
                })
            })
        } else if capture.appended.is_empty() {
            None
        } else {
            Some(RemoteTerminalEmulatorUpdate::Append(capture.appended))
        };
        RemoteTerminalEmulatorResult { effects, update }
    }

    fn accept_inner(
        &mut self,
        message: &Value,
        active_session_id: Option<&str>,
        replaying_held_live: bool,
        emulator: &mut Option<EmulatorUpdateCapture>,
    ) -> Vec<RemoteTerminalOutputEffect> {
        let payload = match message.get("payload") {
            Some(payload)
                if payload.is_object()
                    && payload.get("data").is_some_and(|value| !value.is_null()) =>
            {
                payload
            }
            _ => return Vec::new(),
        };
        let session_id = match message.get("sessionId").and_then(Value::as_str) {
            Some(session_id) if !session_id.trim().is_empty() => session_id.to_string(),
            _ => return Vec::new(),
        };
        let session_id = session_id.as_str();
        let is_active_session = active_session_id == Some(session_id);
        let had_cached_output_at_start = self.has_cached_output(session_id);
        let incoming_request_id = payload_string(&payload, "requestId");
        let active_request_id = self
            .active_buffer_request_by_session
            .get(session_id)
            .cloned();

        let is_buffer_flag = payload.get("buffer").and_then(Value::as_bool) == Some(true);

        if is_buffer_flag
            && active_request_id.is_some()
            && (had_cached_output_at_start || incoming_request_id.is_some())
            && incoming_request_id != active_request_id
        {
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                payload_int(&payload, "outputSeq"),
                payload_int(&payload, "bufferLength"),
            )];
        }
        if is_buffer_flag
            && active_request_id.is_none()
            && incoming_request_id.as_deref().map(str::is_empty) == Some(false)
            && self.has_cached_output(session_id)
        {
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                payload_int(&payload, "outputSeq"),
                payload_int(&payload, "bufferLength"),
            )];
        }
        if is_buffer_flag
            && active_request_id.is_none()
            && incoming_request_id.as_deref().map(str::is_empty) == Some(false)
        {
            let request_id = incoming_request_id.clone().unwrap();
            self.active_buffer_request_by_session
                .insert(session_id.to_string(), request_id.clone());
            if self.content(session_id).is_none() {
                self.restore_buffer_request_ids
                    .insert(buffer_request_key(session_id, &request_id));
            }
        }
        if is_buffer_flag
            && had_cached_output_at_start
            && !self
                .active_buffer_request_by_session
                .contains_key(session_id)
        {
            let payload_offset =
                payload_int(&payload, "startOffset").or_else(|| payload_int(&payload, "offset"));
            let payload_buffer_length = payload_int(&payload, "bufferLength");
            let payload_output_seq = payload_int(&payload, "outputSeq");
            let known_output_seq = self.sequencer.sequence_for(session_id);
            if payload_offset == Some(0)
                && payload_output_seq.is_some()
                && payload_output_seq.unwrap() <= known_output_seq
            {
                self.assembler.remove(session_id);
                return vec![RemoteTerminalOutputEffect::ack(
                    session_id,
                    payload_output_seq,
                    payload_buffer_length,
                )];
            }
            let buffer_offset = self.buffer_offset(session_id) as i64;
            if (payload_offset == Some(0)
                && payload_buffer_length.is_some()
                && payload_buffer_length.unwrap() > buffer_offset)
                || payload_offset.map(|offset| offset > 0) == Some(true)
            {
                self.assembler.remove(session_id);
                return vec![RemoteTerminalOutputEffect::ack(
                    session_id,
                    payload_int(&payload, "outputSeq"),
                    payload_buffer_length,
                )];
            }
        }

        let assembly = self.assembler.accept(session_id, payload);
        if !assembly.ready {
            if assembly.progress.is_none() {
                return Vec::new();
            }
            if !is_active_session {
                return Vec::new();
            }
            return vec![RemoteTerminalOutputEffect::loading(
                true,
                RemoteTerminalBufferPhase::Receiving,
                assembly.progress,
            )];
        }
        // Only a chunked transfer yields a replacement; every live frame keeps
        // the payload it arrived with, so nothing is copied on the hot path.
        let assembled = assembly.payload;
        let payload = assembled.as_ref().unwrap_or(payload);
        let decoded = decode_terminal_output_payload(payload);
        let raw = decoded.data.clone();
        let is_buffer = decoded.is_buffer;
        let output_seq = payload_int(&payload, "outputSeq");

        let active_request_id_after_assembly = self
            .active_buffer_request_by_session
            .get(session_id)
            .cloned();
        if is_buffer
            && had_cached_output_at_start
            && active_request_id_after_assembly.is_none()
            && decoded.offset == Some(0)
            && decoded.buffer_length.is_some()
            && decoded.buffer_length.unwrap() > self.buffer_offset(session_id) as i64
        {
            self.assembler.remove(session_id);
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                output_seq,
                decoded.buffer_length,
            )];
        }
        if is_buffer
            && had_cached_output_at_start
            && active_request_id_after_assembly.is_none()
            && decoded.offset.map(|offset| offset > 0) == Some(true)
        {
            self.assembler.remove(session_id);
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                output_seq,
                decoded.buffer_length,
            )];
        }

        // Hold live output that arrives before the baseline is restored.
        // Serializing the frame is deferred into the closure: `hold_live` only
        // calls it while the session is actually restoring, so streaming output
        // never pays for a frame nobody keeps.
        if !replaying_held_live
            && !is_buffer
            && self
                .session(session_id)
                .hold_live(output_seq, || message.to_string())
        {
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                output_seq,
                decoded.buffer_length,
            )];
        }

        if !is_active_session && !is_buffer {
            let resync = self
                .sequencer
                .observe(session_id, false, output_seq, None, false);
            if resync.should_render() && (!raw.is_empty() || decoded.screen_data.is_some()) {
                self.apply_live_to_session(
                    session_id,
                    &raw,
                    decoded.screen_data.as_deref(),
                    decoded.screen_wrapped_rows.as_deref(),
                    decoded.buffer_length,
                    decoded.buffer_end,
                    output_seq,
                    emulator,
                );
            }
            let mut effects = Vec::new();
            if resync.gap && self.gap_sessions.insert(session_id.to_string()) {
                effects.push(RemoteTerminalOutputEffect::simple(
                    RemoteTerminalOutputEffectKind::RequestBaselineResync,
                    session_id,
                ));
            }
            effects.push(RemoteTerminalOutputEffect::ack(
                session_id,
                output_seq,
                decoded.buffer_length,
            ));
            return effects;
        }

        let resync = self.sequencer.observe(
            session_id,
            is_buffer,
            output_seq,
            decoded
                .offset
                .and_then(|offset| usize::try_from(offset).ok()),
            decoded.tail,
        );
        if !resync.should_render() {
            return vec![RemoteTerminalOutputEffect::ack(
                session_id,
                output_seq,
                decoded.buffer_length,
            )];
        }

        let mut effects = Vec::new();
        if resync.gap && self.gap_sessions.insert(session_id.to_string()) {
            effects.push(RemoteTerminalOutputEffect::simple(
                RemoteTerminalOutputEffectKind::RequestBaselineResync,
                session_id,
            ));
        }
        let mut held_live: Vec<String> = Vec::new();

        if is_buffer {
            if decoded.baseline_failed && decoded.tail {
                held_live = self.complete_empty_baseline(session_id, output_seq);
                self.active_buffer_request_by_session.remove(session_id);
                self.remove_restore_request(session_id);
                self.gap_sessions.insert(session_id.to_string());
                if is_active_session {
                    effects.push(RemoteTerminalOutputEffect::loading(
                        false,
                        RemoteTerminalBufferPhase::Requesting,
                        None,
                    ));
                    effects.push(RemoteTerminalOutputEffect::simple(
                        RemoteTerminalOutputEffectKind::RequestBaselineResync,
                        session_id,
                    ));
                }
                effects.push(RemoteTerminalOutputEffect::ack(
                    session_id,
                    output_seq,
                    decoded.buffer_length,
                ));
                self.replay_held(held_live, active_session_id, &mut effects, emulator);
                return effects;
            }
            let active_request_id = self
                .active_buffer_request_by_session
                .get(session_id)
                .cloned();
            let local_cache_empty = self.content(session_id).is_none();
            let is_restore_request = active_request_id
                .as_ref()
                .map(|id| {
                    self.restore_buffer_request_ids
                        .contains(&buffer_request_key(session_id, id))
                })
                .unwrap_or(false);
            let is_baseline_restore = decoded.tail
                || self.session(session_id).is_restoring_baseline()
                || is_restore_request
                || local_cache_empty;
            let baseline_has_renderable_data = !raw.is_empty() || decoded.screen_data.is_some();
            if is_baseline_restore && !baseline_has_renderable_data {
                if decoded.tail {
                    held_live = self.complete_empty_baseline(session_id, output_seq);
                    if let Some(emulator) = emulator.as_mut() {
                        emulator.replace();
                    }
                    self.active_buffer_request_by_session.remove(session_id);
                    self.remove_restore_request(session_id);
                    if is_active_session {
                        effects.push(RemoteTerminalOutputEffect::loading(
                            false,
                            RemoteTerminalBufferPhase::Requesting,
                            None,
                        ));
                        effects.push(RemoteTerminalOutputEffect::simple(
                            RemoteTerminalOutputEffectKind::SessionUpdated,
                            session_id,
                        ));
                        effects.push(RemoteTerminalOutputEffect::mark_buffer_received(
                            session_id, false,
                        ));
                    }
                } else if is_active_session && active_request_id.is_some() {
                    effects.push(RemoteTerminalOutputEffect::loading(
                        true,
                        RemoteTerminalBufferPhase::Requesting,
                        None,
                    ));
                }
                effects.push(RemoteTerminalOutputEffect::ack(
                    session_id,
                    output_seq,
                    decoded.buffer_length,
                ));
                self.replay_held(held_live, active_session_id, &mut effects, emulator);
                return effects;
            }
            if is_baseline_restore {
                held_live = self.replace_session_from_baseline(
                    session_id,
                    &raw,
                    decoded.screen_data.as_deref(),
                    decoded.screen_wrapped_rows.as_deref(),
                    decoded.buffer_length,
                    decoded.buffer_end,
                    output_seq,
                );
                if let Some(emulator) = emulator.as_mut() {
                    emulator.replace();
                }
            }

            if is_baseline_restore || self.content(session_id).is_none() {
                if !is_baseline_restore {
                    self.replace_session_from_baseline(
                        session_id,
                        &raw,
                        decoded.screen_data.as_deref(),
                        decoded.screen_wrapped_rows.as_deref(),
                        decoded.buffer_length,
                        decoded.buffer_end,
                        output_seq,
                    );
                    if let Some(emulator) = emulator.as_mut() {
                        emulator.replace();
                    }
                }
                self.active_buffer_request_by_session.remove(session_id);
                self.remove_restore_request(session_id);
                if is_active_session {
                    effects.push(RemoteTerminalOutputEffect::simple(
                        RemoteTerminalOutputEffectKind::SessionUpdated,
                        session_id,
                    ));
                    effects.push(RemoteTerminalOutputEffect::mark_buffer_received(
                        session_id,
                        decoded.screen_data.is_some(),
                    ));
                }
            } else {
                self.apply_live_to_session(
                    session_id,
                    &raw,
                    decoded.screen_data.as_deref(),
                    decoded.screen_wrapped_rows.as_deref(),
                    decoded.buffer_length,
                    decoded.buffer_end,
                    output_seq,
                    emulator,
                );
                self.active_buffer_request_by_session.remove(session_id);
                self.remove_restore_request(session_id);
                if is_active_session {
                    effects.push(RemoteTerminalOutputEffect::simple(
                        RemoteTerminalOutputEffectKind::SessionUpdated,
                        session_id,
                    ));
                    effects.push(RemoteTerminalOutputEffect::mark_buffer_received(
                        session_id,
                        decoded.screen_data.is_some(),
                    ));
                }
            }
        } else if (!raw.is_empty() || decoded.screen_data.is_some()) && is_active_session {
            effects.push(RemoteTerminalOutputEffect::loading(
                false,
                RemoteTerminalBufferPhase::Requesting,
                None,
            ));
        }

        if !is_buffer && (!raw.is_empty() || decoded.screen_data.is_some()) {
            self.apply_live_to_session(
                session_id,
                &raw,
                decoded.screen_data.as_deref(),
                decoded.screen_wrapped_rows.as_deref(),
                decoded.buffer_length,
                decoded.buffer_end,
                output_seq,
                emulator,
            );
            if is_active_session {
                effects.push(RemoteTerminalOutputEffect::simple(
                    RemoteTerminalOutputEffectKind::SessionUpdated,
                    session_id,
                ));
            }
        }

        effects.push(RemoteTerminalOutputEffect::ack(
            session_id,
            output_seq,
            decoded.buffer_length,
        ));

        self.replay_held(held_live, active_session_id, &mut effects, emulator);
        effects
    }

    fn replay_held(
        &mut self,
        held_live: Vec<String>,
        active_session_id: Option<&str>,
        effects: &mut Vec<RemoteTerminalOutputEffect>,
        emulator: &mut Option<EmulatorUpdateCapture>,
    ) {
        for held in held_live {
            if let Ok(message) = serde_json::from_str::<Value>(&held) {
                let mut more = self.accept_inner(&message, active_session_id, true, emulator);
                effects.append(&mut more);
            }
        }
    }

    fn remove_restore_request(&mut self, session_id: &str) {
        let prefix = format!("{session_id}:");
        self.restore_buffer_request_ids
            .retain(|key| !key.starts_with(&prefix));
    }

    fn replace_session_from_baseline(
        &mut self,
        session_id: &str,
        data: &str,
        screen_data: Option<&str>,
        screen_wrapped_rows: Option<&[bool]>,
        buffer_length: Option<i64>,
        buffer_end: Option<i64>,
        output_seq: Option<TerminalSequence>,
    ) -> Vec<String> {
        self.gap_sessions.remove(session_id);
        let replay = self.session(session_id).replace_from_baseline(
            data,
            screen_data,
            screen_wrapped_rows,
            buffer_length.and_then(|value| usize::try_from(value).ok()),
            buffer_end.and_then(|value| usize::try_from(value).ok()),
            output_seq,
        );
        self.complete_baseline_sessions
            .insert(session_id.to_string());
        self.bump_render(session_id);
        replay
    }

    fn complete_empty_baseline(
        &mut self,
        session_id: &str,
        output_seq: Option<TerminalSequence>,
    ) -> Vec<String> {
        self.gap_sessions.remove(session_id);
        let replay = self.session(session_id).complete_empty_baseline(output_seq);
        self.complete_baseline_sessions
            .insert(session_id.to_string());
        self.bump_render(session_id);
        replay
    }

    fn apply_live_to_session(
        &mut self,
        session_id: &str,
        data: &str,
        screen_data: Option<&str>,
        screen_wrapped_rows: Option<&[bool]>,
        buffer_length: Option<i64>,
        buffer_end: Option<i64>,
        output_seq: Option<TerminalSequence>,
        emulator: &mut Option<EmulatorUpdateCapture>,
    ) {
        let buffer_len = buffer_length.and_then(|value| usize::try_from(value).ok());
        // A pure keyframe owns only the visible grid. Rebuilding through the
        // bounded raw cache would silently discard older headless history and
        // reset its absolute watermark. Baseline publication remains exclusive
        // to `buffer: true` handling above.
        if data.is_empty()
            && let Some(screen_data) = screen_data
        {
            self.session(session_id).reconcile_visible_keyframe(
                screen_data,
                screen_wrapped_rows,
                output_seq,
            );
            self.bump_render(session_id);
            return;
        }
        let uncovered_start = self.session(session_id).append_live(
            data,
            buffer_len,
            buffer_end.and_then(|value| usize::try_from(value).ok()),
            output_seq,
        );
        if let Some(emulator) = emulator.as_mut() {
            emulator.append(&data.as_bytes()[uncovered_start..]);
        }
        self.bump_render(session_id);
    }
}

fn buffer_request_key(session_id: &str, request_id: &str) -> String {
    format!("{session_id}:{request_id}")
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    let text = payload.get(key)?;
    let text = match text {
        Value::String(value) => value.trim().to_string(),
        Value::Null => return None,
        other => other.to_string(),
    };
    if text.is_empty() { None } else { Some(text) }
}

fn payload_int(payload: &Value, key: &str) -> Option<i64> {
    let value = payload.get(key)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| {
            value
                .as_str()
                .and_then(|value| value.trim().parse::<i64>().ok())
        })
}

fn decode_terminal_output_payload(payload: &Value) -> DecodedPayload {
    DecodedPayload {
        data: decode_data(payload),
        is_buffer: payload.get("buffer").and_then(Value::as_bool) == Some(true),
        screen_data: payload_string(payload, "screenData"),
        screen_wrapped_rows: payload_bool_array(payload, "screenWrappedRows"),
        offset: payload_int(payload, "offset"),
        buffer_length: payload_int(payload, "bufferLength"),
        buffer_end: payload_int(payload, "bufferEnd"),
        tail: payload.get("tail").and_then(Value::as_bool) == Some(true),
        baseline_failed: payload.get("baselineFailed").and_then(Value::as_bool) == Some(true),
    }
}

fn payload_bool_array(payload: &Value, key: &str) -> Option<Vec<bool>> {
    payload
        .get(key)?
        .as_array()?
        .iter()
        .map(Value::as_bool)
        .collect()
}

fn decode_data(payload: &Value) -> String {
    let value = payload
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if payload.get("compressed").and_then(Value::as_bool) != Some(true) {
        return value;
    }
    if payload.get("encoding").and_then(Value::as_str) != Some("base64+deflate+utf8") {
        return value;
    }
    let Ok(compressed) = general_purpose::URL_SAFE_NO_PAD.decode(value.trim_end_matches('='))
    else {
        return String::new();
    };
    use std::io::Read;
    let mut decoder = flate2::read::DeflateDecoder::new(compressed.as_slice());
    let mut output = String::new();
    if decoder.read_to_string(&mut output).is_err() {
        return String::new();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    struct BufferFlags {
        truncated: bool,
        tail: bool,
    }

    fn kinds(effects: &[RemoteTerminalOutputEffect]) -> Vec<&'static str> {
        effects.iter().map(|effect| effect.kind.as_str()).collect()
    }

    /// Visible text of a session's screen, read from the cell grid the renderer
    /// draws. The snapshot JSON no longer carries the ANSI repaint string, so
    /// searching the raw JSON for a word would only ever find it split across
    /// one cell per character.
    fn screen_text(router: &RemoteTerminalOutputRouter, session_id: &str) -> String {
        let json = router
            .screen_snapshot_json(session_id)
            .expect("screen snapshot");
        let snapshot: Value = serde_json::from_str(&json).expect("snapshot json");
        let mut rows: std::collections::BTreeMap<
            i64,
            std::collections::BTreeMap<i64, (String, i64)>,
        > = std::collections::BTreeMap::new();
        for cell in snapshot["cells"].as_array().into_iter().flatten() {
            let row = cell["row"].as_i64().unwrap_or_default();
            let col = cell["col"].as_i64().unwrap_or_default();
            let text = cell["text"].as_str().unwrap_or_default().to_string();
            // `width` is omitted for the single-column majority.
            let width = cell["width"].as_i64().unwrap_or(1).max(1);
            rows.entry(row).or_default().insert(col, (text, width));
        }
        // Blank default-styled cells are omitted, so spaces live in the gaps
        // between column numbers.
        rows.values()
            .map(|row| {
                let mut line = String::new();
                let mut next_col = 0i64;
                for (col, (text, width)) in row {
                    while next_col < *col {
                        line.push(' ');
                        next_col += 1;
                    }
                    line.push_str(text);
                    next_col += width;
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn screen_snapshot(router: &RemoteTerminalOutputRouter, session_id: &str) -> Value {
        serde_json::from_str(
            &router
                .screen_snapshot_json(session_id)
                .expect("screen snapshot"),
        )
        .expect("snapshot json")
    }

    fn buffer(
        session: &str,
        data: &str,
        offset: i64,
        buffer_length: i64,
        truncated: bool,
        output_seq: i64,
        request_id: Option<&str>,
    ) -> Value {
        buffer_with_tail(
            session,
            data,
            offset,
            buffer_length,
            BufferFlags {
                truncated,
                tail: false,
            },
            output_seq,
            request_id,
        )
    }

    fn buffer_with_tail(
        session: &str,
        data: &str,
        offset: i64,
        buffer_length: i64,
        flags: BufferFlags,
        output_seq: i64,
        request_id: Option<&str>,
    ) -> Value {
        let mut payload = json!({
            "data": data,
            "buffer": true,
            "offset": offset,
            "bufferLength": buffer_length,
            "truncated": flags.truncated,
            "tail": flags.tail,
            "outputSeq": output_seq,
        });
        if let Some(request_id) = request_id {
            payload["requestId"] = json!(request_id);
        }
        json!({ "type": "terminal.output", "sessionId": session, "payload": payload })
    }

    fn live(session: &str, data: &str, output_seq: i64) -> Value {
        json!({
            "type": "terminal.output",
            "sessionId": session,
            "payload": { "data": data, "outputSeq": output_seq },
        })
    }

    fn live_with_watermark(session: &str, data: &str, buffer_end: i64, output_seq: i64) -> Value {
        json!({
            "type": "terminal.output",
            "sessionId": session,
            "payload": {
                "data": data,
                "bufferLength": buffer_end,
                "bufferEnd": buffer_end,
                "outputSeq": output_seq,
            },
        })
    }

    fn buffer_with_screen_data(
        session: &str,
        data: &str,
        screen_data: &str,
        output_seq: i64,
    ) -> Value {
        json!({
            "type": "terminal.output",
            "sessionId": session,
            "payload": {
                "data": data,
                "screenData": screen_data,
                "buffer": true,
                "offset": 0,
                "bufferLength": data.len(),
                "tail": true,
                "outputSeq": output_seq,
            },
        })
    }

    #[test]
    fn truncated_baseline_renders_retained_window() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        router.bind_session("session-1", true);
        let result = router.accept(
            &buffer("session-1", "abcd", 0, 8, true, 10, None),
            Some("session-1"),
        );
        assert_eq!(
            kinds(&result),
            ["sessionUpdated", "markBufferReceived", "ack"]
        );
        assert_eq!(router.content("session-1"), Some("abcd"));
        assert_eq!(router.buffer_offset("session-1"), 8);
    }

    #[test]
    fn live_output_held_until_baseline() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        router.bind_session("session-1", true);
        let held = router.accept(&live("session-1", "new", 11), Some("session-1"));
        assert_eq!(kinds(&held), ["ack"]);
        assert_eq!(router.content("session-1"), None);

        let first = router.accept(
            &buffer("session-1", "old-", 0, 8, true, 10, None),
            Some("session-1"),
        );
        assert_eq!(
            kinds(&first),
            [
                "sessionUpdated",
                "markBufferReceived",
                "ack",
                "loading",
                "sessionUpdated",
                "ack"
            ]
        );
        assert_eq!(router.content("session-1"), Some("old-new"));
    }

    #[test]
    fn live_keyframe_waits_for_and_replays_after_buffer_baseline() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);
        assert!(router.start_buffer_request("session-1", "baseline-1", true, true, true));

        let held = router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "screenData": "\x1b[H\x1b[2Jfresh-live-screen",
                    "bufferLength": 8,
                    "outputSeq": 11,
                },
            }),
            Some("session-1"),
        );

        assert_eq!(kinds(&held), ["ack"]);
        assert_eq!(
            router.active_buffer_request_id("session-1"),
            Some("baseline-1")
        );
        assert!(
            router
                .session_ref("session-1")
                .is_some_and(RemotePtySession::is_restoring_baseline)
        );

        let mut baseline = buffer_with_screen_data(
            "session-1",
            "baseline-history",
            "\x1b[H\x1b[2Jbaseline-screen",
            10,
        );
        baseline["payload"]["requestId"] = json!("baseline-1");
        router.accept(&baseline, Some("session-1"));

        assert_eq!(router.active_buffer_request_id("session-1"), None);
        assert!(
            router
                .session_ref("session-1")
                .is_some_and(|session| !session.is_restoring_baseline())
        );
        let screen = screen_text(&router, "session-1");
        assert!(
            screen.contains("fresh-live-screen"),
            "replayed screen: {screen}"
        );
        assert!(
            !screen.contains("baseline-screen"),
            "held keyframe must reconcile after baseline: {screen}"
        );
    }

    #[test]
    fn emulator_update_replaces_chunked_baseline_after_interleaved_live_output() {
        let mut router = RemoteTerminalOutputRouter::new(65_536, 65_536);
        router.bind_session("session-1", true);
        router.resize_screen("session-1", 80, 24);
        assert!(router.start_buffer_request("session-1", "baseline-1", true, true, true));

        let first = router.accept_for_emulator(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "stale-head",
                    "buffer": true,
                    "chunked": true,
                    "snapshotId": "snapshot-1",
                    "chunkIndex": 0,
                    "chunkCount": 2,
                    "offset": 0,
                    "startOffset": 0,
                    "bufferLength": 20,
                    "bufferEnd": 20,
                    "outputSeq": 10,
                    "tail": true,
                    "requestId": "baseline-1",
                    "screenData": "\x1b[H\x1b[2Jbaseline-screen"
                }
            }),
            Some("session-1"),
        );
        assert_eq!(first.update, None);

        let live = router.accept_for_emulator(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "\x1b[Hlatest",
                    "bufferLength": 26,
                    "outputSeq": 11
                }
            }),
            Some("session-1"),
        );
        assert_eq!(live.update, None);

        let final_chunk = router.accept_for_emulator(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "stale-tail",
                    "buffer": true,
                    "chunked": true,
                    "snapshotId": "snapshot-1",
                    "chunkIndex": 1,
                    "chunkCount": 2,
                    "offset": 10,
                    "startOffset": 0,
                    "bufferLength": 20,
                    "bufferEnd": 20,
                    "outputSeq": 10,
                    "tail": true,
                    "requestId": "baseline-1"
                }
            }),
            Some("session-1"),
        );

        let Some(RemoteTerminalEmulatorUpdate::Replace {
            history, screen, ..
        }) = final_chunk.update
        else {
            panic!("completed baseline must produce one replacement");
        };
        assert!(String::from_utf8_lossy(&history).ends_with("\x1b[Hlatest"));
        let screen = String::from_utf8_lossy(&screen);
        assert!(
            screen.contains("latestne-screen"),
            "screen replay: {screen:?}"
        );
        assert!(router.has_complete_baseline("session-1"));
    }

    #[test]
    fn cancelling_emulator_baseline_replays_held_live_in_sequence_order() {
        let mut router = RemoteTerminalOutputRouter::new(65_536, 65_536);
        router.bind_session("session-1", true);
        assert!(router.start_buffer_request("session-1", "baseline-1", true, true, false));

        for (data, output_seq) in [("two", 2), ("one", 1)] {
            let result = router.accept_for_emulator(
                &json!({
                    "type": "terminal.output",
                    "sessionId": "session-1",
                    "payload": {
                        "data": data,
                        "bufferLength": 6,
                        "bufferEnd": output_seq * 3,
                        "outputSeq": output_seq
                    }
                }),
                Some("session-1"),
            );
            assert_eq!(result.update, None);
        }

        let cancelled = router
            .cancel_buffer_request("session-1", "baseline-1", Some("session-1"))
            .expect("current request must cancel");
        assert_eq!(
            cancelled.update,
            Some(RemoteTerminalEmulatorUpdate::Append(b"onetwo".to_vec()))
        );
        assert!(!router.has_active_buffer_request("session-1"));
        assert!(!router
            .session_ref("session-1")
            .unwrap()
            .is_restoring_baseline());
        assert!(router
            .cancel_buffer_request("session-1", "baseline-1", Some("session-1"))
            .is_none());
    }

    #[test]
    fn held_live_already_covered_by_baseline_is_not_duplicated() {
        let mut router = RemoteTerminalOutputRouter::new(64, 65536);
        router.bind_session("session-1", true);
        router.accept(
            &live_with_watermark("session-1", "overlap", 12, 11),
            Some("session-1"),
        );

        let mut baseline = buffer_with_tail(
            "session-1",
            "12345overlap",
            0,
            12,
            BufferFlags {
                truncated: false,
                tail: true,
            },
            10,
            None,
        );
        baseline["payload"]["bufferEnd"] = json!(12);
        router.accept(&baseline, Some("session-1"));

        assert_eq!(router.content("session-1"), Some("12345overlap"));
        assert_eq!(router.buffer_offset("session-1"), 12);
    }

    #[test]
    fn reset_transient_releases_sessions_waiting_for_baseline() {
        let mut router = RemoteTerminalOutputRouter::new(64, 65536);
        router.bind_session("session-1", true);
        assert!(
            router
                .session_ref("session-1")
                .is_some_and(RemotePtySession::is_restoring_baseline)
        );

        router.reset_transient();

        assert!(
            router
                .session_ref("session-1")
                .is_some_and(|session| !session.is_restoring_baseline())
        );
    }

    #[test]
    fn cached_content_is_raw_history_without_keyframe() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);

        // The baseline keyframe updates only the cell-grid screen; the native
        // render content stays the raw history so the emulator can rebuild its
        // scrollback (splicing the keyframe's ESC[2J would erase it).
        let effects = router.accept(
            &buffer_with_screen_data("session-1", "raw-history", "\x1b[2J\x1b[Hkeyframe", 10),
            Some("session-1"),
        );
        let received = effects
            .iter()
            .find(|effect| effect.kind == RemoteTerminalOutputEffectKind::MarkBufferReceived)
            .expect("baseline received effect");
        assert_eq!(received.baseline_screen_keyframe, Some(true));
        assert_eq!(
            received.to_json()["baselineScreenKeyframe"].as_bool(),
            Some(true)
        );

        assert_eq!(router.content("session-1"), Some("raw-history"));
        assert_eq!(router.content("session-1"), Some("raw-history"));

        // Live output appends its raw bytes; native render content tracks the
        // raw history exactly.
        router.accept(&live("session-1", "\nlive", 11), Some("session-1"));

        assert_eq!(router.content("session-1"), Some("raw-history\nlive"));
        assert_eq!(router.content("session-1"), Some("raw-history\nlive"));

        // A live frame that also carries a fresh screen keyframe still only
        // appends its raw bytes -- the keyframe never enters the stream.
        router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "\nworking",
                    "screenData": "\x1b[2J\x1b[Hupdated keyframe",
                    "outputSeq": 12,
                },
            }),
            Some("session-1"),
        );

        assert_eq!(
            router.content("session-1"),
            Some("raw-history\nlive\nworking")
        );
        assert_eq!(
            router.content("session-1"),
            Some("raw-history\nlive\nworking")
        );
    }

    #[test]
    fn baseline_restores_structured_soft_wrap_metadata() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);
        router.resize_screen("session-1", 10, 4);

        router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "screenData": "\x1b[H\x1b[2Jabcdefghij\x1b[2;1Hklmno\x1b[3;1Hsecond",
                    "screenWrappedRows": [true, false, false, false],
                    "buffer": true,
                    "offset": 0,
                    "bufferLength": 0,
                    "tail": true,
                    "outputSeq": 10,
                },
            }),
            Some("session-1"),
        );

        let snapshot: Value = serde_json::from_str(
            &router
                .screen_snapshot_json("session-1")
                .expect("screen snapshot"),
        )
        .expect("snapshot json");
        assert_eq!(snapshot["wrappedRows"], json!([true, false, false, false]));
    }

    #[test]
    fn empty_live_screen_keyframe_leaves_cached_content_untouched() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);

        router.accept(
            &buffer_with_screen_data("session-1", "raw-history", "\x1b[2J\x1b[Hold", 10),
            Some("session-1"),
        );

        // An empty-data live keyframe refreshes only the cell-grid screen used
        // by the scroll path; the raw native render content is left as-is.
        let effects = router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "screenData": "\x1b[2J\x1b[Hnew",
                    "bufferLength": 11,
                    "outputSeq": 11,
                },
            }),
            Some("session-1"),
        );

        assert_eq!(kinds(&effects), ["loading", "sessionUpdated", "ack"]);
        assert_eq!(router.content("session-1"), Some("raw-history"));
        assert_eq!(router.content("session-1"), Some("raw-history"));
    }

    #[test]
    fn nonawaiting_screen_keyframe_reconciles_drifted_screen() {
        // Regression: a viewport resize keyframe is captured before the TUI
        // repaints, so the consumer can latch a half-painted screen as its
        // baseline. The host then keeps sending a corrected keyframe, but a
        // session that is no longer *awaiting* a baseline used to drop it on the
        // floor (append-only live path), so the torn screen never self-healed.
        // A pure keyframe (empty data) must now reconcile the cell grid even
        // when not awaiting, while leaving the raw scrollback untouched.
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);
        router.accept(
            &buffer_with_screen_data("session-1", "raw-history", "\x1b[2J\x1b[Hold-screen", 10),
            Some("session-1"),
        );
        let before = screen_text(&router, "session-1");
        assert!(before.contains("old-screen"), "baseline screen: {before}");

        let effects = router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "screenData": "\x1b[2J\x1b[Hfresh-screen",
                    "bufferLength": 11,
                    "outputSeq": 11,
                },
            }),
            Some("session-1"),
        );
        assert_eq!(kinds(&effects), ["loading", "sessionUpdated", "ack"]);

        let after = screen_text(&router, "session-1");
        assert!(
            after.contains("fresh-screen"),
            "screen adopts keyframe: {after}"
        );
        assert!(
            !after.contains("old-screen"),
            "stale screen cleared: {after}"
        );
        // Raw history (native render content / scrollback) is left intact.
        assert_eq!(router.content("session-1"), Some("raw-history"));
    }

    #[test]
    fn pure_keyframe_preserves_full_screen_history_scroll_and_watermark() {
        let mut router = RemoteTerminalOutputRouter::new(1_000_000, 1_000_000);
        router.bind_session("session-1", true);
        router.resize_screen("session-1", 20, 8);
        let history = (0..900)
            .map(|index| format!("line-{index:04}"))
            .collect::<Vec<_>>()
            .join("\r\n");
        let history_end = history.chars().count() as i64;
        let mut baseline =
            buffer_with_screen_data("session-1", &history, "\x1b[H\x1b[2Jold-live-screen", 10);
        baseline["payload"]["bufferEnd"] = json!(history_end);
        router.accept(&baseline, Some("session-1"));

        let cached_lines = router
            .content("session-1")
            .expect("raw cache")
            .lines()
            .count();
        let full_before = screen_snapshot(&router, "session-1");
        let total_before = full_before["totalLines"].as_u64().expect("total lines");
        assert!(
            total_before > cached_lines as u64,
            "test must exceed capped raw cache"
        );

        router.scroll_screen_pixels("session-1", 120.0 * 20.0, 20.0);
        let scrolled_before = screen_snapshot(&router, "session-1");
        assert_eq!(scrolled_before["displayOffset"], json!(120));
        let watermark_before = router
            .session_ref("session-1")
            .and_then(RemotePtySession::buffer_end);

        router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "screenData": "\x1b[H\x1b[2Jfresh-live-screen",
                    "bufferLength": history_end,
                    "outputSeq": 11,
                },
            }),
            Some("session-1"),
        );

        let scrolled_after = screen_snapshot(&router, "session-1");
        assert_eq!(scrolled_after["totalLines"], json!(total_before));
        assert_eq!(scrolled_after["displayOffset"], json!(120));
        assert_eq!(
            router
                .session_ref("session-1")
                .and_then(RemotePtySession::buffer_end),
            watermark_before
        );

        router.scroll_screen_pixels("session-1", 100_000.0, 20.0);
        let oldest = screen_text(&router, "session-1");
        assert!(
            oldest.contains("line-0000") || oldest.contains("line-0001"),
            "oldest retained history vanished: {oldest}"
        );
        router.scroll_screen_pixels("session-1", -100_000.0, 20.0);
        let bottom = screen_text(&router, "session-1");
        assert!(
            bottom.contains("fresh-live-screen"),
            "bottom screen: {bottom}"
        );
    }

    #[test]
    fn empty_refresh_baseline_preserves_cached_content() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", false);
        router.accept(
            &buffer_with_screen_data("session-1", "history", "\x1b[2J\x1b[Hscreen", 10),
            Some("session-1"),
        );
        assert_eq!(router.content("session-1"), Some("history"));

        assert!(router.start_buffer_request("session-1", "refresh-empty", true, true, true));
        let empty = router.accept(
            &buffer_with_tail(
                "session-1",
                "",
                0,
                0,
                BufferFlags {
                    truncated: false,
                    tail: true,
                },
                11,
                Some("refresh-empty"),
            ),
            Some("session-1"),
        );

        assert_eq!(
            kinds(&empty),
            ["loading", "sessionUpdated", "markBufferReceived", "ack"]
        );
        assert_eq!(
            empty
                .iter()
                .find(|effect| effect.kind == RemoteTerminalOutputEffectKind::MarkBufferReceived)
                .and_then(|effect| effect.baseline_screen_keyframe),
            Some(false)
        );
        assert_eq!(router.content("session-1"), Some("history"));
        assert_eq!(router.active_buffer_request_id("session-1"), None);
    }

    #[test]
    fn empty_non_tail_refresh_baseline_stays_pending() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", false);
        router.accept(
            &buffer_with_screen_data("session-1", "history", "\x1b[2J\x1b[Hscreen", 10),
            Some("session-1"),
        );
        assert_eq!(router.content("session-1"), Some("history"));

        assert!(router.start_buffer_request("session-1", "refresh-empty", true, true, true));
        let empty = router.accept(
            &buffer("session-1", "", 0, 0, false, 11, Some("refresh-empty")),
            Some("session-1"),
        );

        assert_eq!(kinds(&empty), ["loading", "ack"]);
        assert_eq!(router.content("session-1"), Some("history"));
        assert_eq!(
            router.active_buffer_request_id("session-1"),
            Some("refresh-empty")
        );
    }

    #[test]
    fn empty_tail_baseline_closes_loading_without_cached_content() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);

        let empty = router.accept(
            &buffer_with_tail(
                "session-1",
                "",
                0,
                0,
                BufferFlags {
                    truncated: false,
                    tail: true,
                },
                11,
                Some("empty-tail"),
            ),
            Some("session-1"),
        );

        assert_eq!(
            kinds(&empty),
            ["loading", "sessionUpdated", "markBufferReceived", "ack"]
        );
        assert_eq!(
            empty
                .iter()
                .find(|effect| effect.kind == RemoteTerminalOutputEffectKind::MarkBufferReceived)
                .and_then(|effect| effect.baseline_screen_keyframe),
            Some(false)
        );
        assert_eq!(router.content("session-1"), None);
    }

    #[test]
    fn empty_tail_baseline_replays_held_live_output() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);

        let held = router.accept(&live("session-1", "live", 12), Some("session-1"));
        assert_eq!(kinds(&held), ["ack"]);
        assert_eq!(router.content("session-1"), None);

        let empty = router.accept(
            &buffer_with_tail(
                "session-1",
                "",
                0,
                0,
                BufferFlags {
                    truncated: false,
                    tail: true,
                },
                11,
                Some("empty-tail"),
            ),
            Some("session-1"),
        );

        assert_eq!(
            kinds(&empty),
            [
                "loading",
                "sessionUpdated",
                "markBufferReceived",
                "ack",
                "loading",
                "sessionUpdated",
                "ack",
            ]
        );
        assert_eq!(router.content("session-1"), Some("live"));
    }

    #[test]
    fn failed_empty_tail_baseline_requests_resync_without_marking_received() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);

        let failed = router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "",
                    "buffer": true,
                    "offset": 0,
                    "bufferLength": 0,
                    "truncated": false,
                    "tail": true,
                    "outputSeq": 11,
                    "requestId": "failed-tail",
                    "baselineFailed": true,
                },
            }),
            Some("session-1"),
        );

        assert_eq!(kinds(&failed), ["loading", "requestBaselineResync", "ack"]);
        assert!(router.has_sequence_gap("session-1"));
        assert_eq!(router.content("session-1"), None);
        assert_eq!(router.active_buffer_request_id("session-1"), None);
    }

    #[test]
    fn tail_refresh_replaces_raw_cached_content_for_cached_session() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", true);
        router.accept(
            &buffer_with_screen_data("session-1", "raw-history", "\x1b[2J\x1b[Hold", 10),
            Some("session-1"),
        );

        assert!(router.start_buffer_request("session-1", "tail-refresh", false, true, true));
        let refresh = router.accept(
            &json!({
                "type": "terminal.output",
                "sessionId": "session-1",
                "payload": {
                    "data": "raw-history\nnew",
                    "screenData": "\x1b[2J\x1b[Hnew",
                    "buffer": true,
                    "offset": 0,
                    "bufferLength": 15,
                    "tail": true,
                    "outputSeq": 10,
                    "requestId": "tail-refresh",
                },
            }),
            Some("session-1"),
        );

        assert_eq!(
            kinds(&refresh),
            ["sessionUpdated", "markBufferReceived", "ack"]
        );
        assert_eq!(router.content("session-1"), Some("raw-history\nnew"));
        assert_eq!(router.content("session-1"), Some("raw-history\nnew"));
    }

    #[test]
    fn live_sequence_gap_requests_resync_once() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        router.bind_session("session-1", false);
        router.accept(&live("session-1", "one", 1), Some("session-1"));
        assert!(!router.has_sequence_gap("session-1"));

        let skipped = router.accept(&live("session-1", "three", 3), Some("session-1"));
        assert_eq!(
            kinds(&skipped),
            ["requestBaselineResync", "loading", "sessionUpdated", "ack"]
        );
        assert_eq!(router.content("session-1"), Some("onethree"));
        assert!(router.has_sequence_gap("session-1"));

        let next = router.accept(&live("session-1", "six", 6), Some("session-1"));
        assert!(!kinds(&next).contains(&"requestBaselineResync"));
    }

    #[test]
    fn stale_request_id_baseline_cannot_replace() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        router.bind_session("session-1", true);
        router.start_buffer_request("session-1", "request-new", false, true, false);

        let stale = router.accept(
            &buffer("session-1", "old", 0, 3, false, 10, Some("request-old")),
            Some("session-1"),
        );
        assert_eq!(kinds(&stale), ["ack"]);
        assert_eq!(router.content("session-1"), None);

        let current = router.accept(
            &buffer("session-1", "new", 0, 3, false, 10, Some("request-new")),
            Some("session-1"),
        );
        assert_eq!(
            kinds(&current),
            ["sessionUpdated", "markBufferReceived", "ack"]
        );
        assert_eq!(router.content("session-1"), Some("new"));
    }

    #[test]
    fn duplicate_baseline_is_acked() {
        let mut router = RemoteTerminalOutputRouter::new(65536, 65536);
        router.bind_session("session-1", false);
        router.accept(
            &buffer("session-1", "cached", 0, 6, false, 10, None),
            Some("session-1"),
        );
        let result = router.accept(
            &buffer("session-1", "old-prefix", 0, 400000, true, 697, None),
            Some("session-1"),
        );
        assert_eq!(kinds(&result), ["ack"]);
        assert_eq!(router.content("session-1"), Some("cached"));
        assert_eq!(router.buffer_offset("session-1"), 6);
    }

    #[test]
    fn inactive_live_output_updates_cache_without_render() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        let result = router.accept(&live("session-2", "background", 1), Some("session-1"));
        assert_eq!(kinds(&result), ["ack"]);
        assert_eq!(router.content("session-2"), Some("background"));
        assert_eq!(router.content("session-1"), None);
    }

    #[test]
    fn evicts_inactive_sessions_keeping_active() {
        let mut router = RemoteTerminalOutputRouter::new(4, 65536);
        for id in ["s1", "s2", "s3"] {
            router.accept(&live(id, "x", 1), Some(id));
        }
        let evicted = router.evict_inactive_sessions("s3", 2);
        assert_eq!(evicted.len(), 1);
        assert!(router.session_ref("s3").is_some());
        assert!(!evicted.contains(&"s3".to_string()));
    }
}
