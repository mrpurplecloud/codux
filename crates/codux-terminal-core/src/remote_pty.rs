use std::collections::BTreeMap;

use crate::{HeadlessTerminalScreen, TerminalScreenSnapshot, TerminalSequence};

/// Upper bound on live frames held while awaiting a baseline. A baseline that
/// never arrives (host torn down mid-request) would otherwise let the held
/// buffers grow without limit; past the cap we drop the oldest held frames.
const MAX_HELD_LIVE: usize = 2048;

/// History mutation chosen by the shared session, also applied by mirrors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTerminalHistoryUpdate {
    Replace(Vec<u8>),
    Append(Vec<u8>),
}

pub struct RemotePtyBaseline<T> {
    pub held_live: Vec<T>,
    pub history: RemoteTerminalHistoryUpdate,
}

pub struct RemotePtySession<T> {
    max_cached_chars: usize,
    content: String,
    buffer_length: usize,
    buffer_end: Option<usize>,
    sequence: TerminalSequence,
    history_screen: HeadlessTerminalScreen,
    awaiting_baseline: bool,
    held_sequenced_live: BTreeMap<TerminalSequence, T>,
    held_unsequenced_live: Vec<T>,
}

impl<T> RemotePtySession<T> {
    pub fn new(max_cached_chars: usize) -> Self {
        Self {
            max_cached_chars,
            content: String::new(),
            buffer_length: 0,
            buffer_end: None,
            sequence: 0,
            history_screen: HeadlessTerminalScreen::new(80, 24, 2_000),
            awaiting_baseline: false,
            held_sequenced_live: BTreeMap::new(),
            held_unsequenced_live: Vec::new(),
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn buffer_length(&self) -> usize {
        self.buffer_length
    }

    #[cfg(test)]
    pub(crate) fn buffer_end(&self) -> Option<usize> {
        self.buffer_end
    }

    pub fn sequence(&self) -> TerminalSequence {
        self.sequence
    }

    pub fn is_restoring_baseline(&self) -> bool {
        self.awaiting_baseline
    }

    pub fn screen_snapshot(&self) -> TerminalScreenSnapshot {
        // One screen owns both the replayed history and authoritative visible
        // keyframes, so live output, scrolling, and baseline restores cannot
        // diverge into separate render states.
        //
        // `include_data = false`: the consumer renders from `cells`, so the
        // ANSI repaint string is never read. Building it cost the screen worker
        // a full-viewport string per snapshot, and shipping it added tens of KB
        // to every frame the mobile renderer decoded.
        self.history_screen.snapshot_request(false).snapshot()
    }

    /// Snapshot including an ANSI repaint string for consumers that mirror
    /// this state into a separate terminal emulator.
    pub fn screen_replay_snapshot(&self) -> TerminalScreenSnapshot {
        self.history_screen.snapshot_request(true).snapshot()
    }

    pub fn resize_screen(&mut self, cols: usize, rows: usize) {
        self.history_screen.resize(cols, rows);
    }

    pub fn scroll_screen_pixels(&mut self, pixels: f64, cell_height: f64) {
        if !pixels.is_finite() || pixels == 0.0 || !cell_height.is_finite() || cell_height <= 0.0 {
            return;
        }
        self.history_screen.scroll_pixels(pixels, cell_height);
    }

    pub fn settle_screen_pixel_scroll(&mut self) {
        self.history_screen.settle_pixel_scroll();
    }

    pub fn require_baseline(&mut self) {
        self.awaiting_baseline = true;
        self.held_sequenced_live.clear();
        self.held_unsequenced_live.clear();
    }

    pub fn reset_transient(&mut self, reset_sequence: bool) {
        self.awaiting_baseline = false;
        self.held_sequenced_live.clear();
        self.held_unsequenced_live.clear();
        if reset_sequence {
            self.sequence = 0;
            self.buffer_end = None;
        }
    }

    /// Stop waiting for a baseline and return every parked live frame in
    /// sequence order. The caller must replay them before later live output.
    pub fn abort_baseline(&mut self) -> Vec<T> {
        self.drain_held_live(None)
    }

    /// Park a live frame until the baseline that precedes it arrives. Returns
    /// whether the frame was taken; a session that is not restoring keeps
    /// nothing.
    ///
    /// `output` is a closure rather than a value because building the frame is
    /// itself expensive -- the caller has to re-serialize an envelope carrying
    /// up to a 16 KB payload -- and the steady-state streaming path is not
    /// restoring, so nothing would be held. Owning that decision here means no
    /// caller has to re-derive "am I restoring" to avoid the cost, and a caller
    /// that forgets cannot silently pay it.
    pub fn hold_live(
        &mut self,
        sequence: Option<TerminalSequence>,
        output: impl FnOnce() -> T,
    ) -> bool {
        if !self.awaiting_baseline {
            return false;
        }
        let output = output();
        if let Some(sequence) = sequence {
            self.held_sequenced_live.entry(sequence).or_insert(output);
            // Drop the oldest held frames past the cap. The baseline replay and
            // sequence-gap resync repair any resulting hole.
            while self.held_sequenced_live.len() > MAX_HELD_LIVE {
                let oldest = *self
                    .held_sequenced_live
                    .keys()
                    .next()
                    .expect("non-empty held buffer");
                self.held_sequenced_live.remove(&oldest);
            }
        } else {
            self.held_unsequenced_live.push(output);
            if self.held_unsequenced_live.len() > MAX_HELD_LIVE {
                let overflow = self.held_unsequenced_live.len() - MAX_HELD_LIVE;
                self.held_unsequenced_live.drain(0..overflow);
            }
        }
        true
    }

    pub fn replace_from_baseline(
        &mut self,
        content: &str,
        screen_data: Option<&str>,
        screen_wrapped_rows: Option<&[bool]>,
        buffer_length: Option<usize>,
        buffer_end: Option<usize>,
        sequence: Option<TerminalSequence>,
    ) -> RemotePtyBaseline<T> {
        // Preserve the user's scroll position across a baseline replace: a
        // resync (e.g. after a dropped frame) rebuilds the buffer, and snapping
        // back to the bottom mid-scroll is jarring. If the user was scrolled up
        // by N lines, restore that distance from the new bottom.
        let prev_offset = self.history_screen.display_offset();
        // A bounded raw tail is not a replacement for rendered scrollback.
        // Keep the existing parser/history when the tail covers our watermark;
        // only an unknown stream, a new lifetime, or a gap requires rebuilding.
        let continuous = self
            .buffer_end
            .zip(buffer_end)
            .is_some_and(|(previous, end)| {
                previous <= end
                    && previous >= end.saturating_sub(content.chars().count())
                    && sequence.is_none_or(|sequence| sequence >= self.sequence)
            });
        if !continuous {
            self.content.clear();
            self.buffer_end = None;
            self.history_screen.clear();
        }
        let uncovered_start = self.append_live(content, buffer_length, buffer_end, sequence);
        let bytes = content.as_bytes()[uncovered_start..].to_vec();
        let history = if continuous {
            RemoteTerminalHistoryUpdate::Append(bytes)
        } else {
            RemoteTerminalHistoryUpdate::Replace(bytes)
        };
        if let Some(buffer_length) = buffer_length {
            self.buffer_length = buffer_length;
        }
        let mut rendered = !content.is_empty();
        // Reconstruct the current screen from the host keyframe. An alt-screen
        // TUI (e.g. Claude) keeps its UI outside raw scrollback, while a normal
        // screen keyframe may overlap the end of that history. Replace the
        // visible grid in place so the keyframe never pushes its old viewport
        // (including blank rows and partial redraws) into scrollback again.
        if let Some(screen_data) = screen_data
            && !screen_data.is_empty()
        {
            self.reconcile_visible_keyframe(screen_data, screen_wrapped_rows, sequence);
            rendered = true;
        }
        if rendered {
            self.history_screen.scroll_to_bottom();
            if prev_offset > 0 {
                self.history_screen.scroll_to_offset(prev_offset);
                // A rebuilt buffer can be shorter than the old scroll distance;
                // a clamped restore would strand the view at the very top, so
                // fall back to the bottom when the exact spot no longer exists.
                if self.history_screen.display_offset() != prev_offset {
                    self.history_screen.scroll_to_bottom();
                }
            }
        }
        let base_sequence = sequence.unwrap_or(self.sequence);
        self.sequence = base_sequence;
        RemotePtyBaseline {
            held_live: self.drain_held_live(Some(base_sequence)),
            history,
        }
    }

    /// Reconcile only the live grid from a host keyframe. Raw cache, complete
    /// screen history, scroll position, and absolute buffer watermark keep
    /// their existing ownership; a keyframe describes none of them.
    pub fn reconcile_visible_keyframe(
        &mut self,
        screen_data: &str,
        screen_wrapped_rows: Option<&[bool]>,
        sequence: Option<TerminalSequence>,
    ) {
        self.history_screen
            .replace_visible_with_keyframe(screen_data.as_bytes());
        if let Some(wrapped_rows) = screen_wrapped_rows {
            self.history_screen
                .restore_visible_wrapped_rows(wrapped_rows);
        }
        if let Some(sequence) = sequence {
            self.sequence = sequence;
        }
    }

    pub fn complete_empty_baseline(&mut self, sequence: Option<TerminalSequence>) -> Vec<T> {
        let base_sequence = sequence.unwrap_or(self.sequence);
        self.sequence = base_sequence;
        self.drain_held_live(Some(base_sequence))
    }

    fn drain_held_live(&mut self, after_sequence: Option<TerminalSequence>) -> Vec<T> {
        self.awaiting_baseline = false;
        let mut replay = Vec::new();
        for (sequence, output) in std::mem::take(&mut self.held_sequenced_live) {
            if after_sequence.is_none_or(|base| sequence > base) {
                replay.push(output);
            }
        }
        replay.append(&mut self.held_unsequenced_live);
        replay
    }

    pub fn append_live(
        &mut self,
        data: &str,
        buffer_length: Option<usize>,
        buffer_end: Option<usize>,
        sequence: Option<TerminalSequence>,
    ) -> usize {
        let data_chars = data.chars().count();
        let previous_end = self.buffer_end;
        let covered_chars = buffer_end
            .zip(previous_end)
            .map(|(buffer_end, previous_end)| {
                let frame_start = buffer_end.saturating_sub(data_chars);
                previous_end.saturating_sub(frame_start).min(data_chars)
            })
            .unwrap_or(0);
        let uncovered_start = if covered_chars == 0 {
            0
        } else {
            data.char_indices()
                .nth(covered_chars)
                .map(|(index, _)| index)
                .unwrap_or(data.len())
        };
        let uncovered = &data[uncovered_start..];
        if !uncovered.is_empty() {
            // The live view is the raw PTY history reflowed to the consumer's
            // grid. A baseline can already include a queued live frame, so use
            // the host's absolute history watermark to append only the suffix
            // not covered by that baseline. Follow the bottom only if we were
            // already there, so a user scrolled up into history stays put.
            let was_at_bottom = self.history_screen.display_offset() == 0;
            push_cache_buffer(&mut self.content, uncovered, self.max_cached_chars);
            self.history_screen.process(uncovered.as_bytes());
            if was_at_bottom {
                self.history_screen.scroll_to_bottom();
            }
        }
        let advances_watermark = match (previous_end, buffer_end) {
            (Some(previous), Some(current)) => current > previous,
            _ => true,
        };
        if advances_watermark {
            self.buffer_length = buffer_length.unwrap_or_else(|| {
                self.buffer_length
                    .saturating_add(data_chars.saturating_sub(covered_chars))
            });
        }
        self.buffer_end = match (previous_end, buffer_end) {
            (Some(previous), Some(current)) => Some(previous.max(current)),
            (None, Some(current)) => Some(current),
            (_, None) => None,
        };
        if let Some(sequence) = sequence {
            self.sequence = sequence;
        }
        uncovered_start
    }

    pub fn clear(&mut self) {
        self.content.clear();
        self.buffer_length = 0;
        self.buffer_end = None;
        self.sequence = 0;
        self.history_screen.clear();
        self.reset_transient(false);
    }
}

/// Trailing line budget for the cached raw history and native ANSI replay.
///
/// The native terminal emulator (iOS SwiftTerm / Android) keeps its own
/// ~500-line scrollback, so caching far more than it can hold only makes the
/// full re-feed on a session switch needlessly large (the emulator parses it
/// all and then discards everything past its scrollback). Bounding the cache
/// a little above that scrollback keeps a switch's `replace` small while still
/// fully repopulating the emulator.
const MAX_CACHED_LINES: usize = 600;

/// How far past [`MAX_CACHED_LINES`] the cache may grow before a trim runs.
///
/// Trimming means `String::drain` from the front, which memmoves the entire
/// retained window (~600 lines). Cutting the instant the budget is exceeded
/// would pay that memmove on every output frame that carries a newline. Letting
/// the buffer overshoot and then cutting back to the budget in one go amortizes
/// it over roughly this many appended lines instead.
const LINE_TRIM_SLACK: usize = MAX_CACHED_LINES / 8;

/// Append `data` to `buffer`, then trim the front to the cache budget. Appends
/// in place (no per-frame reallocation of the whole buffer).
fn push_cache_buffer(buffer: &mut String, data: &str, max_chars: usize) {
    buffer.push_str(data);
    trim_cache_buffer(buffer, max_chars);
}

/// Trim the front of `buffer` in place so it keeps roughly [`MAX_CACHED_LINES`]
/// trailing newline-delimited lines and at most `max_chars` characters.
///
/// The line budget is the primary bound -- it matches the native emulator's
/// scrollback so a restore re-feeds only what the emulator can hold.
/// `max_chars` is a safety ceiling that also bounds pathologically long lines.
///
/// Both bounds are hysteretic: the buffer is allowed to overshoot (by
/// [`LINE_TRIM_SLACK`] lines, or by 1/8 of the char ceiling) before a cut runs,
/// and each cut goes all the way back to the budget. Every scan is bounded by
/// the retained window rather than the whole buffer, and the `drain` memmove is
/// amortized over the overshoot, so the steady-state live path costs O(appended
/// bytes) per frame instead of O(buffer length).
///
/// The retained window is therefore in
/// `[MAX_CACHED_LINES, MAX_CACHED_LINES + LINE_TRIM_SLACK]` lines, not exactly
/// at the budget.
fn trim_cache_buffer(buffer: &mut String, max_chars: usize) {
    if max_chars == 0 {
        buffer.clear();
        return;
    }
    let bytes = buffer.as_bytes();
    let len = bytes.len();

    // Line budget: walk back from the end recording where a cut to
    // MAX_CACHED_LINES lines would land, but only commit it once the buffer has
    // also run past the slack. Cutting just after a newline is always a UTF-8
    // and line boundary, so the kept stream starts at a clean line.
    let mut cut = 0usize;
    let mut cut_to_budget = 0usize;
    let mut seen = 0usize;
    let mut i = len;
    while i > 0 {
        i -= 1;
        if bytes[i] == b'\n' {
            seen += 1;
            if seen == MAX_CACHED_LINES + 1 {
                cut_to_budget = i + 1;
            } else if seen > MAX_CACHED_LINES + LINE_TRIM_SLACK {
                cut = cut_to_budget;
                break;
            }
        }
    }

    // Char ceiling: only scan when the retained window is still over the byte
    // ceiling (bytes >= chars), then drop to 7/8 of the ceiling so the
    // pathological long-line case re-trims rarely rather than every frame.
    if len - cut > max_chars {
        let remaining = &buffer[cut..];
        let total = remaining.chars().count();
        if total > max_chars {
            let target = max_chars.saturating_sub(max_chars / 8).max(1);
            let drop = total - target;
            let extra = remaining
                .char_indices()
                .nth(drop)
                .map(|(index, _)| index)
                .unwrap_or(remaining.len());
            cut += extra;
        }
    }

    if cut > 0 {
        buffer.drain(..cut);
    }
}
