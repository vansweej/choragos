//! choragos — CLI mirroring the `choragos_run_plan` MCP tool.
//!
//! Parses `--plan`, `--profile`, and `--slug` for a single-repo run, or
//! `--change-ref` (mutually exclusive with `--plan`) for a Phase 5
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
    /// Defaults to `"PLAN.md"` for backward compatibility when neither
    /// `--plan` nor `--change-ref` is given. Mutually exclusive with
    /// `--change-ref`.
    #[arg(long, conflicts_with = "change_ref")]
    plan: Option<String>,

    /// Reference to a Phase 5 change manifest stored in cerebrum (a
    /// `plan:<id>` scope id whose content is a JSON `ChangeManifest`).
    /// Runs each listed repo sequentially via `run_multi`. Mutually
    /// exclusive with `--plan`.
    #[arg(long, conflicts_with = "plan")]
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
    let plan_ref = args.plan.unwrap_or_else(|| "PLAN.md".to_string());

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
    };

    let record = choragos_core::orchestrator::run(&runner, &config, inputs).await?;

    let json = serde_json::to_string_pretty(&record)?;
    println!("{json}");

    std::process::exit(worst_exit_code(std::slice::from_ref(&record)));
}
