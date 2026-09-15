use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Error;
use codux_protocol::RemoteTerminalBufferWindow;
use tokio::sync::Notify;

use crate::terminal_pty::TerminalManager;

pub const REMOTE_TERMINAL_OUTPUT_BATCH_DELAY: Duration = Duration::from_millis(32);

#[derive(Debug, PartialEq, Eq)]
pub struct RemoteTerminalOutputBatch {
    pub data: String,
    pub buffer_length: usize,
    pub buffer_end: usize,
    pub viewers: HashSet<String>,
    pub output_seq: i64,
}

#[derive(Default)]
struct PendingRemoteTerminalOutput {
    data: String,
    buffer_length: usize,
    buffer_end: usize,
    viewers: HashSet<String>,
}

#[derive(Default)]
struct RemoteTerminalOutputSession {
    output_seq: i64,
    pending: Option<PendingRemoteTerminalOutput>,
}

#[derive(Clone, Default)]
pub struct RemoteTerminalOutputPipeline {
    sessions: Arc<Mutex<HashMap<String, RemoteTerminalOutputSession>>>,
    batch_taken: Arc<Notify>,
}

impl RemoteTerminalOutputPipeline {
    pub fn queue(
        &self,
        session_id: &str,
        data: String,
        buffer_length: usize,
        buffer_end: usize,
        viewers: impl IntoIterator<Item = String>,
    ) -> bool {
        if data.is_empty() {
            return false;
        }
        let viewers = viewers.into_iter().collect::<HashSet<_>>();
        if viewers.is_empty() {
            return false;
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let session = sessions.entry(session_id.to_string()).or_default();
        let should_schedule = session.pending.is_none();
        let pending = session
            .pending
            .get_or_insert_with(PendingRemoteTerminalOutput::default);
        pending.data.push_str(&data);
        pending.buffer_length = buffer_length;
        pending.buffer_end = pending.buffer_end.max(buffer_end);
        pending.viewers.extend(viewers);
        should_schedule
    }

    pub fn take(&self, session_id: &str) -> Option<RemoteTerminalOutputBatch> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let session = sessions.get_mut(session_id)?;
        let pending = session.pending.take()?;
        self.batch_taken.notify_waiters();
        if pending.data.is_empty() || pending.viewers.is_empty() {
            return None;
        }
        session.output_seq += 1;
        Some(RemoteTerminalOutputBatch {
            data: pending.data,
            buffer_length: pending.buffer_length,
            buffer_end: pending.buffer_end,
            viewers: pending.viewers,
            output_seq: session.output_seq,
        })
    }

    pub fn current_sequence(&self, session_id: &str) -> i64 {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(session_id)
            .map(|session| session.output_seq)
            .unwrap_or(0)
    }

    pub fn next_sequence(&self, session_id: &str) -> i64 {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let session = sessions.entry(session_id.to_string()).or_default();
        session.output_seq += 1;
        session.output_seq
    }

    pub fn reset_sequence(&self, session_id: &str) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(session_id)
        {
            session.output_seq = 0;
        }
    }

    pub async fn wait_for_pending_batch(&self, session_id: &str) {
        loop {
            let batch_taken = self.batch_taken.notified();
            tokio::pin!(batch_taken);
            batch_taken.as_mut().enable();
            let has_pending = self
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(session_id)
                .is_some_and(|session| session.pending.is_some());
            if !has_pending {
                return;
            }
            batch_taken.await;
        }
    }

    pub fn clear(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session_id);
        self.batch_taken.notify_waiters();
    }

    #[cfg(feature = "test-support")]
    pub fn set_sequence_for_test(&self, session_id: &str, output_seq: i64) {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry(session_id.to_string())
            .or_default()
            .output_seq = output_seq;
    }
}

pub async fn wait_for_remote_terminal_output_batch() {
    tokio::time::sleep(REMOTE_TERMINAL_OUTPUT_BATCH_DELAY).await;
}

