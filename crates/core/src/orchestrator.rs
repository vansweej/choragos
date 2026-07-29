//! Orchestrator: the top-level plan-cycle run flow.
//!
//! [`run`] drives the entire lifecycle: clean-start gate, branch management,
//! retry loop, PR creation, ledger append, and Telegram notification.
//!
//! Internally the post-gate flow is split into two private phases:
//! [`produce`] (branch management, the retry loop, and post-run git state)
//! and [`publish`] (failure-class derivation and the idempotent
//! find-or-create PR decision). This mirrors the produce/publish
//! distinction used elsewhere in choragos: `produce` only touches the local
//! repo and the plan-cycle executor, while `publish` is the only phase that
//! pushes state to the outside world (git push, PR creation).

use crate::telegram::render;

/// Inputs supplied by the caller (MCP tool or CLI) for a single run.
#[derive(Debug, Clone)]
pub struct RunInputs {
    /// Absolute path to the workspace repository root.
    pub workspace: String,
    /// Repository name (typically the workspace directory basename).
    pub repo: String,
    /// Reference to the plan stored in cerebrum (a memory id), resolved via
    /// [`crate::CommandRunner::fetch_plan`].
    pub plan_ref: String,
    /// Pipeline profile to use; falls back to [`crate::Config::default_profile`]
    /// when `None`.
    pub profile: Option<String>,
    /// Override the auto-derived branch slug; when `None` the slug is derived
    /// from the plan title.
    pub slug_override: Option<String>,
}

/// Result of the `produce` phase: everything needed to derive a
/// [`crate::LedgerRecord`] and decide whether to publish a PR, except the
/// failure-class/PR decision itself (that's [`publish`]'s job).
struct ProduceOutcome {
    branch: String,
    slug: String,
    title: String,
    base_sha: String,
    head_sha: String,
    commits_ahead: u32,
    exit_code: i32,
    attempts: u32,
}

/// Runs plan fetch, branch management, the retry loop, and captures
/// post-run git state. Does not touch anything outside the local repo and
/// the plan-cycle executor — no push, no PR, no ledger, no Telegram.
async fn produce<R: crate::CommandRunner>(
    runner: &R,
    cfg: &crate::Config,
    inputs: &RunInputs,
    profile: &str,
    session: &str,
    base_sha: &str,
) -> Result<ProduceOutcome, crate::CoreError> {
    // ── Read plan and derive branch ───────────────────────────────────────
    let plan_contents = runner.fetch_plan(&inputs.plan_ref).await?;
    let title = crate::plan::parse_title(&plan_contents).ok_or_else(|| {
        crate::CoreError::Message(format!(
            "no level-1 heading found in plan '{}'",
            inputs.plan_ref
        ))
    })?;
    let slug = inputs
        .slug_override
        .clone()
        .unwrap_or_else(|| crate::plan::slugify(&title));
    let branch = crate::plan::branch_name(&slug);

    if runner.branch_exists(&branch).await? {
        runner.switch_branch(&branch).await?;
    } else {
        runner.create_branch(&branch).await?;
    }

    // ── Retry loop ────────────────────────────────────────────────────────
    let mut code = 0i32;
    let mut attempts = 0u32;
    for attempt in 1..=cfg.max_attempts {
        let _ = runner
            .note_progress(session, &format!("attempt {attempt} started"))
            .await;
        code = runner
            .run_plan_cycle(&inputs.workspace, &inputs.plan_ref, profile, session)
            .await?;
        attempts = attempt;
        if code == 0 || code == 3 {
            break;
        }
        // code == 2: continue retrying
    }

    // ── Post-run state ────────────────────────────────────────────────────
    let head_sha = runner.head_sha().await?;
    let commits_ahead = runner.commits_ahead(base_sha).await?;

    Ok(ProduceOutcome {
        branch,
        slug,
        title,
        base_sha: base_sha.to_string(),
        head_sha,
        commits_ahead,
        exit_code: code,
        attempts,
    })
}

