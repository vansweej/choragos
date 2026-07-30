//! Thin typed client over the cerebrum MCP server.
//!
//! Spawns the cerebrum binary as a child process over stdio (the same
//! wrapped binary the opencode session's cerebrum MCP registration uses, so
//! both share one memory store with zero extra configuration), lazily
//! connecting on first use. All calls degrade to
//! [`crate::CoreError::Transient`] on failure — cerebrum is a best-effort
//! dependency, never a hard requirement for a plan-cycle run to proceed
//! (aside from `fetch_plan`, where a missing plan is a genuine
//! [`crate::CoreError::NotFound`]).

use rmcp::model::CallToolRequestParams;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::OnceCell;

/// A lazily-connected client for the cerebrum MCP server.
///
/// Cheap to construct (`new` does no I/O); the child process is spawned and
/// the MCP handshake performed on the first call that needs it, then reused
/// for the lifetime of this client. Intended to be held behind an `Arc` and
/// shared across a batch of runs (never re-spawned per repo).
pub struct CerebrumClient {
    bin: String,
    conn: OnceCell<Arc<RunningService<RoleClient, ()>>>,
}

impl CerebrumClient {
    /// Creates a new client that will spawn `bin` (the cerebrum binary path)
    /// on first use.
    pub fn new(bin: impl Into<String>) -> Self {
        Self {
            bin: bin.into(),
            conn: OnceCell::new(),
        }
    }

    /// Returns the shared connection, lazily spawning and connecting on the
    /// first call.
    async fn connection(&self) -> Result<&Arc<RunningService<RoleClient, ()>>, crate::CoreError> {
        self.conn
            .get_or_try_init(|| async {
                let transport = TokioChildProcess::new(tokio::process::Command::new(&self.bin))
                    .map_err(|e| crate::CoreError::Transient {
                        context: "cerebrum spawn".to_string(),
                        message: e.to_string(),
                    })?;
                let service =
                    ().serve(transport)
                        .await
                        .map_err(|e| crate::CoreError::Transient {
                            context: "cerebrum init".to_string(),
                            message: e.to_string(),
                        })?;
                Ok::<_, crate::CoreError>(Arc::new(service))
            })
            .await
    }