pub fn terminal_live_baseline_window(
    terminals: &TerminalManager,
    output: &RemoteTerminalOutputPipeline,
    session_id: &str,
    max_chars: usize,
    request_id: Option<String>,
) -> Result<RemoteTerminalBufferWindow, Error> {
    // Read sequence before snapshot so baseline never claims a later live frame.
    // Absolute history watermark removes any prefix atomic snapshot covered.
    let output_seq = output.current_sequence(session_id);
    let baseline = terminals.baseline_snapshot(session_id, max_chars.max(1))?;
    let (screen_data, screen_wrapped_rows) = (!baseline.screen.data.is_empty())
        .then(|| {
            (
                Some(baseline.screen.data),
                Some(baseline.screen.wrapped_rows),
            )
        })
        .unwrap_or_default();
    Ok(RemoteTerminalBufferWindow {
        data: baseline.data,
        screen_data,
        screen_wrapped_rows,
        offset: baseline.offset,
        total_characters: baseline.buffer_length,
        buffer_end: Some(baseline.buffer_end),
        truncated: false,
        output_seq: Some(output_seq),
        request_id,
        tail: true,
        has_previous: baseline.offset > 0,
        baseline_failed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_sequence_covers_all_output_and_viewers_in_a_batch() {
        let pipeline = RemoteTerminalOutputPipeline::default();

        assert!(pipeline.queue(
            "session-1",
            "\x1b[7".to_string(),
            3,
            3,
            ["phone-a".to_string()],
        ));
        assert!(!pipeline.queue(
            "session-1",
            "mWorking".to_string(),
            11,
            11,
            ["phone-a".to_string(), "desktop-a".to_string()],
        ));
        assert_eq!(pipeline.current_sequence("session-1"), 0);

        let batch = pipeline.take("session-1").expect("pending batch");
        assert_eq!(batch.data, "\x1b[7mWorking");
        assert_eq!(batch.buffer_length, 11);
        assert_eq!(batch.buffer_end, 11);
        assert_eq!(batch.output_seq, 1);
        assert_eq!(
            batch.viewers,
            HashSet::from(["phone-a".to_string(), "desktop-a".to_string()])
        );
        assert_eq!(pipeline.current_sequence("session-1"), 1);
    }

    #[test]
    fn clear_removes_pending_output_and_sequence_together() {
        let pipeline = RemoteTerminalOutputPipeline::default();
        assert!(pipeline.queue(
            "session-1",
            "pending".to_string(),
            7,
            7,
            ["phone-a".to_string()],
        ));
        pipeline.clear("session-1");

        assert!(pipeline.take("session-1").is_none());
        assert_eq!(pipeline.current_sequence("session-1"), 0);
    }

    #[test]
    fn sequence_reset_preserves_an_already_queued_batch() {
        let pipeline = RemoteTerminalOutputPipeline::default();
        assert!(pipeline.queue(
            "session-1",
            "pending".to_string(),
            7,
            7,
            ["phone-a".to_string()],
        ));

        pipeline.reset_sequence("session-1");

        assert_eq!(pipeline.take("session-1").unwrap().data, "pending");
        assert_eq!(pipeline.current_sequence("session-1"), 1);
    }

    #[test]
    fn baseline_waits_for_the_current_pending_batch() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let pipeline = RemoteTerminalOutputPipeline::default();
                assert!(pipeline.queue(
                    "session-1",
                    "pending".to_string(),
                    7,
                    7,
                    ["phone-a".to_string()],
                ));
                let waiting_pipeline = pipeline.clone();
                let waiting = tokio::spawn(async move {
                    waiting_pipeline.wait_for_pending_batch("session-1").await;
                });
                tokio::task::yield_now().await;
                assert!(!waiting.is_finished());

                assert!(pipeline.take("session-1").is_some());
                waiting.await.unwrap();
            });
    }
}