/// Derives the [`crate::FailureClass`] and PR decision from a
/// [`ProduceOutcome`].
///
/// On a green run with commits ahead, this is the only phase that pushes to
/// the outside world: it pushes `HEAD` then does an idempotent
/// find-or-create PR lookup (reuse an existing open PR rather than fail or
/// duplicate one on a resumed run). Any push/PR failure degrades gracefully
/// to Green + `pr_url: None` + an explanatory `reason` — the plan itself
/// succeeded, so a sink failure must never turn a green run red.
async fn publish<R: crate::CommandRunner>(
    runner: &R,
    outcome: &ProduceOutcome,
) -> (crate::FailureClass, Option<String>, Option<String>) {
    // Post-run invariant: green exit but dirty tree → Red override.
    let post_run_clean = runner.is_working_tree_clean().await.unwrap_or_default();

    if outcome.exit_code == 0 && !post_run_clean {
        return (
            crate::FailureClass::Red,
            None,
            Some("executor left tree dirty (pipeline invariant violation)".to_string()),
        );
    }

    match crate::FailureClass::from_exit_code(outcome.exit_code) {
        crate::FailureClass::Green => {
            if outcome.commits_ahead > 0 {
                let body = format!(
                    "Automated run of plan `{plan_id}` on branch `{branch}`.",
                    plan_id = outcome.slug,
                    branch = outcome.branch,
                );

                // Push, then find-or-create: reuse an existing open PR if
                // one is already there (idempotent resume), otherwise
                // create one. Any failure along this path is best-effort —
                // it must not turn a successful plan run red.
                match runner.push_head().await {
                    Ok(()) => match runner.find_pr(&outcome.branch).await {
                        Ok(Some(url)) => (crate::FailureClass::Green, Some(url), None),
                        Ok(None) => match runner.create_pr("main", &outcome.title, &body).await {
                            Ok(url) => (crate::FailureClass::Green, Some(url), None),
                            Err(e) => (
                                crate::FailureClass::Green,
                                None,
                                Some(format!("plan succeeded but PR creation failed: {e}")),
                            ),
                        },
                        Err(e) => (
                            crate::FailureClass::Green,
                            None,
                            Some(format!("plan succeeded but PR lookup failed: {e}")),
                        ),
                    },
                    Err(e) => (
                        crate::FailureClass::Green,
                        None,
                        Some(format!("plan succeeded but branch push failed: {e}")),
                    ),
                }
            } else {
                (
                    crate::FailureClass::Green,
                    None,
                    Some("no changes to land".to_string()),
                )
            }
        }
        crate::FailureClass::Orange => (
            crate::FailureClass::Orange,
            None,
            Some("max attempts reached without success".to_string()),
        ),
        crate::FailureClass::Red => (
            crate::FailureClass::Red,
            None,
            Some("plan cycle exited with hard failure".to_string()),
        ),
    }
}

