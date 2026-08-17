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

/// Derives a run-id from a session string of the form
/// `session:{plan_ref}:{nanos}` by taking the final `:`-delimited segment.
/// Falls back to a deterministic value derived from the whole session
/// string when the shape doesn't match (e.g. a test double's
/// `"session:{plan_ref}"` with no nanos segment). Never panics, never
/// returns an empty string.
pub fn run_id_from_session(session: &str) -> String {
    match session.rsplit(':').next() {
        Some(nanos) if !nanos.is_empty() && nanos != session => format!("run-{nanos}"),
        _ => format!("run-{session}"),
    }
}

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
    /// Trunk branch name (the clean-start gate branch and the PR base).
    /// Defaults to `"main"` via [`RunInputs::default_trunk`] — set this
    /// explicitly for repos whose default branch is `master`/`develop`/etc.
    /// (Phase 5's per-repo manifest entries will carry this per repo.)
    pub trunk: String,
    /// Correlates this run's [`crate::LedgerRecord`] with a Phase 5
    /// multi-repo batch. `None` for a standalone single-repo run;
    /// [`crate::change::run_multi`] sets this to the batch's `change_ref`
    /// for every repo it runs, so it lands in the ledger record itself (not
    /// just a post-hoc mutation of the returned value, which would miss the
    /// already-written ledger line).
    pub change_id: Option<String>,
    /// When `true`, runs the ai-coding S7 dry-run mode (token-free), passed
    /// through to `Pipeline::run_plan_cycle`.
    pub dry_run: bool,
}

impl RunInputs {
    /// The trunk branch name assumed when a caller doesn't set one.
    pub fn default_trunk() -> String {
        "main".to_string()
    }
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
    ledger_correlation_reason: Option<String>,
}

