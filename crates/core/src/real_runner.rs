//! Production [`CommandRunner`] implementation backed by git, gh, bun, and reqwest.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{CerebrumClient, CoreError, LedgerRecord, Memory, Pipeline, Sink, Vcs};

/// A [`CommandRunner`] that shells out to real external tools.
///
/// - Git operations use the `git` binary on `PATH`, run in [`workdir`].
/// - Pull-request creation uses the `gh` CLI (also run in `workdir`).
/// - Plan-cycle execution uses `bun run … pipeline plan-cycle`.
/// - Telegram notifications use `reqwest` to POST to the Bot API.
/// - Plan fetch and session progress use the cerebrum MCP server via
///   [`CerebrumClient`].
///
/// [`workdir`]: RealRunner::workdir
pub struct RealRunner {
    /// Directory the git/gh commands run in (the target workspace repo).
    pub workdir: PathBuf,
    /// Absolute path to the `ai-coding` monorepo checkout.
    pub ai_coding_monorepo: String,
    /// Telegram bot token, if configured.
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID, if configured.
    pub telegram_chat_id: Option<String>,
    /// Cerebrum MCP client (lazily connects on first use). Shared via `Arc`
    /// so a multi-repo batch (Phase 5's `run_multi`) can construct one
    /// [`RealRunner`] per repo workdir while spawning cerebrum only once —
    /// see [`RealRunner::with_shared_cerebrum`].
    pub cerebrum: Arc<CerebrumClient>,
}

impl RealRunner {
    /// Creates a new [`RealRunner`] operating on `workdir`, owning a fresh
    /// [`CerebrumClient`] (wrapped in its own `Arc`). For a single-repo run
    /// this is the usual entry point; for a multi-repo batch, construct one
    /// shared client and use [`RealRunner::with_shared_cerebrum`] per repo
    /// instead, so cerebrum is spawned only once for the whole batch.
    pub fn new(
        workdir: impl Into<PathBuf>,
        ai_coding_monorepo: impl Into<String>,
        telegram_bot_token: Option<String>,
        telegram_chat_id: Option<String>,
        cerebrum_bin: impl Into<String>,
    ) -> Self {
        Self::with_shared_cerebrum(
            workdir,
            ai_coding_monorepo,
            telegram_bot_token,
            telegram_chat_id,
            Arc::new(CerebrumClient::new(cerebrum_bin)),
        )
    }

    /// Creates a new [`RealRunner`] operating on `workdir`, sharing an
    /// already-constructed [`CerebrumClient`] (e.g. across every repo in a
    /// Phase 5 multi-repo batch, so cerebrum is spawned once per batch, not
    /// once per repo).
    pub fn with_shared_cerebrum(
        workdir: impl Into<PathBuf>,
        ai_coding_monorepo: impl Into<String>,
        telegram_bot_token: Option<String>,
        telegram_chat_id: Option<String>,
        cerebrum: Arc<CerebrumClient>,
    ) -> Self {
        Self {
            workdir: workdir.into(),
            ai_coding_monorepo: ai_coding_monorepo.into(),
            telegram_bot_token,
            telegram_chat_id,
            cerebrum,
        }
    }
}

