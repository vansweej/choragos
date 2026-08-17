//! Dedicated coverage tests for `orchestrator.rs`'s `produce`/`publish`
//! branches, using `FakeRunner`'s existing builder setters.

use crate::orchestrator::{run, RunInputs};
use crate::runner::fake::FakeRunner;
use crate::{Config, FailureClass};

fn test_cfg(max_attempts: u32) -> Config {
    Config {
        ai_coding_monorepo: "/ai".to_string(),
        default_profile: "default".to_string(),
        max_attempts,
        telegram_bot_token: None,
        telegram_chat_id: None,
        cerebrum_bin: "/nonexistent-cerebrum-bin".to_string(),
    }
}

fn test_inputs() -> RunInputs {
    RunInputs {
        workspace: "/workspace".to_string(),
        repo: "my-repo".to_string(),
        plan_ref: "plan-ref-123".to_string(),
        profile: None,
        slug_override: None,
        trunk: RunInputs::default_trunk(),
        change_id: None,
        dry_run: false,
    }
}

#[tokio::test]
async fn happy_path_green_with_pr() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Green);
    assert!(record.pr_url.is_some());
}

#[tokio::test]
async fn missing_ledger_correlation_prevents_green() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);
    runner.set_include_ledger_correlation(false);

    let record = run(&runner, &test_cfg(1), test_inputs()).await.expect("run");

    assert_ne!(record.failure_class, FailureClass::Green);
    assert!(
        record.reason.as_deref().unwrap_or("").contains("diagnosis"),
        "reason must mention diagnosis, got: {:?}",
        record.reason
    );
}

#[tokio::test]
async fn post_run_dirty_tree_invariant() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);
    runner.set_post_run_tree_dirty(true);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Red);
    assert!(record.reason.as_deref().unwrap_or("").contains("dirty"));
}

#[tokio::test]
async fn stale_head_invariant() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);
    runner.set_branch_exists(false);
    runner.set_branch_contains(false);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Red);
    let reason = record.reason.as_deref().unwrap_or("");
    assert!(
        reason.contains("descend") || reason.contains("divergent"),
        "reason should mention descend/divergent, got: {reason}"
    );
}

#[tokio::test]
async fn pr_creation_failure_degrades_gracefully() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);
    runner.set_create_pr_should_fail(true);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Green);
    assert!(record.pr_url.is_none());
    assert!(record
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("PR creation failed"));
}

#[tokio::test]
async fn existing_pr_is_reused() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);
    runner.set_existing_pr(Some("https://github.com/x/y/pull/9"));

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Green);
    assert_eq!(
        record.pr_url.as_deref(),
        Some("https://github.com/x/y/pull/9")
    );

    let create_pr_calls = *runner.create_pr_calls.lock().unwrap();
    assert_eq!(create_pr_calls, 0);
}

#[tokio::test]
async fn no_changes_to_land() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(0);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Green);
    assert!(record.pr_url.is_none());
    assert_eq!(record.reason.as_deref(), Some("no changes to land"));
}

#[tokio::test]
async fn orange_after_exhausted_retries() {
    let mut runner = FakeRunner::new();
    runner.set_exit_codes([2, 2, 2]);
    runner.set_commits_ahead(1);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Orange);
    assert!(record.pr_url.is_none());
    assert!(record
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("max attempts reached"));
}

#[tokio::test]
async fn red_on_hard_failure_exit_code() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(3);

    let record = run(&runner, &test_cfg(3), test_inputs()).await.expect("run");

    assert_eq!(record.failure_class, FailureClass::Red);
    assert!(record.pr_url.is_none());
    assert!(record
        .reason
        .as_deref()
        .unwrap_or("")
        .contains("hard failure"));
}
