//! Phase 5: multi-repo change fan-out.
//!
//! A `change:<id>` manifest (fetched from cerebrum under the `plan:<id>`
//! scope kind — see [`crate::CerebrumClient::fetch_change`]) lists an
//! ordered set of per-repo plan runs to execute sequentially. [`run_multi`]
//! drives each repo through the existing single-repo [`crate::orchestrator::run`]
//! flow unchanged, and rolls the per-repo [`crate::LedgerRecord`]s up into
//! one ordered batch, stopping early (ordered-stop-on-failure) the first
//! time a `required` repo goes non-Green.

use serde::Deserialize;

fn default_required() -> bool {
    true
}

/// One repo's worth of work within a change manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoJob {
    /// Absolute path to the repo's workspace root.
    pub workspace: String,
    /// Reference to this repo's plan in cerebrum (a `plan:<id>` scope id),
    /// resolved exactly as a single-repo run resolves `RunInputs::plan_ref`.
    pub plan_ref: String,
    /// Trunk branch name override for this repo. Falls back to
    /// [`crate::orchestrator::RunInputs::default_trunk`] (`"main"`) when
    /// omitted.
    #[serde(default)]
    pub trunk: Option<String>,
    /// When `true` (the default), this repo going non-Green stops the rest
    /// of the batch (ordered-stop-on-failure). When `false`, a failure here
    /// is recorded but the batch continues to the next repo.
    #[serde(default = "default_required")]
    pub required: bool,
    /// Pipeline profile override for this repo. Falls back to the batch
    /// caller's default (typically [`crate::Config::default_profile`]).
    #[serde(default)]
    pub profile: Option<String>,
    /// Branch-slug override for this repo. Falls back to deriving the slug
    /// from the plan's title, exactly as a single-repo run does.
    #[serde(default)]
    pub slug_override: Option<String>,
}

/// The parsed body of a `change:<id>` manifest: an ordered list of
/// per-repo jobs.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangeManifest {
    /// Repos to run, in the order the planner determined (choragos does
    /// not compute or validate a dependency DAG — it only honours list
    /// order, per the multi-repo-ordering decision).
    pub repos: Vec<RepoJob>,
}