/// Runs a `git` sub-command in `dir` and returns its trimmed stdout.
///
/// Failures are mapped to [`CoreError::Command`].
async fn git_in(dir: &Path, args: &[&str]) -> Result<String, CoreError> {
    let output = tokio::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .await
        .map_err(|e| CoreError::Command {
            context: format!("git {}", args.join(" ")),
            message: e.to_string(),
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(CoreError::Command {
            context: format!("git {}", args.join(" ")),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

#[cfg(not(tarpaulin_include))]
impl Memory for RealRunner {
    async fn fetch_plan(&self, plan_ref: &str) -> Result<String, CoreError> {
        self.cerebrum.fetch_plan(plan_ref).await
    }

    async fn begin_session(&self, plan_ref: &str) -> Result<String, CoreError> {
        // Local mint, infallible: cerebrum has no open-session tool, and a
        // session is just a scope-id string. This must never fail — an
        // Ollama/cerebrum hiccup at session-open time must not abort a run
        // before the plan-cycle executor even starts.
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        Ok(format!("session:{plan_ref}:{nanos}"))
    }

    async fn note_progress(&self, session: &str, text: &str) -> Result<(), CoreError> {
        self.cerebrum.note_progress(session, text).await
    }

    async fn cleanup_session(&self, session: &str) -> Result<(), CoreError> {
        self.cerebrum.cleanup_session(session).await
    }
}

#[cfg(not(tarpaulin_include))]
impl Vcs for RealRunner {
    async fn git_fetch(&self, remote: &str, branch: &str) -> Result<(), CoreError> {
        git_in(&self.workdir, &["fetch", remote, branch]).await?;
        Ok(())
    }

    async fn current_branch(&self) -> Result<String, CoreError> {
        git_in(&self.workdir, &["rev-parse", "--abbrev-ref", "HEAD"]).await
    }

    async fn is_working_tree_clean(&self) -> Result<bool, CoreError> {
        let output = git_in(&self.workdir, &["status", "--porcelain"]).await?;
        Ok(output.is_empty())
    }

    async fn local_matches_remote(&self, branch: &str) -> Result<bool, CoreError> {
        let local = git_in(&self.workdir, &["rev-parse", branch]).await?;
        let remote_ref = format!("origin/{branch}");
        let remote = git_in(&self.workdir, &["rev-parse", &remote_ref]).await?;
        Ok(local == remote)
    }

    async fn branch_exists(&self, name: &str) -> Result<bool, CoreError> {
        let refspec = format!("refs/heads/{name}");
        let output = tokio::process::Command::new("git")
            .current_dir(&self.workdir)
            .args(["rev-parse", "--verify", "--quiet", &refspec])
            .output()
            .await
            .map_err(|e| CoreError::Command {
                context: format!("git rev-parse --verify --quiet {refspec}"),
                message: e.to_string(),
            })?;
        Ok(output.status.success())
    }

    async fn create_branch(&self, name: &str) -> Result<(), CoreError> {
        git_in(&self.workdir, &["switch", "-c", name]).await?;
        Ok(())
    }

    async fn switch_branch(&self, name: &str) -> Result<(), CoreError> {
        git_in(&self.workdir, &["switch", name]).await?;
        Ok(())
    }

    async fn head_sha(&self) -> Result<String, CoreError> {
        git_in(&self.workdir, &["rev-parse", "HEAD"]).await
    }

    async fn commits_ahead(&self, base_sha: &str) -> Result<u32, CoreError> {
        let range = format!("{base_sha}..HEAD");
        let raw = git_in(&self.workdir, &["rev-list", "--count", &range])
            .await
            .unwrap_or_default();
        Ok(raw.trim().parse::<u32>().unwrap_or(0))
    }

    async fn create_pr(&self, base: &str, title: &str, body: &str) -> Result<String, CoreError> {
        // Callers push the branch first (see `Vcs::push_head`); this method
        // only invokes `gh pr create`.
        let output = tokio::process::Command::new("gh")
            .current_dir(&self.workdir)
            .args([
                "pr", "create", "--base", base, "--title", title, "--body", body,
            ])
            .output()
            .await
            .map_err(|e| CoreError::Command {
                context: "gh pr create".to_string(),
                message: e.to_string(),
            })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(CoreError::Command {
                context: "gh pr create".to_string(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            })
        }
    }

    /// Pushes the current branch to `origin`, creating the upstream ref.
    ///
    /// Split out so the push — the part that actually touches git state — is
    /// exercised by the integration tests, while the `gh` invocation stays
    /// the only untested outer edge. Idempotent: `-u` is safe to repeat on a
    /// resumed run.
    async fn push_head(&self) -> Result<(), CoreError> {
        git_in(&self.workdir, &["push", "-u", "origin", "HEAD"]).await?;
        Ok(())
    }

    async fn find_pr(&self, branch: &str) -> Result<Option<String>, CoreError> {
        let output = tokio::process::Command::new("gh")
            .current_dir(&self.workdir)
            .args(["pr", "view", branch, "--json", "url", "-q", ".url"])
            .output()
            .await
            .map_err(|e| CoreError::Command {
                context: "gh pr view".to_string(),
                message: e.to_string(),
            })?;

        if output.status.success() {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if url.is_empty() {
                Ok(None)
            } else {
                Ok(Some(url))
            }
        } else {
            // `gh pr view` exits non-zero when no PR exists for the branch —
            // that is not an error condition here.
            Ok(None)
        }
    }
}

#[cfg(not(tarpaulin_include))]
impl Pipeline for RealRunner {
    async fn run_plan_cycle(
        &self,
        workspace: &str,
        plan_ref: &str,
        profile: &str,
        session: &str,
    ) -> Result<i32, CoreError> {
        let status = tokio::process::Command::new("bun")
            .args([
                "run",
                "--cwd",
                &self.ai_coding_monorepo,
                "pipeline",
                "plan-cycle",
                workspace,
                "--plan-ref",
                plan_ref,
                "--session",
                session,
                "--profile",
                profile,
                "--verbose",
            ])
            .stderr(std::process::Stdio::inherit())
            .status()
            .await
            .map_err(|e| CoreError::Command {
                context: "bun run pipeline plan-cycle".to_string(),
                message: e.to_string(),
            })?;

        Ok(status.code().unwrap_or(3))
    }
}

#[cfg(not(tarpaulin_include))]
impl Sink for RealRunner {
    async fn send_telegram(&self, text: &str) -> Result<(), CoreError> {
        let (token, chat_id) = match (&self.telegram_bot_token, &self.telegram_chat_id) {
            (Some(t), Some(c)) => (t.clone(), c.clone()),
            _ => return Ok(()),
        };

        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });

        reqwest::Client::new()
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::Command {
                context: "send_telegram".to_string(),
                message: e.to_string(),
            })?;

        Ok(())
    }

    async fn append_ledger(&self, record: &LedgerRecord) -> Result<(), CoreError> {
        let path = match crate::ledger::default_ledger_path() {
            Some(p) => p,
            None => return Ok(()),
        };
        let line = record.to_jsonl_line()?;
        crate::ledger::append_line(&path, &line)?;
        Ok(())
    }
}

// ── Integration tests ────────────────────────────────────────────────────────
//
// These drive the real `git` binary against throwaway repos created in a
// TempDir (and a bare local remote — no network). They retire the
// Coverage: skip debt on the git surface that broke twice in production.
// Only the `gh`, `bun`, and `reqwest` outer edges remain untested by design.
#[cfg(test)]
mod git_integration_tests {
    use super::*;
    use tempfile::TempDir;

    /// Runs a git command in `dir`, asserting success. Used only by test setup.
    async fn setup_git(dir: &Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .await
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// Initializes a repo on `main` with one commit and a configured identity.
    async fn init_repo_with_commit() -> TempDir {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        setup_git(p, &["init", "-b", "main"]).await;
        setup_git(p, &["config", "user.email", "test@example.com"]).await;
        setup_git(p, &["config", "user.name", "choragos-test"]).await;
        tokio::fs::write(p.join("README.md"), "hello\n")
            .await
            .unwrap();
        setup_git(p, &["add", "-A"]).await;
        setup_git(p, &["commit", "-m", "init"]).await;
        dir
    }

    async fn commit_file(dir: &Path, name: &str, contents: &str) {
        tokio::fs::write(dir.join(name), contents).await.unwrap();
        setup_git(dir, &["add", "-A"]).await;
        setup_git(dir, &["commit", "-m", &format!("add {name}")]).await;
    }

    fn runner_for(dir: &Path) -> RealRunner {
        RealRunner::new(
            dir.to_path_buf(),
            "/nonexistent-monorepo",
            None,
            None,
            "/nonexistent-cerebrum-bin",
        )
    }

    #[tokio::test]
    async fn current_branch_head_and_clean_flag() {
        let dir = init_repo_with_commit().await;
        let r = runner_for(dir.path());

        assert_eq!(r.current_branch().await.unwrap(), "main");
        assert!(!r.head_sha().await.unwrap().is_empty());
        assert!(r.is_working_tree_clean().await.unwrap());

        tokio::fs::write(dir.path().join("dirty.txt"), "x")
            .await
            .unwrap();
        assert!(!r.is_working_tree_clean().await.unwrap());
    }

    #[tokio::test]
    async fn create_switch_and_branch_exists() {
        let dir = init_repo_with_commit().await;
        let r = runner_for(dir.path());

        assert!(!r.branch_exists("feat/x").await.unwrap());
        r.create_branch("feat/x").await.unwrap();
        assert_eq!(r.current_branch().await.unwrap(), "feat/x");
        assert!(r.branch_exists("feat/x").await.unwrap());

        r.switch_branch("main").await.unwrap();
        assert_eq!(r.current_branch().await.unwrap(), "main");
    }

    #[tokio::test]
    async fn commits_ahead_counts_new_commits() {
        let dir = init_repo_with_commit().await;
        let r = runner_for(dir.path());

        let base = r.head_sha().await.unwrap();
        assert_eq!(r.commits_ahead(&base).await.unwrap(), 0);

        commit_file(dir.path(), "f.txt", "a").await;
        assert_eq!(r.commits_ahead(&base).await.unwrap(), 1);

        commit_file(dir.path(), "g.txt", "b").await;
        assert_eq!(r.commits_ahead(&base).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn fetch_and_local_matches_remote() {
        let remote = TempDir::new().unwrap();
        setup_git(remote.path(), &["init", "--bare", "-b", "main"]).await;

        let dir = init_repo_with_commit().await;
        setup_git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        )
        .await;
        setup_git(dir.path(), &["push", "-u", "origin", "main"]).await;

        let r = runner_for(dir.path());
        r.git_fetch("origin", "main").await.unwrap();
        assert!(r.local_matches_remote("main").await.unwrap());

        // A local commit that is not pushed => local no longer matches remote.
        commit_file(dir.path(), "h.txt", "z").await;
        r.git_fetch("origin", "main").await.unwrap();
        assert!(!r.local_matches_remote("main").await.unwrap());
    }

    #[tokio::test]
    async fn push_head_pushes_current_branch_to_remote() {
        let remote = TempDir::new().unwrap();
        setup_git(remote.path(), &["init", "--bare", "-b", "main"]).await;

        let dir = init_repo_with_commit().await;
        setup_git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        )
        .await;

        let r = runner_for(dir.path());
        r.create_branch("feat/pushme").await.unwrap();
        r.push_head().await.unwrap();

        let out = tokio::process::Command::new("git")
            .current_dir(remote.path())
            .args(["branch", "--list", "feat/pushme"])
            .output()
            .await
            .unwrap();
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("feat/pushme"),
            "remote should have received feat/pushme"
        );
    }
}
