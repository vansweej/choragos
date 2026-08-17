//! In-process [`FakeRunner`] for use in unit and integration tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::{CoreError, LedgerRecord, Memory, Pipeline, Sink, Vcs};

/// A fully scripted, in-memory [`CommandRunner`] implementation.
///
/// All mutable state is guarded by a [`Mutex`] so the runner can be shared
/// across `async` tasks.  Builder-style setters return `&mut Self` for
/// convenient chaining.
#[derive(Debug, Default)]
pub struct FakeRunner {
    /// Exit codes to return from successive [`run_plan_cycle`] calls (FIFO).
    ///
    /// [`run_plan_cycle`]: CommandRunner::run_plan_cycle
    exit_codes: Mutex<VecDeque<i32>>,
    /// Contents returned by [`fetch_plan`].
    ///
    /// [`fetch_plan`]: CommandRunner::fetch_plan
    plan_contents: Mutex<String>,
    /// Whether [`current_branch`] reports `"main"`.
    ///
    /// [`current_branch`]: CommandRunner::current_branch
    is_on_main: Mutex<bool>,
    /// Whether [`is_working_tree_clean`] returns `true`.
    ///
    /// [`is_working_tree_clean`]: CommandRunner::is_working_tree_clean
    tree_clean: Mutex<bool>,
    /// Whether [`local_matches_remote`] returns `true`.
    ///
    /// [`local_matches_remote`]: CommandRunner::local_matches_remote
    local_matches_remote_flag: Mutex<bool>,
    /// Whether [`branch_exists`] returns `true`.
    ///
    /// [`branch_exists`]: CommandRunner::branch_exists
    branch_exists_flag: Mutex<bool>,
    /// Whether [`branch_contains`] returns `true`. Defaults to `true` so
    /// existing resume-branch tests keep switching rather than aborting.
    ///
    /// [`branch_contains`]: CommandRunner::branch_contains
    branch_contains_flag: Mutex<bool>,
    /// Value returned by [`head_sha`].
    ///
    /// [`head_sha`]: CommandRunner::head_sha
    scripted_head_sha: Mutex<String>,
    /// Value returned by [`commits_ahead`].
    ///
    /// [`commits_ahead`]: CommandRunner::commits_ahead
    scripted_commits_ahead: Mutex<u32>,
    /// Telegram messages recorded by [`send_telegram`].
    ///
    /// [`send_telegram`]: CommandRunner::send_telegram
    pub sent_telegrams: Mutex<Vec<String>>,
    /// Ledger records recorded by [`append_ledger`].
    ///
    /// [`append_ledger`]: CommandRunner::append_ledger
    pub appended_records: Mutex<Vec<LedgerRecord>>,
    /// Branch names passed to [`create_branch`] or [`switch_branch`] (in
    /// call order).
    ///
    /// [`create_branch`]: CommandRunner::create_branch
    /// [`switch_branch`]: CommandRunner::switch_branch
    pub branch_ops: Mutex<Vec<String>>,
    /// Whether the post-run tree should appear dirty (used to test the
    /// pipeline-invariant-violation path).
    post_run_tree_dirty: Mutex<bool>,
    /// Call count for `is_working_tree_clean`, used to flip to dirty after
    /// the first call.
    clean_call_count: Mutex<u32>,
    /// When `true`, [`create_pr`] returns an error instead of a fixed URL
    /// (used to test the graceful-degradation-on-PR-failure path).
    ///
    /// [`create_pr`]: CommandRunner::create_pr
    create_pr_should_fail: Mutex<bool>,
    /// When `true`, [`fetch_plan`] returns an error.
    ///
    /// [`fetch_plan`]: CommandRunner::fetch_plan
    fetch_plan_should_fail: Mutex<bool>,
    /// When `true`, [`begin_session`] returns an error.
    ///
    /// [`begin_session`]: CommandRunner::begin_session
    begin_session_should_fail: Mutex<bool>,
    /// When `true`, [`cleanup_session`] returns an error.
    ///
    /// [`cleanup_session`]: CommandRunner::cleanup_session
    cleanup_should_fail: Mutex<bool>,
    /// Plan refs passed to [`begin_session`] (in call order).
    ///
    /// [`begin_session`]: CommandRunner::begin_session
    pub sessions_begun: Mutex<Vec<String>>,
    /// `(session, text)` pairs recorded by [`note_progress`].
    ///
    /// [`note_progress`]: CommandRunner::note_progress
    pub progress_notes: Mutex<Vec<(String, String)>>,
    /// Session ids passed to [`cleanup_session`] (in call order).
    ///
    /// [`cleanup_session`]: CommandRunner::cleanup_session
    pub sessions_cleaned: Mutex<Vec<String>>,
    /// Value returned by [`find_pr`] (`None` unless scripted via
    /// [`set_existing_pr`]).
    ///
    /// [`find_pr`]: Vcs::find_pr
    /// [`set_existing_pr`]: FakeRunner::set_existing_pr
    existing_pr: Mutex<Option<String>>,
    /// Number of times [`push_head`] was called.
    ///
    /// [`push_head`]: Vcs::push_head
    pub push_head_calls: Mutex<u32>,
    /// Number of times [`create_pr`] was called.
    ///
    /// [`create_pr`]: Vcs::create_pr
    pub create_pr_calls: Mutex<u32>,
    /// When `true` (the default), [`run_plan_cycle`] returns a `Rollup`
    /// with a populated `run_id`, `ledger_path`, and non-empty
    /// `ledger_lines`, simulating a properly-correlated green run. Set to
    /// `false` to simulate a missing/uncorrelated ledger.
    ///
    /// [`run_plan_cycle`]: Pipeline::run_plan_cycle
    pub include_ledger_correlation: Mutex<bool>,
    /// The `dry_run` argument passed to each [`run_plan_cycle`] call, in
    /// call order.
    ///
    /// [`run_plan_cycle`]: Pipeline::run_plan_cycle
    pub run_plan_cycle_dry_run_flags: Mutex<Vec<bool>>,
}

