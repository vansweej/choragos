//! choragos-mcp-server — exposes `choragos_run_plan` over the rmcp stdio
//! transport.

use anyhow::Result;
use rmcp::{
    model::{CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    schemars,
    tool, tool_handler, tool_router,
    handler::server::tool::ToolRouter,
    ServerHandler,
};
use serde::Deserialize;

/// Arguments accepted by the `choragos_run_plan` MCP tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunPlanArgs {
    /// Path to the plan Markdown file, relative to the workspace root.
    /// Defaults to `"PLAN.md"` when omitted.
    pub plan_path: Option<String>,

    /// Pipeline profile to use.  Falls back to `CHORAGOS_DEFAULT_PROFILE`
    /// when omitted.
    pub profile: Option<String>,

    /// Override the auto-derived branch slug.  When omitted the slug is
    /// derived from the plan title.
    pub slug: Option<String>,
}

/// The choragos MCP server.
pub struct ChoragosServer {
    tool_router: ToolRouter<Self>,
    config: choragos_core::Config,
}

#[tool_router]
impl ChoragosServer {
    /// Creates a new [`ChoragosServer`] from the given [`choragos_core::Config`].
    pub fn new(config: choragos_core::Config) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
        }
    }

    /// Runs the choragos plan-cycle orchestrator and returns the ledger record.
    #[tool(description = "Run the choragos plan-cycle orchestrator. Branches feat/<slug>, executes the ai-coding plan-cycle, opens a PR on a green run with commits, and returns the LedgerRecord as JSON.")]
    async fn choragos_run_plan(
        &self,
        #[tool(aggr)] args: RunPlanArgs,
    ) -> Result<CallToolResult, rmcp::Error> {
        let workspace = std::env::current_dir()
            .map_err(|e| rmcp::Error::internal_error(e.to_string(), None))?;

        let repo = workspace
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let workspace_str = workspace
            .to_str()
            .ok_or_else(|| rmcp::Error::internal_error("non-UTF-8 workspace path".to_string(), None))?
            .to_string();

        let plan_path = args.plan_path.unwrap_or_else(|| "PLAN.md".to_string());

        let runner = choragos_core::RealRunner::new(
            self.config.ai_coding_monorepo.clone(),
            self.config.telegram_bot_token.clone(),
            self.config.telegram_chat_id.clone(),
        );

        let inputs = choragos_core::orchestrator::RunInputs {
            workspace: workspace_str,
            repo,
            plan_path,
            profile: args.profile,
            slug_override: args.slug,
        };

        let record = choragos_core::orchestrator::run(&runner, &self.config, inputs)
            .await
            .map_err(|e| rmcp::Error::internal_error(e.to_string(), None))?;

        let json = serde_json::to_string(&record)
            .map_err(|e| rmcp::Error::internal_error(e.to_string(), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

#[tool_handler]
impl ServerHandler for ChoragosServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "choragos-mcp-server".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(
                "Use choragos_run_plan to run the plan-cycle orchestrator. \
                 The tool branches, runs ai-coding, opens a PR on success, \
                 and returns a JSON LedgerRecord."
                    .to_string(),
            ),
        }
    }
}

#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() -> Result<()> {
    let config = choragos_core::Config::from_env()?;
    let server = ChoragosServer::new(config);
    let transport = rmcp::transport::stdio();
    let ct = rmcp::service::serve_server(server, transport).await?;
    ct.waiting().await?;
    Ok(())
}
