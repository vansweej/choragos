//! choragos — CLI mirroring the `choragos_run_plan` MCP tool.
//!
//! Parses `--plan-ref`, `--profile`, and `--slug` for a single-repo run, or
//! `--change-ref` (mutually exclusive with `--plan-ref`) for a Phase 5
//! multi-repo batch, runs the plan-cycle orchestrator, prints the
//! resulting [`LedgerRecord`](s) as pretty JSON, and exits with a code
//! reflecting the worst [`FailureClass`] across all records produced:
//!
//! | Exit code | Meaning |
//! |-----------|---------|
//! | `0`       | Green   |
//! | `1`       | Orange  |
//! | `2`       | Red     |

use std::sync::Arc;

use clap::Parser;

/// choragos — deterministic plan-cycle orchestrator CLI.
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Reference to a plan stored in cerebrum (a `plan:<id>` scope id).
    /// Required unless `--change-ref` is given. Mutually exclusive with
    /// `--change-ref`.
    #[arg(
        long = "plan-ref",
        conflicts_with = "change_ref",
        required_unless_present = "change_ref"
    )]
    plan_ref: Option<String>,

    /// Reference to a Phase 5 change manifest stored in cerebrum (a
    /// `plan:<id>` scope id whose content is a JSON `ChangeManifest`).
    /// Runs each listed repo sequentially via `run_multi`. Mutually
    /// exclusive with `--plan-ref`.
    #[arg(long, conflicts_with = "plan_ref")]
    change_ref: Option<String>,

    /// Pipeline profile to use.  Falls back to `CHORAGOS_DEFAULT_PROFILE`
    /// when omitted. Ignored for `--change-ref` runs where a `RepoJob`
    /// supplies its own profile.
    #[arg(long)]
    profile: Option<String>,

    /// Override the auto-derived branch slug.  When omitted the slug is
    /// derived from the plan title. Ignored for `--change-ref` runs.
    #[arg(long)]
    slug: Option<String>,

    /// Runs the ai-coding S7 dry-run mode (token-free).
    #[arg(long)]
    dry_run: bool,
}

/// Maps a batch of records to a process exit code reflecting the worst
/// (highest-severity) [`choragos_core::FailureClass`] across all of them.
fn worst_exit_code(records: &[choragos_core::LedgerRecord]) -> i32 {
    let worst = records
        .iter()
        .map(|r| r.failure_class)
        .max()
        .unwrap_or(choragos_core::FailureClass::Red);
    match worst {
        choragos_core::FailureClass::Green => 0,
        choragos_core::FailureClass::Orange => 1,
        choragos_core::FailureClass::Red => 2,
    }
}

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config = choragos_core::config::from_env()?;

    let workspace = std::env::current_dir()?;
    let repo = workspace
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let workspace_str = workspace
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path is not valid UTF-8"))?
        .to_string();

    if let Some(change_ref) = args.change_ref {
        // ── Phase 5: multi-repo batch ────────────────────────────────────
        let cerebrum = Arc::new(choragos_core::CerebrumClient::new(
            config.cerebrum_bin.clone(),
        ));
        let manifest_body = cerebrum.fetch_change(&change_ref).await?;
        let manifest: choragos_core::ChangeManifest = serde_json::from_str(&manifest_body)?;

        let cfg = config.clone();
        let records = choragos_core::change::run_multi(
            &config,
            manifest,
            Some(&change_ref),
            args.dry_run,
            |job: &choragos_core::RepoJob| {
                let job = job.clone();
                let cfg = cfg.clone();
                let cerebrum = Arc::clone(&cerebrum);
                async move {
                    choragos_core::RealRunner::with_shared_cerebrum(
                        job.workspace,
                        cfg.ai_coding_monorepo,
                        cfg.telegram_bot_token,
                        cfg.telegram_chat_id,
                        cerebrum,
                    )
                }
            },
        )
        .await;

        let json = serde_json::to_string_pretty(&records)?;
        println!("{json}");

        std::process::exit(worst_exit_code(&records));
    }

    // ── Single-repo run ───────────────────────────────────────────────────
    let plan_ref = args
        .plan_ref
        .expect("clap guarantees --plan-ref is present unless --change-ref");

    let runner = choragos_core::RealRunner::new(
        workspace.clone(),
        config.ai_coding_monorepo.clone(),
        config.telegram_bot_token.clone(),
        config.telegram_chat_id.clone(),
        config.cerebrum_bin.clone(),
    );

    let inputs = choragos_core::orchestrator::RunInputs {
        workspace: workspace_str,
        repo,
        plan_ref,
        profile: args.profile,
        slug_override: args.slug,
        trunk: choragos_core::orchestrator::RunInputs::default_trunk(),
        change_id: None,
        dry_run: args.dry_run,
    };

    let record = choragos_core::orchestrator::run(&runner, &config, inputs).await?;

    let json = serde_json::to_string_pretty(&record)?;
    println!("{json}");

    std::process::exit(worst_exit_code(std::slice::from_ref(&record)));
}

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn requires_plan_ref_or_change_ref() {
        assert!(Args::try_parse_from(["choragos"]).is_err());
    }

    #[test]
    fn plan_ref_and_change_ref_are_mutually_exclusive() {
        assert!(
            Args::try_parse_from(["choragos", "--plan-ref", "p", "--change-ref", "c"]).is_err()
        );
    }
}

#[cfg(test)]
mod worst_exit_code_tests {
    use super::worst_exit_code;
    use choragos_core::{FailureClass, LedgerRecord};

    fn make_record(failure_class: FailureClass) -> LedgerRecord {
        LedgerRecord {
            run_id: "run-1".to_string(),
            plan_id: "plan-1".to_string(),
            repo: "repo".to_string(),
            branch: "feat/x".to_string(),
            profile: "default".to_string(),
            exit_code: 0,
            attempts: 1,
            failure_class,
            base_sha: "abc".to_string(),
            head_sha: "def".to_string(),
            commits_ahead: 1,
            pr_url: None,
            reason: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            finished_at: "2024-01-01T00:01:00Z".to_string(),
            schema_version: 3,
            change_id: None,
        }
    }

    #[test]
    fn empty_slice_is_red() {
        assert_eq!(worst_exit_code(&[]), 2);
    }

    #[test]
    fn single_green_record_is_zero() {
        let records = [make_record(FailureClass::Green)];
        assert_eq!(worst_exit_code(&records), 0);
    }

    #[test]
    fn single_orange_record_is_one() {
        let records = [make_record(FailureClass::Orange)];
        assert_eq!(worst_exit_code(&records), 1);
    }

    #[test]
    fn single_red_record_is_two() {
        let records = [make_record(FailureClass::Red)];
        assert_eq!(worst_exit_code(&records), 2);
    }

    #[test]
    fn worst_of_mixed_batch_wins() {
        let records = [make_record(FailureClass::Green), make_record(FailureClass::Orange)];
        assert_eq!(worst_exit_code(&records), 1);
    }

    #[test]
    fn red_beats_everything() {
        let records = [
            make_record(FailureClass::Green),
            make_record(FailureClass::Orange),
            make_record(FailureClass::Red),
        ];
        assert_eq!(worst_exit_code(&records), 2);
    }
}