impl FakeRunner {
    /// Creates a new [`FakeRunner`] with sensible defaults:
    ///
    /// - on `main`, tree clean, local matches remote, branch does not exist
    /// - `head_sha` = `"sha-base"`, `commits_ahead` = `1`
    /// - no scripted exit codes (caller must push at least one)
    pub fn new() -> Self {
        Self {
            is_on_main: Mutex::new(true),
            tree_clean: Mutex::new(true),
            local_matches_remote_flag: Mutex::new(true),
            branch_exists_flag: Mutex::new(false),
            branch_contains_flag: Mutex::new(true),
            scripted_head_sha: Mutex::new("sha-base".to_string()),
            scripted_commits_ahead: Mutex::new(1),
            plan_contents: Mutex::new("# Feature: test plan\n\nsome body".to_string()),
            include_ledger_correlation: Mutex::new(true),
            ..Default::default()
        }
    }

    /// Controls whether [`run_plan_cycle`] returns a correlated ledger
    /// (see [`include_ledger_correlation`]).
    ///
    /// [`run_plan_cycle`]: crate::Pipeline::run_plan_cycle
    /// [`include_ledger_correlation`]: FakeRunner::include_ledger_correlation
    pub fn set_include_ledger_correlation(&mut self, value: bool) -> &mut Self {
        *self.include_ledger_correlation.lock().unwrap() = value;
        self
    }

    // ── builder-style setters ────────────────────────────────────────────

    /// Enqueues an exit code to be returned by the next [`run_plan_cycle`]
    /// call.
    ///
    /// [`run_plan_cycle`]: CommandRunner::run_plan_cycle
    pub fn push_exit_code(&mut self, code: i32) -> &mut Self {
        self.exit_codes.lock().unwrap().push_back(code);
        self
    }

    /// Replaces all queued exit codes.
    pub fn set_exit_codes(&mut self, codes: impl IntoIterator<Item = i32>) -> &mut Self {
        {
            let mut q = self.exit_codes.lock().unwrap();
            q.clear();
            q.extend(codes);
        }
        self
    }

    /// Sets the content returned by [`fetch_plan`].
    ///
    /// [`fetch_plan`]: CommandRunner::fetch_plan
    pub fn set_fetched_plan(&mut self, contents: impl Into<String>) -> &mut Self {
        *self.plan_contents.lock().unwrap() = contents.into();
        self
    }

    /// Controls whether [`current_branch`] reports `"main"`.
    ///
    /// [`current_branch`]: CommandRunner::current_branch
    pub fn set_on_main(&mut self, value: bool) -> &mut Self {
        *self.is_on_main.lock().unwrap() = value;
        self
    }

    /// Controls whether [`is_working_tree_clean`] returns `true`.
    ///
    /// [`is_working_tree_clean`]: CommandRunner::is_working_tree_clean
    pub fn set_tree_clean(&mut self, value: bool) -> &mut Self {
        *self.tree_clean.lock().unwrap() = value;
        self
    }

    /// Controls whether [`local_matches_remote`] returns `true`.
    ///
    /// [`local_matches_remote`]: CommandRunner::local_matches_remote
    pub fn set_local_matches_remote(&mut self, value: bool) -> &mut Self {
        *self.local_matches_remote_flag.lock().unwrap() = value;
        self
    }

    /// Controls whether [`branch_exists`] returns `true`.
    ///
    /// [`branch_exists`]: CommandRunner::branch_exists
    pub fn set_branch_exists(&mut self, value: bool) -> &mut Self {
        *self.branch_exists_flag.lock().unwrap() = value;
        self
    }