/// Derives a repo name from a workspace path's final path component,
/// falling back to `"unknown"` (mirrors the CLI/MCP single-repo derivation).
fn repo_name_from_workspace(workspace: &str) -> String {
    std::path::Path::new(workspace)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Builds a synthetic Red [`crate::LedgerRecord`] for a repo whose run
/// returned a hard [`crate::CoreError`] (e.g. a plan fetch failure) rather
/// than completing to a normal [`crate::orchestrator::run`] outcome. Ensures
/// `run_multi` never hard-aborts the whole batch on one repo's I/O error —
/// the ledger is the primitive, written on every path, including this one.
fn error_record(
    job: &RepoJob,
    profile: &str,
    started_at: &str,
    change_id: Option<&str>,
    error: &crate::CoreError,
) -> crate::LedgerRecord {
    let finished_at = chrono::Utc::now().to_rfc3339();
    crate::LedgerRecord {
        run_id: format!("run-{}-error", repo_name_from_workspace(&job.workspace)),
        plan_id: String::new(),
        repo: repo_name_from_workspace(&job.workspace),
        branch: job
            .trunk
            .clone()
            .unwrap_or_else(crate::orchestrator::RunInputs::default_trunk),
        profile: profile.to_string(),
        exit_code: -1,
        attempts: 0,
        failure_class: crate::FailureClass::Red,
        base_sha: String::new(),
        head_sha: String::new(),
        commits_ahead: 0,
        pr_url: None,
        reason: Some(format!("run failed before completion: {error}")),
        started_at: started_at.to_string(),
        finished_at,
        schema_version: crate::ledger::CURRENT_SCHEMA_VERSION,
        change_id: change_id.map(str::to_string),
    }
}

/// Runs a Phase 5 multi-repo batch sequentially (deterministic, git-safe —
/// never parallel), stopping early the first time a `required` repo's run
/// finishes non-Green (ordered-stop-on-failure, the default policy).
///
/// `make_runner` constructs a fresh [`crate::CommandRunner`] for each
/// [`RepoJob`] (production callers should share one
/// [`crate::CerebrumClient`] — via [`crate::RealRunner::with_shared_cerebrum`]
/// — across every call, so cerebrum is spawned once per batch, not once per
/// repo). Each repo's run goes through the ordinary single-repo
/// [`crate::orchestrator::run`] flow unchanged; a hard [`crate::CoreError`]
/// from one repo (e.g. a plan fetch failure) is captured as a synthetic Red
/// record rather than aborting the whole batch.
///
/// Every produced [`crate::LedgerRecord`] has its `change_id` field set to
/// `change_id`, correlating the batch's per-repo rows.
pub async fn run_multi<R, F, Fut>(
    cfg: &crate::Config,
    manifest: ChangeManifest,
    change_id: Option<&str>,
    make_runner: F,
) -> Vec<crate::LedgerRecord>
where
    F: Fn(&RepoJob) -> Fut,
    Fut: std::future::Future<Output = R>,
    R: crate::CommandRunner,
{
    let mut records = Vec::with_capacity(manifest.repos.len());

    for job in &manifest.repos {
        let started_at = chrono::Utc::now().to_rfc3339();
        let profile = job
            .profile
            .clone()
            .unwrap_or_else(|| cfg.default_profile.clone());

        let runner = make_runner(job).await;
        let inputs = crate::orchestrator::RunInputs {
            workspace: job.workspace.clone(),
            repo: repo_name_from_workspace(&job.workspace),
            plan_ref: job.plan_ref.clone(),
            profile: Some(profile.clone()),
            slug_override: job.slug_override.clone(),
            trunk: job
                .trunk
                .clone()
                .unwrap_or_else(crate::orchestrator::RunInputs::default_trunk),
            change_id: change_id.map(str::to_string),
        };

        let record = match crate::orchestrator::run(&runner, cfg, inputs).await {
            Ok(record) => record,
            Err(e) => error_record(job, &profile, &started_at, change_id, &e),
        };

        let stop = record.failure_class != crate::FailureClass::Green && job.required;
        records.push(record);
        if stop {
            break;
        }
    }

    records
}

#[cfg(test)]
mod tests {
    use super::{ChangeManifest, RepoJob};
    use crate::runner::fake::FakeRunner;
    use crate::{Config, FailureClass};

    fn test_cfg() -> Config {
        Config {
            ai_coding_monorepo: "/ai".to_string(),
            default_profile: "default".to_string(),
            max_attempts: 3,
            telegram_bot_token: None,
            telegram_chat_id: None,
            cerebrum_bin: "/nonexistent-cerebrum-bin".to_string(),
        }
    }

    fn job(workspace: &str, required: bool) -> RepoJob {
        RepoJob {
            workspace: workspace.to_string(),
            plan_ref: "plan-ref".to_string(),
            trunk: None,
            required,
            profile: None,
            slug_override: None,
        }
    }

    #[tokio::test]
    async fn all_green_runs_every_repo_and_stamps_change_id() {
        let manifest = ChangeManifest {
            repos: vec![job("/repo-a", true), job("/repo-b", true)],
        };

        let records = super::run_multi(&test_cfg(), manifest, Some("change-123"), |_job| async {
            let mut runner = FakeRunner::new();
            runner.push_exit_code(0);
            runner.set_commits_ahead(1);
            runner
        })
        .await;

        assert_eq!(records.len(), 2, "both repos should have run");
        assert!(records
            .iter()
            .all(|r| r.failure_class == FailureClass::Green));
        assert!(
            records
                .iter()
                .all(|r| r.change_id.as_deref() == Some("change-123")),
            "every record must be stamped with the batch's change_id"
        );
    }

    #[tokio::test]
    async fn required_repo_failure_stops_the_batch() {
        let manifest = ChangeManifest {
            repos: vec![job("/repo-a", true), job("/repo-b", true)],
        };

        let records = super::run_multi(&test_cfg(), manifest, None, |job| {
            let workspace = job.workspace.clone();
            async move {
                let mut runner = FakeRunner::new();
                if workspace == "/repo-a" {
                    runner.push_exit_code(3); // Red
                } else {
                    runner.push_exit_code(0);
                    runner.set_commits_ahead(1);
                }
                runner
            }
        })
        .await;

        assert_eq!(
            records.len(),
            1,
            "batch must stop after the first required repo goes Red, never reaching repo-b"
        );
        assert_eq!(records[0].failure_class, FailureClass::Red);
        assert_eq!(records[0].repo, "repo-a");
    }

    #[tokio::test]
    async fn non_required_repo_failure_does_not_stop_the_batch() {
        let manifest = ChangeManifest {
            repos: vec![job("/repo-a", false), job("/repo-b", true)],
        };

        let records = super::run_multi(&test_cfg(), manifest, None, |job| {
            let workspace = job.workspace.clone();
            async move {
                let mut runner = FakeRunner::new();
                if workspace == "/repo-a" {
                    runner.push_exit_code(3); // Red, but not required
                } else {
                    runner.push_exit_code(0);
                    runner.set_commits_ahead(1);
                }
                runner
            }
        })
        .await;

        assert_eq!(
            records.len(),
            2,
            "a non-required repo's failure must not stop the batch"
        );
        assert_eq!(records[0].failure_class, FailureClass::Red);
        assert_eq!(records[1].failure_class, FailureClass::Green);
    }

    #[tokio::test]
    async fn fetch_plan_error_produces_a_red_record_instead_of_aborting() {
        let manifest = ChangeManifest {
            repos: vec![job("/repo-a", true)],
        };

        let records = super::run_multi(&test_cfg(), manifest, None, |_job| async {
            let mut runner = FakeRunner::new();
            runner.set_fetch_plan_should_fail(true);
            runner
        })
        .await;

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].failure_class, FailureClass::Red);
        assert!(
            records[0]
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("run failed before completion"),
            "reason should explain the hard error, got: {:?}",
            records[0].reason
        );
    }
}
