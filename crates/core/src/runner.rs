//! I/O seams between the orchestrator and the outside world.
//!
//! All side-effectful operations (git, gh, bun, cerebrum, Telegram, ledger)
//! are accessed exclusively through these traits so that the orchestrator can
//! be driven by a [`fake::FakeRunner`] in tests.
//!
//! The seam is split into four focused traits — [`Vcs`], [`Pipeline`],
//! [`Memory`], and [`Sink`] — unified by the [`CommandRunner`] marker
//! supertrait so that `orchestrator::run` can keep a single `R: CommandRunner`
//! bound while each concern stays independently documented and (in future
//! phases) independently mockable.

use std::future::Future;

/// Git and GitHub operations: branch management, PR creation.
pub trait Vcs: Send + Sync {
    /// Fetches `branch` from `remote`.
    fn git_fetch(
        &self,
        remote: &str,
        branch: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Returns the name of the currently checked-out branch.
    fn current_branch(&self) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Returns `true` when the working tree has no uncommitted changes.
    fn is_working_tree_clean(&self) -> impl Future<Output = Result<bool, crate::CoreError>> + Send;

    /// Returns `true` when the local `branch` tip matches its remote
    /// counterpart.
    fn local_matches_remote(
        &self,
        branch: &str,
    ) -> impl Future<Output = Result<bool, crate::CoreError>> + Send;

    /// Returns `true` when a local branch with `name` already exists.
    fn branch_exists(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<bool, crate::CoreError>> + Send;

    /// Returns `true` when `branch`'s tip contains `commit` (i.e. `commit`
    /// is an ancestor of, or equal to, `branch`). Used to distinguish a
    /// legitimate resume branch built atop the current trunk from a
    /// stale/divergent branch that would produce a misleading
    /// commits-ahead count.
    fn branch_contains(
        &self,
        branch: &str,
        commit: &str,
    ) -> impl Future<Output = Result<bool, crate::CoreError>> + Send;

    /// Creates a new local branch with `name` and switches to it.
    fn create_branch(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Switches to the existing local branch `name`.
    fn switch_branch(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Returns the SHA of `HEAD`.
    fn head_sha(&self) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Returns the number of commits on `HEAD` that are not reachable from
    /// `base_sha`.
    fn commits_ahead(
        &self,
        base_sha: &str,
    ) -> impl Future<Output = Result<u32, crate::CoreError>> + Send;

    /// Pushes the current branch (`HEAD`) to `origin`, creating the upstream
    /// ref. Idempotent: safe to call again on a resumed run.
    fn push_head(&self) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Looks up an existing open pull request for `branch`, returning its
    /// URL if one exists. Used to make PR creation idempotent: a resumed run
    /// should reuse an already-open PR rather than fail or duplicate it.
    fn find_pr(
        &self,
        branch: &str,
    ) -> impl Future<Output = Result<Option<String>, crate::CoreError>> + Send;

    /// Creates a pull request and returns its URL.
    ///
    /// Callers are responsible for pushing the branch first via
    /// [`push_head`](Vcs::push_head) — this method only invokes `gh pr
    /// create` and does not push.
    fn create_pr(
        &self,
        base: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;
}

/// Result of a plan-cycle executor invocation.
///
/// Carries the raw exit code plus placeholder fields for future
/// correlation data (populated by later subplans) such as the executor's
/// own run id or the path to its ledger output.
#[derive(Debug, Clone, Default)]
pub struct Rollup {
    /// Raw process exit code returned by the plan-cycle executor.
    pub exit_code: i32,
    /// The executor's own run id, if captured. Reserved for future use.
    pub run_id: Option<String>,
    /// Path to the executor's ledger output, if captured. Reserved for
    /// future use.
    pub ledger_path: Option<String>,
}

/// Execution of the ai-coding plan-cycle pipeline.
pub trait Pipeline: Send + Sync {
    /// Runs the ai-coding plan-cycle executor and returns a [`Rollup`]
    /// describing its outcome.
    fn run_plan_cycle(
        &self,
        workspace: &str,
        plan_ref: &str,
        profile: &str,
        session: &str,
    ) -> impl Future<Output = Result<Rollup, crate::CoreError>> + Send;
}

/// Cerebrum-backed plan storage and session-scoped progress notes.
pub trait Memory: Send + Sync {
    /// Fetches the plan body identified by `plan_ref` from cerebrum.
    fn fetch_plan(
        &self,
        plan_ref: &str,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Opens a cerebrum session scoped to `plan_ref` and returns an opaque
    /// session id. The session outlives individual plan-cycle attempts so
    /// that retries can recall progress notes from earlier attempts.
    fn begin_session(
        &self,
        plan_ref: &str,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Records a best-effort progress note under `session`.
    fn note_progress(
        &self,
        session: &str,
        text: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Cleans up `session`'s scoped memories (best-effort, scoped forget —
    /// never a global session clear).
    fn cleanup_session(
        &self,
        session: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;
}

/// Output sinks: Telegram notifications and the run-ledger.
pub trait Sink: Send + Sync {
    /// Sends a Telegram notification with `text`.
    fn send_telegram(
        &self,
        text: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Appends `record` to the run-ledger.
    fn append_ledger(
        &self,
        record: &crate::LedgerRecord,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;
}

/// Marker supertrait unifying [`Vcs`], [`Pipeline`], [`Memory`], and [`Sink`]
/// so that `orchestrator::run` can keep a single generic bound.
///
/// Blanket-implemented for any type that implements all four seams — no
/// manual `impl CommandRunner for T {}` is needed.
pub trait CommandRunner: Vcs + Pipeline + Memory + Sink {}

impl<T: Vcs + Pipeline + Memory + Sink> CommandRunner for T {}

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