    /// Controls whether [`branch_contains`] returns `true`.
    ///
    /// [`branch_contains`]: CommandRunner::branch_contains
    pub fn set_branch_contains(&mut self, value: bool) -> &mut Self {
        *self.branch_contains_flag.lock().unwrap() = value;
        self
    }

    /// Sets the value returned by [`head_sha`].
    ///
    /// [`head_sha`]: CommandRunner::head_sha
    pub fn set_head_sha(&mut self, sha: impl Into<String>) -> &mut Self {
        *self.scripted_head_sha.lock().unwrap() = sha.into();
        self
    }

    /// Sets the value returned by [`commits_ahead`].
    ///
    /// [`commits_ahead`]: CommandRunner::commits_ahead
    pub fn set_commits_ahead(&mut self, n: u32) -> &mut Self {
        *self.scripted_commits_ahead.lock().unwrap() = n;
        self
    }

    /// When set to `true`, the tree will appear dirty on the *second* call to
    /// [`is_working_tree_clean`] (simulating a dirty post-run tree).
    ///
    /// [`is_working_tree_clean`]: CommandRunner::is_working_tree_clean
    pub fn set_post_run_tree_dirty(&mut self, value: bool) -> &mut Self {
        *self.post_run_tree_dirty.lock().unwrap() = value;
        self
    }

    /// When set to `true`, [`create_pr`] returns an error instead of a fixed
    /// URL.
    ///
    /// [`create_pr`]: CommandRunner::create_pr
    pub fn set_create_pr_should_fail(&mut self, value: bool) -> &mut Self {
        *self.create_pr_should_fail.lock().unwrap() = value;
        self
    }

    /// When set to `true`, [`fetch_plan`] returns an error.
    ///
    /// [`fetch_plan`]: CommandRunner::fetch_plan
    pub fn set_fetch_plan_should_fail(&mut self, value: bool) -> &mut Self {
        *self.fetch_plan_should_fail.lock().unwrap() = value;
        self
    }

    /// When set to `true`, [`begin_session`] returns an error.
    ///
    /// [`begin_session`]: CommandRunner::begin_session
    pub fn set_begin_session_should_fail(&mut self, value: bool) -> &mut Self {
        *self.begin_session_should_fail.lock().unwrap() = value;
        self
    }

    /// When set to `true`, [`cleanup_session`] returns an error.
    ///
    /// [`cleanup_session`]: CommandRunner::cleanup_session
    pub fn set_cleanup_should_fail(&mut self, value: bool) -> &mut Self {
        *self.cleanup_should_fail.lock().unwrap() = value;
        self
    }

    /// Scripts [`find_pr`] to return `Some(url)` for an already-open PR, or
    /// `None` (the default) when no PR exists yet.
    ///
    /// [`find_pr`]: Vcs::find_pr
    pub fn set_existing_pr(&mut self, url: Option<impl Into<String>>) -> &mut Self {
        *self.existing_pr.lock().unwrap() = url.map(Into::into);
        self
    }
}

impl Memory for FakeRunner {
    async fn fetch_plan(&self, _plan_ref: &str) -> Result<String, CoreError> {
        if *self.fetch_plan_should_fail.lock().unwrap() {
            return Err(CoreError::Message(
                "fetch_plan failed (scripted)".to_string(),
            ));
        }
        Ok(self.plan_contents.lock().unwrap().clone())
    }

    async fn begin_session(&self, plan_ref: &str) -> Result<String, CoreError> {
        if *self.begin_session_should_fail.lock().unwrap() {
            return Err(CoreError::Message(
                "begin_session failed (scripted)".to_string(),
            ));
        }
        self.sessions_begun
            .lock()
            .unwrap()
            .push(plan_ref.to_string());
        Ok(format!("session:{plan_ref}"))
    }

    async fn note_progress(&self, session: &str, text: &str) -> Result<(), CoreError> {
        self.progress_notes
            .lock()
            .unwrap()
            .push((session.to_string(), text.to_string()));
        Ok(())
    }

    async fn cleanup_session(&self, session: &str) -> Result<(), CoreError> {
        self.sessions_cleaned
            .lock()
            .unwrap()
            .push(session.to_string());
        if *self.cleanup_should_fail.lock().unwrap() {
            return Err(CoreError::Message(
                "cleanup_session failed (scripted)".to_string(),
            ));
        }
        Ok(())
    }
}

impl Vcs for FakeRunner {
    async fn git_fetch(&self, _remote: &str, _branch: &str) -> Result<(), CoreError> {
        Ok(())
    }

    async fn current_branch(&self) -> Result<String, CoreError> {
        let on_main = *self.is_on_main.lock().unwrap();
        if on_main {
            Ok("main".to_string())
        } else {
            Ok("other-branch".to_string())
        }
    }

