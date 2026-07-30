//! choragos — CLI mirroring the `choragos_run_plan` MCP tool.
//!
//! Parses `--plan`, `--profile`, and `--slug` arguments, runs the
//! plan-cycle orchestrator, prints the [`LedgerRecord`] as pretty JSON, and
//! exits with a code reflecting the [`FailureClass`]:
//!
//! | Exit code | Meaning |
//! |-----------|---------|
//! | `0`       | Green   |
//! | `1`       | Orange  |
//! | `2`       | Red     |

use clap::Parser;

/// choragos — deterministic plan-cycle orchestrator CLI.
#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Path to the plan Markdown file, relative to the workspace root.
    #[arg(long, default_value = "PLAN.md")]
    plan: String,

    /// Pipeline profile to use.  Falls back to `CHORAGOS_DEFAULT_PROFILE`
    /// when omitted.
    #[arg(long)]
    profile: Option<String>,

    /// Override the auto-derived branch slug.  When omitted the slug is
    /// derived from the plan title.
    #[arg(long)]
    slug: Option<String>,
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
        plan_ref: args.plan,
        profile: args.profile,
        slug_override: args.slug,
    };

    let record = choragos_core::orchestrator::run(&runner, &config, inputs).await?;

    let json = serde_json::to_string_pretty(&record)?;
    println!("{json}");

    let exit_code = match record.failure_class {
        choragos_core::FailureClass::Green => 0,
        choragos_core::FailureClass::Orange => 1,
        choragos_core::FailureClass::Red => 2,
    };

    std::process::exit(exit_code);
}
