use super::state::{
    mark_project_completed, mark_project_progress, mark_project_queued, mark_project_running,
    project_source_fingerprint_unchanged, seed_or_queue_project_state,
    set_project_source_fingerprint,
};
use super::types::AIHistoryIndexerState;
use super::{AIHistoryEvent, AIHistoryIndexer};
use crate::normalized::{
    AIHistoryProjectRequest, AIHistorySnapshot, AIHistorySourceFileFingerprint,
    AIHistorySourceFingerprint, AIProjectUsageSummary,
    project_history_source_fingerprint_with_home,
};
use std::sync::{Arc, Mutex};

#[test]
fn configured_history_home_drives_worker_and_refresh_fingerprint() {
    let root = std::env::temp_dir().join(format!(
        "codux-indexer-history-home-{}",
        uuid::Uuid::new_v4()
    ));
    let history_home = root.join("history-home");
    let project_path = root.join("project");
    let project_path = project_path.to_string_lossy().to_string();
    let claude_project_dir = history_home
        .join(".claude/projects")
        .join(project_path.replace(['/', '.'], "-"));
    std::fs::create_dir_all(&claude_project_dir).unwrap();
    std::fs::write(
        claude_project_dir.join("session.jsonl"),
        format!(
            "{{\"type\":\"user\",\"sessionId\":\"s1\",\"cwd\":{project_path:?},\"timestamp\":\"2026-05-17T00:00:00Z\",\"message\":{{\"content\":\"hello\"}}}}\n\
             {{\"type\":\"assistant\",\"sessionId\":\"s1\",\"cwd\":{project_path:?},\"timestamp\":\"2026-05-17T00:01:00Z\",\"uuid\":\"a1\",\"message\":{{\"model\":\"claude-sonnet\",\"usage\":{{\"input_tokens\":100,\"output_tokens\":50}}}}}}\n"
        ),
    )
    .unwrap();
    let project = AIHistoryProjectRequest {
        id: "project-1".to_string(),
        name: "Project".to_string(),
        path: project_path,
    };
    let indexer = AIHistoryIndexer::with_database_path_and_history_home(
        root.join("ai-usage.sqlite3"),
        history_home.clone(),
    );

    let global = indexer.global_summary(vec![project.clone()]).unwrap();
    assert_eq!(global.total_tokens, 150);

    set_project_source_fingerprint(
        &indexer.state,
        &project.id,
        project_history_source_fingerprint_with_home(&project, &history_home),
    );
    indexer.drain_events();
    indexer.refresh_project(project).unwrap();
    let events = indexer.drain_events();
    assert!(events.iter().any(|event| matches!(
        event,
        AIHistoryEvent::ProjectState { state } if !state.is_loading && !state.queued
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        AIHistoryEvent::ProjectState { state } if state.is_loading || state.queued
    )));

    drop(indexer);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn project_state_tracks_queue_progress_and_completion() {
    let state = Arc::new(Mutex::new(AIHistoryIndexerState::default()));
    let project = test_project();

    let (queued, should_enqueue) = mark_project_queued(&state, &project, None).unwrap();
    assert!(should_enqueue);
    assert!(queued.is_loading);
    assert!(queued.queued);
    assert_eq!(queued.detail, "queued");
    assert_eq!(queued.progress, Some(0.0));
    assert_eq!(queued.version, 1);

    let (duplicate, should_enqueue_duplicate) =
        mark_project_queued(&state, &project, None).unwrap();
    assert!(!should_enqueue_duplicate);
    assert!(duplicate.is_loading);
    assert!(duplicate.queued);
    assert_eq!(duplicate.detail, "queued");
    assert_eq!(duplicate.progress, Some(0.0));
    assert!(duplicate.version > queued.version);

    let running = mark_project_running(&state, &project).unwrap();
    assert!(running.is_loading);
    assert!(!running.queued);
    assert_eq!(running.detail, "indexing");

    let progressed = mark_project_progress(&state, &project, 0.58, "readingSources").unwrap();
    assert!(progressed.is_loading);
    assert_eq!(progressed.progress, Some(0.58));
    assert_eq!(progressed.detail, "readingSources");

    let completed = mark_project_completed(&state, &project, test_snapshot()).unwrap();
    assert!(!completed.is_loading);
    assert!(!completed.queued);
    assert_eq!(completed.progress, Some(1.0));
    assert_eq!(completed.detail, "completed");
    assert!(completed.snapshot.is_some());
    assert!(
        !state
            .lock()
            .unwrap()
            .queued_or_running_projects
            .contains(&project.id)
    );
}

#[test]
fn project_source_fingerprint_tracks_changes_per_project() {
    let state = Arc::new(Mutex::new(AIHistoryIndexerState::default()));
    let fingerprint = AIHistorySourceFingerprint {
        files: vec![AIHistorySourceFileFingerprint {
            source: "codex".to_string(),
            path: "/tmp/project/session.jsonl".to_string(),
            modified_millis: 10,
            size: 128,
        }],
    };
    assert!(!project_source_fingerprint_unchanged(
        &state,
        "project-1",
        &fingerprint
    ));
    set_project_source_fingerprint(&state, "project-1", fingerprint.clone());
    assert!(project_source_fingerprint_unchanged(
        &state,
        "project-1",
        &fingerprint
    ));

    let changed = AIHistorySourceFingerprint {
        files: vec![AIHistorySourceFileFingerprint {
            size: 256,
            ..fingerprint.files[0].clone()
        }],
    };
    assert!(!project_source_fingerprint_unchanged(
        &state,
        "project-1",
        &changed
    ));
}

#[test]
fn cache_miss_project_state_is_queued_for_indexing() {
    let state = Arc::new(Mutex::new(AIHistoryIndexerState::default()));
    let project = test_project();

    let (queued, should_enqueue) = seed_or_queue_project_state(&state, &project, None).unwrap();

    assert!(should_enqueue);
    assert!(queued.is_loading);
    assert!(queued.queued);
    assert_eq!(queued.progress, Some(0.0));
    assert!(queued.snapshot.is_none());
    assert!(
        state
            .lock()
            .unwrap()
            .queued_or_running_projects
            .contains(&project.id)
    );
}

fn test_project() -> AIHistoryProjectRequest {
    AIHistoryProjectRequest {
        id: "project-1".to_string(),
        name: "Project".to_string(),
        path: "/tmp/project".to_string(),
    }
}

fn test_snapshot() -> AIHistorySnapshot {
    AIHistorySnapshot {
        project_id: "project-1".to_string(),
        project_name: "Project".to_string(),
        project_summary: AIProjectUsageSummary {
            project_id: "project-1".to_string(),
            project_name: "Project".to_string(),
            current_session_tokens: 0,
            current_session_cached_input_tokens: 0,
            project_total_tokens: 10,
            project_cached_input_tokens: 0,
            today_total_tokens: 10,
            today_cached_input_tokens: 0,
            usage_amounts: Vec::new(),
            today_usage_amounts: Vec::new(),
            current_tool: None,
            current_model: None,
            current_session_updated_at: None,
        },
        sessions: Vec::new(),
        heatmap: Vec::new(),
        today_time_buckets: Vec::new(),
        tool_breakdown: Vec::new(),
        model_breakdown: Vec::new(),
        indexed_at: 1.0,
    }
}