    async fn is_working_tree_clean(&self) -> Result<bool, CoreError> {
        let mut count = self.clean_call_count.lock().unwrap();
        *count += 1;
        let call = *count;
        drop(count);

        let post_dirty = *self.post_run_tree_dirty.lock().unwrap();
        // On the second call (post-run check) return dirty when requested.
        if post_dirty && call >= 2 {
            return Ok(false);
        }
        Ok(*self.tree_clean.lock().unwrap())
    }

    async fn local_matches_remote(&self, _branch: &str) -> Result<bool, CoreError> {
        Ok(*self.local_matches_remote_flag.lock().unwrap())
    }

    async fn branch_exists(&self, _name: &str) -> Result<bool, CoreError> {
        Ok(*self.branch_exists_flag.lock().unwrap())
    }

    async fn branch_contains(&self, _branch: &str, _commit: &str) -> Result<bool, CoreError> {
        Ok(*self.branch_contains_flag.lock().unwrap())
    }

    async fn create_branch(&self, name: &str) -> Result<(), CoreError> {
        self.branch_ops
            .lock()
            .unwrap()
            .push(format!("create:{name}"));
        Ok(())
    }

    async fn switch_branch(&self, name: &str) -> Result<(), CoreError> {
        self.branch_ops
            .lock()
            .unwrap()
            .push(format!("switch:{name}"));
        Ok(())
    }

    async fn head_sha(&self) -> Result<String, CoreError> {
        Ok(self.scripted_head_sha.lock().unwrap().clone())
    }

    async fn commits_ahead(&self, _base_sha: &str) -> Result<u32, CoreError> {
        Ok(*self.scripted_commits_ahead.lock().unwrap())
    }

    async fn create_pr(&self, _base: &str, _title: &str, _body: &str) -> Result<String, CoreError> {
        *self.create_pr_calls.lock().unwrap() += 1;
        if *self.create_pr_should_fail.lock().unwrap() {
            return Err(CoreError::Command {
                context: "gh pr create".to_string(),
                message: "aborted: you must first push the current branch to a remote".to_string(),
            });
        }
        Ok("https://github.com/x/y/pull/1".to_string())
    }

    async fn push_head(&self) -> Result<(), CoreError> {
        *self.push_head_calls.lock().unwrap() += 1;
        Ok(())
    }

    async fn find_pr(&self, _branch: &str) -> Result<Option<String>, CoreError> {
        Ok(self.existing_pr.lock().unwrap().clone())
    }
}

impl Pipeline for FakeRunner {
    async fn run_plan_cycle(
        &self,
        _workspace: &str,
        _plan_ref: &str,
        _profile: &str,
        _session: &str,
        dry_run: bool,
    ) -> Result<crate::runner::Rollup, CoreError> {
        self.run_plan_cycle_dry_run_flags
            .lock()
            .unwrap()
            .push(dry_run);
        let code = self.exit_codes.lock().unwrap().pop_front().unwrap_or(0);
        let (run_id, ledger_path, ledger_lines) =
            if *self.include_ledger_correlation.lock().unwrap() {
                (
                    Some("run-fake-1".to_string()),
                    Some("/tmp/fake-ledger.jsonl".to_string()),
                    vec![crate::LedgerLine {
                        schema_version: 1,
                        run_id: "run-fake-1".to_string(),
                        ts: "2024-01-01T00:00:00Z".to_string(),
                        kind: "run_finished".to_string(),
                        phase: None,
                        step: None,
                        op_id: None,
                        payload: serde_json::Value::Null,
                    }],
                )
            } else {
                (None, None, Vec::new())
            };
        Ok(crate::runner::Rollup {
            exit_code: code,
            run_id,
            ledger_path,
            ledger_lines,
        })
    }
}

impl Sink for FakeRunner {
    async fn send_telegram(&self, text: &str) -> Result<(), CoreError> {
        self.sent_telegrams.lock().unwrap().push(text.to_string());
        Ok(())
    }

    async fn append_ledger(&self, record: &LedgerRecord) -> Result<(), CoreError> {
        self.appended_records.lock().unwrap().push(record.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::FakeRunner;
    use crate::Pipeline;

    #[tokio::test]
    async fn scripted_exit_code_is_returned() {
        let mut runner = FakeRunner::new();
        runner.push_exit_code(2);
        let rollup = runner
            .run_plan_cycle("workspace", "plan-ref", "default", "session-1", false)
            .await
            .expect("run_plan_cycle");
        assert_eq!(rollup.exit_code, 2);
    }

    #[tokio::test]
    async fn default_exit_code_is_zero_when_queue_empty() {
        let runner = FakeRunner::new();
        let rollup = runner
            .run_plan_cycle("workspace", "plan-ref", "default", "session-1", false)
            .await
            .expect("run_plan_cycle");
        assert_eq!(rollup.exit_code, 0);
    }
}
