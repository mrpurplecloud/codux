use std::collections::{HashMap, HashSet, VecDeque};

pub struct RemoteSequenceGuard {
    max_entries_per_channel: usize,
    seen_by_channel: HashMap<String, RemoteSequenceWindow>,
}

impl RemoteSequenceGuard {
    pub fn new(max_entries_per_channel: usize) -> Self {
        Self {
            max_entries_per_channel,
            seen_by_channel: HashMap::new(),
        }
    }

    pub fn accept(&mut self, kind: &str, session_id: Option<&str>, sequence: Option<i64>) -> bool {
        let Some(sequence) = sequence else {
            return true;
        };
        let channel = sequence_channel_for(kind, session_id);
        let max_entries = self.max_entries_per_channel;
        let seen = self
            .seen_by_channel
            .entry(channel)
            .or_insert_with(|| RemoteSequenceWindow::new(max_entries));
        seen.accept(sequence, requires_monotonic_state(kind))
    }

    pub fn reset(&mut self) {
        self.seen_by_channel.clear();
    }
}

/// Sliding duplicate-detection window. `seen` answers "have we taken this
/// sequence already"; `order` remembers insertion order so the oldest entry can
/// be evicted once the window is full. Every inbound message hits this, so both
/// halves are O(1) -- a single `Vec` would make each message pay a linear scan
/// plus a linear shift off the front.
#[derive(Debug, Clone)]
struct RemoteSequenceWindow {
    max_entries: usize,
    seen: HashSet<i64>,
    order: VecDeque<i64>,
    max_sequence: i64,
}

impl RemoteSequenceWindow {
    fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            seen: HashSet::new(),
            order: VecDeque::new(),
            max_sequence: 0,
        }
    }

    fn accept(&mut self, sequence: i64, monotonic: bool) -> bool {
        if self.max_sequence > self.max_entries as i64
            && sequence <= self.max_sequence - self.max_entries as i64
        {
            return false;
        }
        if monotonic && sequence < self.max_sequence {
            return false;
        }
        if !self.seen.insert(sequence) {
            return false;
        }
        self.order.push_back(sequence);
        if sequence > self.max_sequence {
            self.max_sequence = sequence;
        }
        while self.order.len() > self.max_entries {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        true
    }
}

fn sequence_channel_for(kind: &str, session_id: Option<&str>) -> String {
    match session_id.map(str::trim).filter(|value| !value.is_empty()) {
        // Chunked baseline transfers are slow relative to the live output
        // flood on the same session; sharing one window would drop late
        // chunks as stale once 128 newer output frames have passed.
        Some(session_id) if kind == "terminal.buffer" => {
            format!("session:{session_id}:buffer")
        }
        Some(session_id) => format!("session:{session_id}"),
        None => format!("type:{kind}"),
    }
}

fn requires_monotonic_state(kind: &str) -> bool {
    matches!(
        kind,
        "project.list" | "project.selected" | "terminal.list" | "worktree.list" | "host.info"
    )
}
