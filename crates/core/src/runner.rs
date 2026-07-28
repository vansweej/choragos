//! [`CommandRunner`] trait — the I/O seam between the orchestrator and the
//! outside world.
//!
//! All side-effectful operations (git, gh, bun, Telegram, ledger) are
//! accessed exclusively through this trait so that the orchestrator can be
//! driven by a [`fake::FakeRunner`] in tests.

use std::future::Future;

/// Abstracts every external operation the orchestrator needs.
///
/// Each method is an `async fn` returning `Result<_, `[`crate::CoreError`]`>`
/// unless the return type is stated otherwise.  Implementations must be
/// `Send + Sync` so they can be used across await points in a multi-threaded
/// async runtime.
pub trait CommandRunner: Send + Sync {
    /// Reads the entire contents of the file at `path`.
    fn read_to_string(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Fetches `branch` from `remote`.
    fn git_fetch(
        &self,
        remote: &str,
        branch: &str,
    ) -> impl Future<Output = Result<(), crate::CoreError>> + Send;

    /// Returns the name of the currently checked-out branch.
    fn current_branch(
        &self,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Returns `true` when the working tree has no uncommitted changes.
    fn is_working_tree_clean(
        &self,
    ) -> impl Future<Output = Result<bool, crate::CoreError>> + Send;

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
    fn head_sha(
        &self,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

    /// Returns the number of commits on `HEAD` that are not reachable from
    /// `base_sha`.
    fn commits_ahead(
        &self,
        base_sha: &str,
    ) -> impl Future<Output = Result<u32, crate::CoreError>> + Send;

    /// Runs the ai-coding plan-cycle executor and returns its exit code.
    fn run_plan_cycle(
        &self,
        workspace: &str,
        plan_path: &str,
        profile: &str,
    ) -> impl Future<Output = Result<i32, crate::CoreError>> + Send;

    /// Creates a pull request and returns its URL.
    fn create_pr(
        &self,
        base: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<String, crate::CoreError>> + Send;

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

#[cfg(any(test, feature = "test-support"))]
pub mod fake;