/// Runs the full plan-cycle orchestration flow.
///
/// # Flow
///
/// 1. **Clean-start gate** — fetches `origin/main`, then verifies that the
///    current branch is `main`, the working tree is clean, and the local
///    `main` matches the remote.  Any failure returns a Red [`crate::LedgerRecord`]
///    immediately without creating a branch.
/// 2. **Branch management** — captures `base_sha`, reads the plan, derives
///    the slug and branch name, then creates or switches to the feature branch.
/// 3. **Retry loop** — calls `run_plan_cycle` up to `cfg.max_attempts` times,
///    stopping early on exit codes `0` (green) or `3` (red).
/// 4. **Post-run invariant** — if the executor exited `0` but left the tree
///    dirty the result is overridden to Red.
/// 5. **PR decision** — opens a PR only on a green run with commits ahead of
///    `base_sha`.
/// 6. **Finalise** — appends the ledger record and sends a Telegram
///    notification (both best-effort; errors are logged and swallowed).
///
/// # Errors
///
/// Returns [`crate::CoreError`] for hard failures such as an unreadable plan
/// file or a missing plan title.  Telegram and ledger errors are swallowed.
pub async fn run<R: crate::CommandRunner>(
    runner: &R,
    cfg: &crate::Config,
    inputs: RunInputs,
) -> Result<crate::LedgerRecord, crate::CoreError> {
    let started_at = chrono::Utc::now().to_rfc3339();
    let profile = inputs
        .profile
        .clone()
        .unwrap_or_else(|| cfg.default_profile.clone());

    // ── Clean-start gate ─────────────────────────────────────────────────
    runner.git_fetch("origin", "main").await?;

    let abort_reason: Option<String> = {
        let branch = runner.current_branch().await?;
        if branch != "main" {
            Some(format!("current branch is '{branch}', not 'main'"))
        } else if !runner.is_working_tree_clean().await? {
            Some("working tree is not clean".to_string())
        } else if !runner.local_matches_remote("main").await? {
            Some("local main is behind remote".to_string())
        } else {
            None
        }
    };

    if let Some(reason) = abort_reason {
        let finished_at = chrono::Utc::now().to_rfc3339();
        let record = crate::LedgerRecord {
            plan_id: String::new(),
            repo: inputs.repo.clone(),
            branch: "main".to_string(),
            profile: profile.clone(),
            exit_code: -1,
            attempts: 0,
            failure_class: crate::FailureClass::Red,
            base_sha: String::new(),
            head_sha: String::new(),
            commits_ahead: 0,
            pr_url: None,
            reason: Some(reason),
            started_at,
            finished_at,
            schema_version: crate::ledger::CURRENT_SCHEMA_VERSION,
            change_id: None,
        };
        let _ = runner.append_ledger(&record).await;
        let _ = runner.send_telegram(&render(&record)).await;
        return Ok(record);
    }

    // ── Capture base_sha BEFORE creating any branch ───────────────────────
    let base_sha = runner.head_sha().await?;

    // ── Open a cerebrum session for this run ──────────────────────────────
    //
    // The session outlives individual plan-cycle attempts so that a retry
    // can recall progress notes left behind by an earlier attempt.
    let session = runner.begin_session(&inputs.plan_ref).await?;

    // ── Produce: plan fetch, branch mgmt, retry loop, post-run git state ──
    let outcome = produce(runner, cfg, &inputs, &profile, &session, &base_sha).await?;

    // ── Publish: failure-class + idempotent find-or-create PR decision ───
    let (failure_class, pr_url, reason) = publish(runner, &outcome).await;

    let finished_at = chrono::Utc::now().to_rfc3339();
    let record = crate::LedgerRecord {
        plan_id: outcome.slug.clone(),
        repo: inputs.repo.clone(),
        branch: outcome.branch.clone(),
        profile: profile.clone(),
        exit_code: outcome.exit_code,
        attempts: outcome.attempts,
        failure_class,
        base_sha: outcome.base_sha.clone(),
        head_sha: outcome.head_sha.clone(),
        commits_ahead: outcome.commits_ahead,
        pr_url,
        reason,
        started_at,
        finished_at,
        schema_version: crate::ledger::CURRENT_SCHEMA_VERSION,
        change_id: None,
    };

    // Append ledger (propagate errors — caller may want to know).
    runner.append_ledger(&record).await?;

    // Send Telegram best-effort: log and swallow any error.
    if let Err(e) = runner.send_telegram(&render(&record)).await {
        eprintln!("choragos: telegram notification failed (ignored): {e}");
    }

    // Clean up the cerebrum session best-effort: log and swallow any error.
    // This is a SCOPED cleanup of this session's own memories only — never a
    // global session clear, which would affect other concurrent sessions
    // sharing the same cerebrum store.
    if let Err(e) = runner.cleanup_session(&session).await {
        eprintln!("choragos: session cleanup failed (ignored): {e}");
    }

    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::{run, RunInputs};
    use crate::runner::fake::FakeRunner;
    use crate::{Config, FailureClass};

    fn test_cfg(max_attempts: u32) -> Config {
        Config {
            ai_coding_monorepo: "/ai".to_string(),
            default_profile: "default".to_string(),
            max_attempts,
            telegram_bot_token: None,
            telegram_chat_id: None,
        }
    }

    fn test_inputs() -> RunInputs {
        RunInputs {
            workspace: "/workspace".to_string(),
            repo: "my-repo".to_string(),
            plan_ref: "plan-ref-123".to_string(),
            profile: None,
            slug_override: None,
        }
    }

    // ── Clean-start gate tests ────────────────────────────────────────────

    #[tokio::test]
    async fn dirty_tree_yields_red_and_no_branch() {
        let mut runner = FakeRunner::new();
        runner.set_tree_clean(false);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.exit_code, -1);
        assert_eq!(record.attempts, 0);
        assert!(record.reason.is_some());

        let ops = runner.branch_ops.lock().unwrap();
        assert!(ops.is_empty(), "no branch should be created on abort");
        drop(ops);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1, "append_ledger called once");
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1, "send_telegram attempted once");
    }

    #[tokio::test]
    async fn off_main_yields_red_and_no_branch() {
        let mut runner = FakeRunner::new();
        runner.set_on_main(false);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.attempts, 0);

        let ops = runner.branch_ops.lock().unwrap();
        assert!(ops.is_empty());
        drop(ops);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn main_behind_remote_yields_red_and_no_branch() {
        let mut runner = FakeRunner::new();
        runner.set_local_matches_remote(false);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.attempts, 0);

        let ops = runner.branch_ops.lock().unwrap();
        assert!(ops.is_empty());
        drop(ops);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    // ── Happy-path tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn green_first_attempt_with_commits_ahead_opens_pr() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert_eq!(record.attempts, 1);
        assert!(record.pr_url.is_some(), "expected a PR URL");
        assert_eq!(record.base_sha, "sha-base");

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);

        // base_sha captured before branch creation
        let ops = runner.branch_ops.lock().unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].starts_with("create:"));
        drop(ops);

        // exactly one session opened and cleaned up, with a progress note
        let sessions_begun = runner.sessions_begun.lock().unwrap();
        assert_eq!(sessions_begun.len(), 1);
        drop(sessions_begun);

        let sessions_cleaned = runner.sessions_cleaned.lock().unwrap();
        assert_eq!(sessions_cleaned.len(), 1);
        drop(sessions_cleaned);

        let notes = runner.progress_notes.lock().unwrap();
        assert!(
            notes.iter().any(|(_, text)| text == "attempt 1 started"),
            "expected an 'attempt 1 started' progress note, got: {notes:?}"
        );
    }

    #[tokio::test]
    async fn green_with_pr_creation_failure_stays_green_and_writes_ledger() {
        // A create_pr failure (e.g. push rejected, gh auth issue) must NOT
        // abort the run before the ledger is written. The plan itself
        // succeeded, so the run stays Green with pr_url None and a reason
        // describing the PR failure — mirroring how a Telegram failure is
        // handled: a sink failure is never fatal to a green run.
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);
        runner.set_create_pr_should_fail(true);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run should not hard-fail on a PR creation error");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert!(record.pr_url.is_none());
        assert!(
            record
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("PR creation failed"),
            "reason should mention the PR failure, got: {:?}",
            record.reason
        );

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1, "ledger must still be written");
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1, "telegram must still be attempted");
    }

    #[tokio::test]
    async fn green_with_no_commits_ahead_gives_no_pr_and_reason() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(0);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert!(record.pr_url.is_none());
        assert_eq!(record.reason.as_deref(), Some("no changes to land"));

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn green_with_dirty_post_run_tree_gives_red_override_no_pr() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);
        runner.set_post_run_tree_dirty(true);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert!(record.pr_url.is_none());
        assert!(
            record.reason.as_deref().unwrap_or("").contains("dirty"),
            "reason should mention dirty tree"
        );

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn three_exit2_attempts_gives_orange_no_pr() {
        let mut runner = FakeRunner::new();
        runner.set_exit_codes([2, 2, 2]);
        runner.set_commits_ahead(1);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Orange);
        assert_eq!(record.attempts, 3);
        assert!(record.pr_url.is_none());

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn exit2_then_exit0_gives_green_attempts_2() {
        let mut runner = FakeRunner::new();
        runner.set_exit_codes([2, 0]);
        runner.set_commits_ahead(1);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert_eq!(record.attempts, 2);
        assert!(record.pr_url.is_some());

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn exit3_gives_red_attempts_1() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(3);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.attempts, 1);
        assert!(record.pr_url.is_none());

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn existing_branch_causes_switch_not_create() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);
        runner.set_branch_exists(true);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);

        let ops = runner.branch_ops.lock().unwrap();
        assert_eq!(ops.len(), 1);
        assert!(
            ops[0].starts_with("switch:"),
            "expected switch, got: {}",
            ops[0]
        );
        drop(ops);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn base_sha_captured_before_branch_creation() {
        // The FakeRunner always returns "sha-base" from head_sha.
        // After branch creation it still returns "sha-base" (no mutation),
        // but we verify the record's base_sha equals the pre-branch value.
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);
        runner.set_head_sha("sha-before-branch");

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.base_sha, "sha-before-branch");

        // Branch op must have happened after base_sha was captured — the
        // record proves base_sha == the pre-branch value.
        let ops = runner.branch_ops.lock().unwrap();
        assert_eq!(ops.len(), 1);
        assert!(ops[0].starts_with("create:"));
    }

    // ── Cerebrum session lifecycle tests ─────────────────────────────────

    #[tokio::test]
    async fn session_outlives_retries_across_attempts() {
        let mut runner = FakeRunner::new();
        runner.set_exit_codes([2, 0]);
        runner.set_commits_ahead(1);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.attempts, 2);

        // Exactly one session opened and cleaned up for the whole run,
        // spanning both attempts.
        let sessions_begun = runner.sessions_begun.lock().unwrap();
        assert_eq!(sessions_begun.len(), 1);
        drop(sessions_begun);

        let sessions_cleaned = runner.sessions_cleaned.lock().unwrap();
        assert_eq!(sessions_cleaned.len(), 1);
        drop(sessions_cleaned);

        let notes = runner.progress_notes.lock().unwrap();
        let session_ids: std::collections::HashSet<_> =
            notes.iter().map(|(s, _)| s.clone()).collect();
        assert_eq!(
            session_ids.len(),
            1,
            "both attempts must share one session id"
        );
        assert!(notes.iter().any(|(_, text)| text == "attempt 1 started"));
        assert!(notes.iter().any(|(_, text)| text == "attempt 2 started"));
    }

    #[tokio::test]
    async fn fetch_plan_failure_propagates_and_opens_no_branch() {
        let mut runner = FakeRunner::new();
        runner.set_fetch_plan_should_fail(true);

        let result = run(&runner, &test_cfg(3), test_inputs()).await;
        assert!(result.is_err(), "fetch_plan failure must propagate");

        let ops = runner.branch_ops.lock().unwrap();
        assert!(ops.is_empty(), "no branch should be created");
    }

    #[tokio::test]
    async fn cleanup_session_failure_is_best_effort() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);
        runner.set_cleanup_should_fail(true);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run should not hard-fail on a cleanup error");

        assert_eq!(record.failure_class, FailureClass::Green);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1, "ledger must still be written");
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1, "telegram must still be attempted");
    }

    #[tokio::test]
    async fn abort_path_opens_no_session() {
        let mut runner = FakeRunner::new();
        runner.set_tree_clean(false);

        let _record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        let sessions_begun = runner.sessions_begun.lock().unwrap();
        assert!(
            sessions_begun.is_empty(),
            "clean-start abort must not open a session"
        );
    }

    // ── Idempotent PR (produce/publish) tests ────────────────────────────

    #[tokio::test]
    async fn existing_open_pr_is_reused_and_create_pr_not_called() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);
        runner.set_existing_pr(Some("https://github.com/x/y/pull/7"));

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert_eq!(
            record.pr_url.as_deref(),
            Some("https://github.com/x/y/pull/7")
        );

        let create_pr_calls = *runner.create_pr_calls.lock().unwrap();
        assert_eq!(
            create_pr_calls, 0,
            "create_pr must not be called when an open PR already exists"
        );

        let push_calls = *runner.push_head_calls.lock().unwrap();
        assert_eq!(
            push_calls, 1,
            "push_head must still run before the find_pr lookup"
        );
    }

    #[tokio::test]
    async fn no_existing_pr_pushes_then_creates() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
        assert!(record.pr_url.is_some());

        let create_pr_calls = *runner.create_pr_calls.lock().unwrap();
        assert_eq!(create_pr_calls, 1);

        let push_calls = *runner.push_head_calls.lock().unwrap();
        assert_eq!(push_calls, 1, "push_head must run before create_pr");
    }
}
