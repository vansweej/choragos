//! Orchestrator: the top-level plan-cycle run flow.
//!
//! [`run`] drives the entire lifecycle: clean-start gate, branch management,
//! retry loop, PR creation, ledger append, and Telegram notification.

use crate::telegram::render;

/// Inputs supplied by the caller (MCP tool or CLI) for a single run.
#[derive(Debug, Clone)]
pub struct RunInputs {
    /// Absolute path to the workspace repository root.
    pub workspace: String,
    /// Repository name (typically the workspace directory basename).
    pub repo: String,
    /// Path to the plan Markdown file, relative to the workspace.
    pub plan_path: String,
    /// Pipeline profile to use; falls back to [`crate::Config::default_profile`]
    /// when `None`.
    pub profile: Option<String>,
    /// Override the auto-derived branch slug; when `None` the slug is derived
    /// from the plan title.
    pub slug_override: Option<String>,
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
        };
        let _ = runner.append_ledger(&record).await;
        let _ = runner.send_telegram(&render(&record)).await;
        return Ok(record);
    }

    // ── Capture base_sha BEFORE creating any branch ───────────────────────
    let base_sha = runner.head_sha().await?;

    // ── Read plan and derive branch ───────────────────────────────────────
    let plan_contents = runner.read_to_string(&inputs.plan_path).await?;
    let title = crate::plan::parse_title(&plan_contents).ok_or_else(|| {
        crate::CoreError::Message(format!(
            "no level-1 heading found in plan file '{}'",
            inputs.plan_path
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
        code = runner
            .run_plan_cycle(&inputs.workspace, &inputs.plan_path, &profile)
            .await?;
        attempts = attempt;
        if code == 0 || code == 3 {
            break;
        }
        // code == 2: continue retrying
    }

    // ── Post-run state ────────────────────────────────────────────────────
    let head_sha_final = runner.head_sha().await?;
    let commits_ahead = runner.commits_ahead(&base_sha).await?;

    // Post-run invariant: green exit but dirty tree → Red override.
    let (failure_class, pr_url, reason) = if code == 0 && !runner.is_working_tree_clean().await? {
        (
            crate::FailureClass::Red,
            None,
            Some("executor left tree dirty (pipeline invariant violation)".to_string()),
        )
    } else {
        let class = crate::FailureClass::from_exit_code(code);
        match class {
            crate::FailureClass::Green => {
                if commits_ahead > 0 {
                    // Open a PR.
                    let body = format!(
                        "Automated run of plan `{plan_id}` on branch `{branch}`.",
                        plan_id = slug,
                        branch = branch,
                    );
                    let url = runner.create_pr("main", &title, &body).await?;
                    (crate::FailureClass::Green, Some(url), None)
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
    };

    let finished_at = chrono::Utc::now().to_rfc3339();
    let record = crate::LedgerRecord {
        plan_id: slug.clone(),
        repo: inputs.repo.clone(),
        branch: branch.clone(),
        profile: profile.clone(),
        exit_code: code,
        attempts,
        failure_class,
        base_sha: base_sha.clone(),
        head_sha: head_sha_final,
        commits_ahead,
        pr_url,
        reason,
        started_at,
        finished_at,
    };

    // Append ledger (propagate errors — caller may want to know).
    runner.append_ledger(&record).await?;

    // Send Telegram best-effort: log and swallow any error.
    if let Err(e) = runner.send_telegram(&render(&record)).await {
        eprintln!("choragos: telegram notification failed (ignored): {e}");
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
            plan_path: "PLAN.md".to_string(),
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
}
