//! choragos-mcp-server — exposes `choragos_run_plan` over the rmcp stdio
//! transport.

use anyhow::Result;
use rmcp::{
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use serde::Deserialize;

/// Arguments accepted by the `choragos_run_plan` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunPlanArgs {
    /// Path to the plan Markdown file, relative to the workspace root.
    /// Defaults to `"PLAN.md"` when omitted. Mutually exclusive with
    /// `change_ref`.
    pub plan_path: Option<String>,

    /// Reference to a Phase 5 change manifest stored in cerebrum (a
    /// `plan:<id>` scope id whose content is a JSON `ChangeManifest`).
    /// Runs each listed repo sequentially and returns a JSON array of
    /// `LedgerRecord`s instead of a single record. Mutually exclusive with
    /// `plan_path`.
    pub change_ref: Option<String>,

    /// Pipeline profile to use.  Falls back to `CHORAGOS_DEFAULT_PROFILE`
    /// when omitted. Ignored for `change_ref` runs.
    pub profile: Option<String>,

    /// Override the auto-derived branch slug.  When omitted the slug is
    /// derived from the plan title. Ignored for `change_ref` runs.
    pub slug: Option<String>,
}

/// The choragos MCP server.
#[derive(Clone)]
pub struct ChoragosServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    config: std::sync::Arc<choragos_core::Config>,
}

#[tool_router]
impl ChoragosServer {
    /// Creates a new [`ChoragosServer`] from the given [`choragos_core::Config`].
    pub fn new(config: choragos_core::Config) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config: std::sync::Arc::new(config),
        }
    }

    /// Runs the choragos plan-cycle orchestrator and returns the ledger record.
    #[tool(
        description = "Run the choragos plan-cycle orchestrator. Branches feat/<slug>, executes the ai-coding plan-cycle, opens a PR on a green run with commits, and returns the LedgerRecord as JSON."
    )]
    async fn choragos_run_plan(
        &self,
        Parameters(args): Parameters<RunPlanArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if args.plan_path.is_some() && args.change_ref.is_some() {
            return Err(rmcp::ErrorData::invalid_params(
                "plan_path and change_ref are mutually exclusive".to_string(),
                None,
            ));
        }

        let workspace = std::env::current_dir()
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        let repo = workspace
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let workspace_str = workspace
            .to_str()
            .ok_or_else(|| {
                rmcp::ErrorData::internal_error("non-UTF-8 workspace path".to_string(), None)
            })?
            .to_string();

        if let Some(change_ref) = args.change_ref {
            // ── Phase 5: multi-repo batch ────────────────────────────────
            let cerebrum = std::sync::Arc::new(choragos_core::CerebrumClient::new(
                self.config.cerebrum_bin.clone(),
            ));
            let manifest_body = cerebrum
                .fetch_change(&change_ref)
                .await
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
            let manifest: choragos_core::ChangeManifest = serde_json::from_str(&manifest_body)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

            let cfg = (*self.config).clone();
            let records = choragos_core::change::run_multi(
                &self.config,
                manifest,
                Some(&change_ref),
                |job: &choragos_core::RepoJob| {
                    let job = job.clone();
                    let cfg = cfg.clone();
                    let cerebrum = std::sync::Arc::clone(&cerebrum);
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

            let json = serde_json::to_string(&records)
                .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

            return Ok(CallToolResult::success(vec![Content::text(json)]));
        }

        let plan_path = args.plan_path.unwrap_or_else(|| "PLAN.md".to_string());

        let runner = choragos_core::RealRunner::new(
            workspace.clone(),
            self.config.ai_coding_monorepo.clone(),
            self.config.telegram_bot_token.clone(),
            self.config.telegram_chat_id.clone(),
            self.config.cerebrum_bin.clone(),
        );

        let inputs = choragos_core::orchestrator::RunInputs {
            workspace: workspace_str,
            repo,
            plan_ref: plan_path,
            profile: args.profile,
            slug_override: args.slug,
            trunk: choragos_core::orchestrator::RunInputs::default_trunk(),
        };

        let record = choragos_core::orchestrator::run(&runner, &self.config, inputs)
            .await
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        let json = serde_json::to_string(&record)
            .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for ChoragosServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Use choragos_run_plan to run the plan-cycle orchestrator. \
             The tool branches, runs ai-coding, opens a PR on success, \
             and returns a JSON LedgerRecord.",
        )
    }
}

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    let config = choragos_core::config::from_env()?;
    let server = ChoragosServer::new(config);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