/// Runs plan fetch, branch management, the retry loop, and captures
/// post-run git state. Does not touch anything outside the local repo and
/// the plan-cycle executor — no push, no PR, no ledger, no Telegram.
///
/// `branch`, `slug`, and `title` are pre-derived by the caller (see
/// [`run`]) so that the branch-staleness gate can run before this function
/// (and before a cerebrum session is opened).
#[allow(clippy::too_many_arguments)]
async fn produce<R: crate::CommandRunner>(
    runner: &R,
    cfg: &crate::Config,
    inputs: &RunInputs,
    profile: &str,
    session: &str,
    base_sha: &str,
    branch: &str,
    slug: &str,
    title: &str,
    dry_run: bool,
) -> Result<ProduceOutcome, crate::CoreError> {
    if !dry_run {
        if runner.branch_exists(branch).await? {
            runner.switch_branch(branch).await?;
        } else {
            runner.create_branch(branch).await?;
        }
    }

    // ── Retry loop (or single dry-run pass) ─────────────────────────────
    let mut code = 0i32;
    let mut attempts = 0u32;
    let mut ledger_correlation_reason: Option<String> = None;

    if dry_run {
        let _ = runner.note_progress(session, "attempt 1 started").await;
        let rollup = runner
            .run_plan_cycle(&inputs.workspace, &inputs.plan_ref, profile, session, true)
            .await?;
        code = rollup.exit_code;
        attempts = 1;
    } else {
        for attempt in 1..=cfg.max_attempts {
            let _ = runner
                .note_progress(session, &format!("attempt {attempt} started"))
                .await;
            let rollup = runner
                .run_plan_cycle(
                    &inputs.workspace,
                    &inputs.plan_ref,
                    profile,
                    session,
                    inputs.dry_run,
                )
                .await?;
            code = rollup.exit_code;
            attempts = attempt;

            if code == 0 {
                let run_id_ok = rollup.run_id.as_deref().is_some_and(|s| !s.is_empty());
                let ledger_path_ok = rollup.ledger_path.is_some();
                let ledger_lines_ok = !rollup.ledger_lines.is_empty();
                if !(run_id_ok && ledger_path_ok && ledger_lines_ok) {
                    code = 2;
                    ledger_correlation_reason = Some(format!(
                        "diagnosis: missing or empty ledger correlation (run_id present: {run_id_ok}, \
                         ledger_path present: {ledger_path_ok}, ledger_lines non-empty: {ledger_lines_ok}) \
                         — a technically-successful exit must not be reported green without its own \
                         ledger being found and correlated"
                    ));
                }
            }

            if code == 0 || code == 3 {
                break;
            }
            // code == 2: continue retrying
        }
    }

    // ── Post-run state ────────────────────────────────────────────────────
    let head_sha = runner.head_sha().await?;
    let commits_ahead = runner.commits_ahead(base_sha).await?;

    Ok(ProduceOutcome {
        branch: branch.to_string(),
        slug: slug.to_string(),
        title: title.to_string(),
        base_sha: base_sha.to_string(),
        head_sha,
        commits_ahead,
        exit_code: code,
        attempts,
        ledger_correlation_reason,
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
    trunk: &str,
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

    // Post-run invariant: green exit but HEAD does not descend from
    // base_sha → Red override. This is a defense-in-depth catch-all,
    // independent of the pre-switch branch-staleness gate in `run`: it
    // guards against ANY future code path that could land on a
    // divergent/stale branch, not just the one this gate currently
    // prevents.
    let head_descends_from_base = runner
        .branch_contains("HEAD", &outcome.base_sha)
        .await
        .unwrap_or(false);

    if outcome.exit_code == 0 && !head_descends_from_base {
        return (
            crate::FailureClass::Red,
            None,
            Some(format!(
                "post-run invariant: HEAD {head} does not descend from base {base} — \
                 adopted a divergent/stale branch",
                head = outcome.head_sha,
                base = outcome.base_sha,
            )),
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
                        Ok(None) => match runner.create_pr(trunk, &outcome.title, &body).await {
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
/// 1. **Clean-start gate** — fetches `origin/<trunk>`, then verifies that the
///    current branch is `<trunk>`, the working tree is clean, and the local
///    `<trunk>` matches the remote. `<trunk>` defaults to `"main"`
///    ([`RunInputs::trunk`]). Any failure returns a Red [`crate::LedgerRecord`]
///    immediately without creating a branch.
/// 2. **Branch-staleness gate** — captures `base_sha`, reads the plan, and
///    derives the slug and branch name. If a branch matching that name
///    already exists locally but does not contain `base_sha` (i.e. it is
///    not a legitimate resume target built atop the current trunk), returns
///    a Red [`crate::LedgerRecord`] with an explanatory reason, without
///    touching the branch or opening a cerebrum session. This prevents a
///    stale leftover branch from silently producing a false-green
///    "no changes to land" result.
/// 3. **Branch management** — creates or switches to the feature branch.
/// 4. **Retry loop** — calls `run_plan_cycle` up to `cfg.max_attempts` times,
///    stopping early on exit codes `0` (green) or `3` (red).
/// 5. **Post-run invariants** — if the executor exited `0` but left the tree
///    dirty, or if `HEAD` does not descend from `base_sha`, the result is
///    overridden to Red.
/// 6. **PR decision** — opens a PR only on a green run with commits ahead of
///    `base_sha`.
/// 7. **Finalise** — appends the ledger record and sends a Telegram
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
    let trunk = inputs.trunk.as_str();
    runner.git_fetch("origin", trunk).await?;

    let abort_reason: Option<String> = {
        let branch = runner.current_branch().await?;
        if branch != trunk {
            Some(format!("current branch is '{branch}', not '{trunk}'"))
        } else if !runner.is_working_tree_clean().await? {
            Some("working tree is not clean".to_string())
        } else if !runner.local_matches_remote(trunk).await? {
            Some(format!("local {trunk} is behind remote"))
        } else {
            None
        }
    };

    if let Some(reason) = abort_reason {
        let finished_at = chrono::Utc::now().to_rfc3339();
        let abort_nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let record = crate::LedgerRecord {
            run_id: format!("run-{abort_nanos}"),
            plan_id: String::new(),
            repo: inputs.repo.clone(),
            branch: trunk.to_string(),
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
            change_id: inputs.change_id.clone(),
        };
        let _ = runner.append_ledger(&record).await;
        let _ = runner.send_telegram(&render(&record)).await;
        return Ok(record);
    }

    // ── Capture base_sha BEFORE creating any branch ───────────────────────
    let base_sha = runner.head_sha().await?;

    // ── Read plan and derive branch (before opening a session or touching
    // any branch, so the staleness gate below can abort cleanly) ─────────
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

    // ── Branch-staleness gate ──────────────────────────────────────────────
    //
    // A pre-existing branch matching the derived slug is only a legitimate
    // resume target when it contains base_sha (i.e. it was built atop the
    // current trunk). Otherwise it's a stale leftover — commonly from an
    // earlier, unrelated run — and blindly adopting it would make
    // `commits_ahead(base_sha)` silently return 0, producing a false-green
    // "no changes to land" result instead of ever running the plan.
    if runner.branch_exists(&branch).await? && !runner.branch_contains(&branch, &base_sha).await? {
        let finished_at = chrono::Utc::now().to_rfc3339();
        let stale_nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let record = crate::LedgerRecord {
            run_id: format!("run-{stale_nanos}"),
            plan_id: slug.clone(),
            repo: inputs.repo.clone(),
            branch: branch.clone(),
            profile: profile.clone(),
            exit_code: -1,
            attempts: 0,
            failure_class: crate::FailureClass::Red,
            base_sha: base_sha.clone(),
            head_sha: String::new(),
            commits_ahead: 0,
            pr_url: None,
            reason: Some(format!(
                "existing branch '{branch}' is stale: it does not contain the current \
                 {trunk} tip {base_sha}; delete the branch or pass --slug/--change-ref"
            )),
            started_at,
            finished_at,
            schema_version: crate::ledger::CURRENT_SCHEMA_VERSION,
            change_id: inputs.change_id.clone(),
        };
        let _ = runner.append_ledger(&record).await;
        let _ = runner.send_telegram(&render(&record)).await;
        return Ok(record);
    }

    // ── Open a cerebrum session for this run ──────────────────────────────
    //
    // The session outlives individual plan-cycle attempts so that a retry
    // can recall progress notes left behind by an earlier attempt.
    let session = runner.begin_session(&inputs.plan_ref).await?;
    let run_id = run_id_from_session(&session);

    // ── Produce: branch mgmt, retry loop, post-run git state ──────────────
    let outcome = produce(
        runner,
        cfg,
        &inputs,
        &profile,
        &session,
        &base_sha,
        &branch,
        &slug,
        &title,
        inputs.dry_run,
    )
    .await?;

    let finished_at = chrono::Utc::now().to_rfc3339();

    let record = if inputs.dry_run {
        // ── Dry-run: skip publish (push/PR) entirely ──────────────────────
        let failure_class = crate::FailureClass::from_exit_code(outcome.exit_code);
        crate::LedgerRecord {
            run_id: run_id.clone(),
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
            pr_url: None,
            reason: Some("dry run: no branch/push/PR/ledger side effects performed".to_string()),
            started_at,
            finished_at,
            schema_version: crate::ledger::CURRENT_SCHEMA_VERSION,
            change_id: inputs.change_id.clone(),
        }
    } else {
        // ── Publish: failure-class + idempotent find-or-create PR decision ───
        let (failure_class, pr_url, mut reason) = publish(runner, &outcome, trunk).await;
        if reason.is_none() {
            reason = outcome.ledger_correlation_reason.clone();
        } else if let Some(extra) = outcome.ledger_correlation_reason.clone() {
            reason = Some(format!("{} ({extra})", reason.unwrap()));
        }

        let record = crate::LedgerRecord {
            run_id: run_id.clone(),
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
            change_id: inputs.change_id.clone(),
        };

        // Append ledger (propagate errors — caller may want to know).
        runner.append_ledger(&record).await?;

        // Send Telegram best-effort: log and swallow any error.
        if let Err(e) = runner.send_telegram(&render(&record)).await {
            eprintln!("choragos: telegram notification failed (ignored): {e}");
        }

        record
    };

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
    use super::{run, run_id_from_session, RunInputs};

    #[test]
    fn run_id_from_session_well_formed_is_nonempty_and_deterministic() {
        let session = "session:plan-abc:123456789";
        let a = run_id_from_session(session);
        let b = run_id_from_session(session);
        assert!(!a.is_empty());
        assert_eq!(a, b);
        assert_eq!(a, "run-123456789");
    }

    #[test]
    fn run_id_from_session_malformed_does_not_panic_and_is_nonempty() {
        let session = "session:plan-abc";
        let id = run_id_from_session(session);
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn missing_ledger_correlation_prevents_green_and_names_diagnosis() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);
        runner.set_include_ledger_correlation(false);
        // Force red-hard-failure-avoidance: without correlation, exit 0
        // becomes 2 (retry-eligible), which after max_attempts exhausts to
        // Orange, never Green.
        let record = run(&runner, &test_cfg(1), test_inputs())
            .await
            .expect("run");

        assert_ne!(record.failure_class, FailureClass::Green);
        assert!(
            record.reason.as_deref().unwrap_or("").contains("diagnosis"),
            "reason must mention 'diagnosis', got: {:?}",
            record.reason
        );
    }

    #[tokio::test]
    async fn present_ledger_correlation_allows_green() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);
        runner.set_include_ledger_correlation(true);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Green);
    }

    #[tokio::test]
    async fn ledger_record_run_id_is_nonempty_and_session_derived() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert!(!record.run_id.is_empty());
        assert!(record.run_id.starts_with("run-"));
    }
    use crate::runner::fake::FakeRunner;
    use crate::{Config, FailureClass};

    fn test_cfg(max_attempts: u32) -> Config {
        Config {
            ai_coding_monorepo: "/ai".to_string(),
            default_profile: "default".to_string(),
            max_attempts,
            telegram_bot_token: None,
            telegram_chat_id: None,
            cerebrum_bin: "/nix/store/xyz-cerebrum/bin/cerebrum".to_string(),
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
        assert_eq!(tg.len(), 1);
    }

    #[tokio::test]
    async fn green_exit_but_head_not_descended_from_base_is_red_override() {
        // Defense-in-depth: even when the pre-switch staleness gate is
        // bypassed (branch_exists is false here, so `produce` creates a
        // fresh branch normally), publish() must still refuse Green if
        // HEAD doesn't descend from base_sha.
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(2);
        runner.set_branch_exists(false);
        runner.set_branch_contains(false);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert!(record.pr_url.is_none());
        assert!(
            record
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("does not descend"),
            "reason should mention HEAD not descending from base, got: {:?}",
            record.reason
        );

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);
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
    async fn custom_trunk_mismatch_yields_red_and_no_branch() {
        // FakeRunner::new() defaults to reporting current_branch() == "main".
        // With a non-default trunk ("develop"), the gate must compare
        // against THAT trunk, not a hardcoded "main" — so this must abort
        // even though "main" would have passed the old hardcoded check.
        let runner = FakeRunner::new();
        let mut inputs = test_inputs();
        inputs.trunk = "develop".to_string();

        let record = run(&runner, &test_cfg(3), inputs).await.expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.branch, "develop");
        assert!(
            record
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("not 'develop'"),
            "reason should reference the configured trunk, got: {:?}",
            record.reason
        );
    }

    #[tokio::test]
    async fn change_id_reaches_the_appended_ledger_record_not_just_the_return_value() {
        // Regression test for a real bug found via choragos's Phase 5 Gate 2
        // vertical slice: run_multi used to mutate ONLY the record it
        // returned to the caller, AFTER orchestrator::run had already
        // called append_ledger with change_id: None baked in — so the
        // in-memory/printed record looked correct but the persisted ledger
        // line was always missing change_id. Assert against the runner's
        // OWN captured ledger record, not just run()'s return value, so
        // this class of bug can't silently reappear.
        let mut runner = FakeRunner::new();
        runner.push_exit_code(0);
        runner.set_commits_ahead(1);
        let mut inputs = test_inputs();
        inputs.change_id = Some("change-abc".to_string());

        let returned = run(&runner, &test_cfg(3), inputs).await.expect("run");
        assert_eq!(returned.change_id.as_deref(), Some("change-abc"));

        let appended = runner.appended_records.lock().unwrap();
        assert_eq!(appended.len(), 1);
        assert_eq!(
            appended[0].change_id.as_deref(),
            Some("change-abc"),
            "the record actually passed to append_ledger must carry change_id, \
             not just the value returned to the caller"
        );
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
        runner.set_branch_contains(true);

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
    async fn stale_existing_branch_yields_red_no_switch_no_session() {
        // Regression test for the false-green bug: an existing branch
        // matching the derived slug that does NOT contain base_sha must
        // abort Red with an explanatory reason, without ever switching to
        // it or opening a cerebrum session — mirroring the clean-start
        // gate's abort semantics.
        let mut runner = FakeRunner::new();
        runner.set_branch_exists(true);
        runner.set_branch_contains(false);
        runner.push_exit_code(0);
        runner.set_commits_ahead(0);

        let record = run(&runner, &test_cfg(3), test_inputs())
            .await
            .expect("run");

        assert_eq!(record.failure_class, FailureClass::Red);
        assert_eq!(record.exit_code, -1);
        assert_eq!(record.attempts, 0);
        assert!(
            record.reason.as_deref().unwrap_or("").contains("stale"),
            "reason should mention the branch is stale, got: {:?}",
            record.reason
        );

        let ops = runner.branch_ops.lock().unwrap();
        assert!(
            ops.is_empty(),
            "no branch switch/create should happen on a stale-branch abort"
        );
        drop(ops);

        let ledger = runner.appended_records.lock().unwrap();
        assert_eq!(ledger.len(), 1);
        drop(ledger);

        let tg = runner.sent_telegrams.lock().unwrap();
        assert_eq!(tg.len(), 1);

        let sessions_begun = runner.sessions_begun.lock().unwrap();
        assert!(
            sessions_begun.is_empty(),
            "stale-branch abort must not open a cerebrum session"
        );
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