    /// Calls a cerebrum MCP tool by name with JSON arguments and parses the
    /// response's text content as JSON.
    async fn call(&self, name: &'static str, args: Value) -> Result<Value, crate::CoreError> {
        let service = self.connection().await?;
        let mut params = CallToolRequestParams::new(name);
        if let Some(obj) = args.as_object().cloned() {
            params = params.with_arguments(obj);
        }
        let result =
            service
                .peer()
                .call_tool(params)
                .await
                .map_err(|e| crate::CoreError::Transient {
                    context: format!("cerebrum {name}"),
                    message: e.to_string(),
                })?;

        let text = result
            .content
            .first()
            .and_then(|c| match &c.raw {
                rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .ok_or_else(|| crate::CoreError::Transient {
                context: format!("cerebrum {name}"),
                message: "non-text tool result".to_string(),
            })?;

        serde_json::from_str(&text).map_err(crate::CoreError::Json)
    }

    /// Fetches the plan body stored under the exact `plan:<plan_ref>` scope.
    ///
    /// Uses `exact_scope: true` so a large corpus of high-salience global
    /// memories cannot crowd the plan out of the result window (a real
    /// failure mode confirmed against a production cerebrum store — see
    /// cerebrum-mcp's `exact_scope` fix). Returns
    /// [`crate::CoreError::NotFound`] if no memory with that exact scope is
    /// present in the results.
    pub async fn fetch_plan(&self, plan_ref: &str) -> Result<String, crate::CoreError> {
        let scope = format!("plan:{plan_ref}");
        let value = self
            .call(
                "recall_by_scope",
                json!({ "query": plan_ref, "scope": scope, "limit": 5, "exact_scope": true }),
            )
            .await?;

        value
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|r| r.get("scope").and_then(Value::as_str) == Some(scope.as_str()))
            .and_then(|r| r.get("content").and_then(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| crate::CoreError::NotFound(format!("plan_ref '{plan_ref}'")))
    }

    /// Records a best-effort, low-salience progress note under `session`.
    ///
    /// Failures are mapped to `Transient` and should be swallowed by the
    /// caller — a progress note is a nice-to-have, never fatal to a run.
    pub async fn note_progress(&self, session: &str, text: &str) -> Result<(), crate::CoreError> {
        self.call(
            "remember",
            json!({ "content": text, "scope": session, "salience": 0.4, "type": "context" }),
        )
        .await?;
        Ok(())
    }

    /// Cleans up all memories under `session` (best-effort, scoped forget —
    /// never a global session clear which would affect other concurrent
    /// sessions sharing the same cerebrum store).
    pub async fn cleanup_session(&self, session: &str) -> Result<(), crate::CoreError> {
        let value = self
            .call(
                "recall_by_scope",
                json!({ "query": "progress", "scope": session, "limit": 100, "exact_scope": true }),
            )
            .await?;

        if let Some(results) = value.get("results").and_then(Value::as_array) {
            for r in results {
                if let Some(id) = r.get("id").and_then(Value::as_str) {
                    // Best-effort per item: one failed forget must not abort
                    // cleanup of the rest.
                    let _ = self.call("forget", json!({ "memory_id": id })).await;
                }
            }
        }
        Ok(())
    }

    /// Test-only seam: builds a client around an already-connected service
    /// (e.g. one produced by an in-process fake MCP server over a duplex
    /// transport), bypassing the lazy child-process spawn entirely.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_service(service: RunningService<RoleClient, ()>) -> Self {
        let conn = OnceCell::new();
        // `set` on a fresh OnceCell cannot fail.
        let _ = conn.set(Arc::new(service));
        Self {
            bin: String::new(),
            conn,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Fake-MCP contract tests (R9): a minimal in-process cerebrum
    //! `ServerHandler` recording every tool call, connected to a real
    //! [`CerebrumClient`] over an in-memory duplex transport (no child
    //! process, no Ollama). These freeze the exact tool names, argument
    //! shapes, and scope strings choragos depends on, so a cerebrum-side
    //! rename or scope-grammar change breaks a fast CI test instead of a
    //! live run.

    use super::CerebrumClient;
    use rmcp::handler::server::ServerHandler;
    use rmcp::model::{
        Annotated, CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
        RawContent, ServerCapabilities, ServerInfo, Tool,
    };
    use rmcp::service::{RequestContext, RoleClient, RoleServer};
    use rmcp::{ErrorData, ServiceExt};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    /// Records every `(tool_name, arguments)` call and returns canned JSON
    /// matching cerebrum's real response shapes.
    #[derive(Clone, Default)]
    struct FakeCerebrum {
        calls: Arc<Mutex<Vec<(String, Value)>>>,
        recall_results: Arc<Mutex<Value>>,
    }

    impl FakeCerebrum {
        fn set_recall_results(&self, results: Value) {
            *self.recall_results.lock().unwrap() = results;
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    fn tool(name: &'static str) -> Tool {
        Tool::new(name, name, serde_json::Map::new())
    }

    impl ServerHandler for FakeCerebrum {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        #[allow(clippy::manual_async_fn)]
        fn list_tools(
            &self,
            _request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<ListToolsResult, ErrorData>> + '_ {
            async move {
                Ok(ListToolsResult {
                    tools: vec![
                        tool("remember"),
                        tool("recall"),
                        tool("memorize"),
                        tool("forget"),
                        tool("end_session"),
                        tool("recall_by_scope"),
                    ],
                    ..Default::default()
                })
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> impl std::future::Future<Output = Result<CallToolResult, ErrorData>> + '_ {
            async move {
                let name = request.name.to_string();
                let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
                self.calls.lock().unwrap().push((name.clone(), args));

                let payload = match name.as_str() {
                    "remember" => json!({ "success": true, "memory_id": "mem-fake-1" }),
                    "recall_by_scope" => {
                        let results = self.recall_results.lock().unwrap().clone();
                        json!({ "success": true, "count": 0, "results": results })
                    }
                    "forget" => json!({ "success": true, "memory_id": "mem-fake-1" }),
                    _ => json!({ "success": true }),
                };

                Ok(CallToolResult::success(vec![Annotated::new(
                    RawContent::text(payload.to_string()),
                    None,
                )]))
            }
        }
    }

    /// Connects a real `CerebrumClient` to a `FakeCerebrum` over an
    /// in-process duplex transport (`DuplexStream` is `AsyncRead + AsyncWrite`,
    /// satisfying rmcp's combined-RW `IntoTransport` impl directly).
    async fn connect_to_fake(fake: FakeCerebrum) -> CerebrumClient {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);

        // The server and client sides each perform (and block on) the MCP
        // initialize handshake with their peer, so they must run
        // concurrently, not sequentially — awaiting the server side first
        // would deadlock waiting for a client that hasn't started yet.
        tokio::spawn(async move {
            let server = fake
                .serve(server_io)
                .await
                .expect("serve fake cerebrum over duplex");
            let _ = server.waiting().await;
        });

        let client: rmcp::service::RunningService<RoleClient, ()> = ()
            .serve(client_io)
            .await
            .expect("connect real client to fake cerebrum");
        CerebrumClient::from_service(client)
    }

    #[tokio::test]
    async fn fetch_plan_calls_recall_by_scope_with_exact_plan_scope() {
        let fake = FakeCerebrum::default();
        fake.set_recall_results(json!([
            { "id": "g1", "content": "NOISE global body", "scope": "global" },
            { "id": "p1", "content": "# Real Plan\nbody", "scope": "plan:abc" },
        ]));
        let client = connect_to_fake(fake.clone()).await;

        let body = client.fetch_plan("abc").await.expect("fetch_plan");

        let calls = fake.calls();
        assert_eq!(calls[0].0, "recall_by_scope");
        assert_eq!(calls[0].1["scope"], "plan:abc");
        assert_eq!(calls[0].1["exact_scope"], true);
        assert!(calls[0].1.get("query").is_some());
        assert!(calls[0].1.get("limit").is_some());

        assert!(
            body.contains("Real Plan"),
            "must return the plan:abc entry, not global noise"
        );
    }

    #[tokio::test]
    async fn fetch_plan_only_global_result_is_not_found() {
        let fake = FakeCerebrum::default();
        fake.set_recall_results(json!([
            { "id": "g1", "content": "global only", "scope": "global" },
        ]));
        let client = connect_to_fake(fake).await;

        let result = client.fetch_plan("abc").await;
        assert!(
            matches!(result, Err(crate::CoreError::NotFound(_))),
            "a global-only result set must not satisfy fetch_plan; got {result:?}"
        );
    }

    #[tokio::test]
    async fn note_progress_uses_session_scope_and_low_salience() {
        let fake = FakeCerebrum::default();
        let client = connect_to_fake(fake.clone()).await;

        client
            .note_progress("session:abc:123", "attempt 1")
            .await
            .expect("note_progress");

        let calls = fake.calls();
        assert_eq!(calls[0].0, "remember");
        assert_eq!(calls[0].1["scope"], "session:abc:123");
        assert_eq!(calls[0].1["salience"], 0.4);
        assert_eq!(calls[0].1["content"], "attempt 1");
    }

    #[tokio::test]
    async fn cleanup_recalls_session_then_forgets_each_by_id() {
        let fake = FakeCerebrum::default();
        fake.set_recall_results(json!([
            { "id": "s1", "content": "n", "scope": "session:abc:123" },
            { "id": "s2", "content": "n", "scope": "session:abc:123" },
        ]));
        let client = connect_to_fake(fake.clone()).await;

        client
            .cleanup_session("session:abc:123")
            .await
            .expect("cleanup_session");

        let calls = fake.calls();
        assert_eq!(calls[0].0, "recall_by_scope");
        assert_eq!(calls[0].1["scope"], "session:abc:123");
        assert_eq!(calls[0].1["exact_scope"], true);

        let forgotten: Vec<String> = calls[1..]
            .iter()
            .map(|(name, args)| {
                assert_eq!(name, "forget");
                args["memory_id"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(forgotten, vec!["s1".to_string(), "s2".to_string()]);
    }
}
