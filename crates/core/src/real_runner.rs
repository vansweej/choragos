//! Production [`CommandRunner`] implementation backed by git, gh, bun, and reqwest.

use crate::{CommandRunner, CoreError, LedgerRecord};

/// A [`CommandRunner`] that shells out to real external tools.
///
/// - Git operations use the `git` binary on `PATH`.
/// - Pull-request creation uses the `gh` CLI.
/// - Plan-cycle execution uses `bun run … pipeline plan-cycle`.
/// - Telegram notifications use `reqwest` to POST to the Bot API.
pub struct RealRunner {
    /// Absolute path to the `ai-coding` monorepo checkout.
    pub ai_coding_monorepo: String,
    /// Telegram bot token, if configured.
    pub telegram_bot_token: Option<String>,
    /// Telegram chat ID, if configured.
    pub telegram_chat_id: Option<String>,
}

impl RealRunner {
    /// Creates a new [`RealRunner`].
    pub fn new(
        ai_coding_monorepo: impl Into<String>,
        telegram_bot_token: Option<String>,
        telegram_chat_id: Option<String>,
    ) -> Self {
        Self {
            ai_coding_monorepo: ai_coding_monorepo.into(),
            telegram_bot_token,
            telegram_chat_id,
        }
    }
}

/// Runs a `git` sub-command and returns its trimmed stdout.
///
/// Failures are mapped to [`CoreError::Command`].
#[cfg(not(tarpaulin_include))]
async fn git(args: &[&str]) -> Result<String, CoreError> {
    let output = tokio::process::Command::new("git")
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
impl CommandRunner for RealRunner {
    async fn read_to_string(&self, path: &str) -> Result<String, CoreError> {
        tokio::fs::read_to_string(path).await.map_err(CoreError::Io)
    }

    async fn git_fetch(&self, remote: &str, branch: &str) -> Result<(), CoreError> {
        git(&["fetch", remote, branch]).await?;
        Ok(())
    }

    async fn current_branch(&self) -> Result<String, CoreError> {
        git(&["rev-parse", "--abbrev-ref", "HEAD"]).await
    }

    async fn is_working_tree_clean(&self) -> Result<bool, CoreError> {
        let output = git(&["status", "--porcelain"]).await?;
        Ok(output.is_empty())
    }

    async fn local_matches_remote(&self, _branch: &str) -> Result<bool, CoreError> {
        let local = git(&["rev-parse", "main"]).await?;
        let remote = git(&["rev-parse", "origin/main"]).await?;
        Ok(local == remote)
    }

    async fn branch_exists(&self, name: &str) -> Result<bool, CoreError> {
        let refspec = format!("refs/heads/{name}");
        let output = tokio::process::Command::new("git")
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
        git(&["switch", "-c", name]).await?;
        Ok(())
    }

    async fn switch_branch(&self, name: &str) -> Result<(), CoreError> {
        git(&["switch", name]).await?;
        Ok(())
    }

    async fn head_sha(&self) -> Result<String, CoreError> {
        git(&["rev-parse", "HEAD"]).await
    }

    async fn commits_ahead(&self, base_sha: &str) -> Result<u32, CoreError> {
        let range = format!("{base_sha}..HEAD");
        let raw = git(&["rev-list", "--count", &range])
            .await
            .unwrap_or_default();
        Ok(raw.trim().parse::<u32>().unwrap_or(0))
    }

    async fn run_plan_cycle(
        &self,
        workspace: &str,
        plan_path: &str,
        profile: &str,
    ) -> Result<i32, CoreError> {
        let status = tokio::process::Command::new("bun")
            .args([
                "run",
                "--cwd",
                &self.ai_coding_monorepo,
                "pipeline",
                "plan-cycle",
                workspace,
                "--plan",
                plan_path,
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

    async fn create_pr(&self, base: &str, title: &str, body: &str) -> Result<String, CoreError> {
        let output = tokio::process::Command::new("gh")
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
