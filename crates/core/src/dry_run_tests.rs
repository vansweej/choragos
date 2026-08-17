//! Tests covering `RunInputs.dry_run` behaviour end-to-end through the
//! orchestrator, using `FakeRunner` as the seam.

use crate::orchestrator::{run, RunInputs};
use crate::runner::fake::FakeRunner;
use crate::Config;

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

fn test_inputs(dry_run: bool) -> RunInputs {
    RunInputs {
        workspace: "/workspace".to_string(),
        repo: "my-repo".to_string(),
        plan_ref: "plan-ref-123".to_string(),
        profile: None,
        slug_override: None,
        trunk: RunInputs::default_trunk(),
        change_id: None,
        dry_run,
    }
}

#[tokio::test]
async fn dry_run_true_is_recorded_on_run_plan_cycle_calls() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);

    let _record = run(&runner, &test_cfg(3), test_inputs(true))
        .await
        .expect("run");

    let flags = runner.run_plan_cycle_dry_run_flags.lock().unwrap();
    assert_eq!(flags.len(), 1);
    assert!(
        flags[0],
        "dry_run=true must be passed through to run_plan_cycle"
    );
}

#[tokio::test]
async fn dry_run_false_is_recorded_on_run_plan_cycle_calls() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);

    let _record = run(&runner, &test_cfg(3), test_inputs(false))
        .await
        .expect("run");

    let flags = runner.run_plan_cycle_dry_run_flags.lock().unwrap();
    assert_eq!(flags.len(), 1);
    assert!(
        !flags[0],
        "dry_run=false must be passed through to run_plan_cycle"
    );
}

#[tokio::test]
async fn dry_run_skips_branch_push_and_pr_but_still_reaches_preconditions() {
    let mut runner = FakeRunner::new();
    runner.push_exit_code(0);
    runner.set_commits_ahead(1);

    let record = run(&runner, &test_cfg(3), test_inputs(true))
        .await
        .expect("run");

    // No branch create/switch, no push, no PR creation.
    let ops = runner.branch_ops.lock().unwrap();
    assert!(ops.is_empty(), "dry run must not create or switch branches");
    drop(ops);

    let push_calls = *runner.push_head_calls.lock().unwrap();
    assert_eq!(push_calls, 0, "dry run must not push");

    let create_pr_calls = *runner.create_pr_calls.lock().unwrap();
    assert_eq!(create_pr_calls, 0, "dry run must not create a PR");

    assert!(record.pr_url.is_none());

    // But the read-only preconditions (clean-start gate / base_sha /
    // plan-fetch / staleness-check) must still have run.
    let sessions_begun = runner.sessions_begun.lock().unwrap();
    assert_eq!(
        sessions_begun.len(),
        1,
        "dry run should still open a session after preconditions pass"
    );
}

#[tokio::test]
async fn non_dry_run_control_takes_the_mutating_path() {
    let mut runner = FakeRunner::new();
    runner.set_exit_codes([2, 2, 2]);
    runner.set_commits_ahead(1);

    let record = run(&runner, &test_cfg(3), test_inputs(false))
        .await
        .expect("run");

    assert_eq!(record.attempts, 3, "must retry up to max_attempts");

    let ops = runner.branch_ops.lock().unwrap();
    assert_eq!(ops.len(), 1, "a branch must be created for a real run");
    assert!(ops[0].starts_with("create:"));
    drop(ops);

    let flags = runner.run_plan_cycle_dry_run_flags.lock().unwrap();
    assert_eq!(flags.len(), 3);
    assert!(flags.iter().all(|f| !f));
}
