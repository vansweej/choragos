//! Telegram notification rendering for choragos run records.

use crate::{FailureClass, LedgerRecord};

/// Renders a [`LedgerRecord`] as a single Telegram message.
///
/// The message contains:
/// - A coloured circle emoji reflecting the [`FailureClass`].
/// - Key run metadata: repo, branch, plan ID, attempts, and commits ahead.
/// - Either `"PR: <url>"` when a pull request was opened, or
///   `"reason: <reason>"` otherwise.
pub fn render(record: &LedgerRecord) -> String {
    let emoji = match record.failure_class {
        FailureClass::Green => "🟢",
        FailureClass::Orange => "🟠",
        FailureClass::Red => "🔴",
    };

    let outcome_line = match &record.pr_url {
        Some(url) => format!("PR: {url}"),
        None => {
            let reason = record
                .reason
                .as_deref()
                .unwrap_or("(no reason given)");
            format!("reason: {reason}")
        }
    };

    format!(
        "{emoji} {repo} | {branch} | plan: {plan_id} | attempts: {attempts} | ahead: {ahead}\n{outcome_line}",
        emoji = emoji,
        repo = record.repo,
        branch = record.branch,
        plan_id = record.plan_id,
        attempts = record.attempts,
        ahead = record.commits_ahead,
        outcome_line = outcome_line,
    )
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::{FailureClass, LedgerRecord};

    fn base_record(failure_class: FailureClass) -> LedgerRecord {
        LedgerRecord {
            plan_id: "choragos-v1".to_string(),
            repo: "choragos".to_string(),
            branch: "feat/choragos-v1".to_string(),
            profile: "default".to_string(),
            exit_code: 0,
            attempts: 1,
            failure_class,
            base_sha: "abc".to_string(),
            head_sha: "def".to_string(),
            commits_ahead: 3,
            pr_url: None,
            reason: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            finished_at: "2024-01-01T00:01:00Z".to_string(),
        }
    }

    #[test]
    fn green_with_pr_url_uses_green_emoji_and_pr_line() {
        let mut record = base_record(FailureClass::Green);
        record.pr_url = Some("https://github.com/x/y/pull/42".to_string());
        let msg = render(&record);
        assert!(msg.starts_with("🟢"), "expected green emoji, got: {msg}");
        assert!(msg.contains("PR: https://github.com/x/y/pull/42"), "expected PR line, got: {msg}");
    }

    #[test]
    fn orange_record_uses_orange_emoji_and_reason_line() {
        let mut record = base_record(FailureClass::Orange);
        record.exit_code = 2;
        record.reason = Some("max attempts reached".to_string());
        let msg = render(&record);
        assert!(msg.starts_with("🟠"), "expected orange emoji, got: {msg}");
        assert!(msg.contains("reason: max attempts reached"), "expected reason line, got: {msg}");
    }

    #[test]
    fn red_record_uses_red_emoji_and_reason_line() {
        let mut record = base_record(FailureClass::Red);
        record.exit_code = 3;
        record.reason = Some("hard failure".to_string());
        let msg = render(&record);
        assert!(msg.starts_with("🔴"), "expected red emoji, got: {msg}");
        assert!(msg.contains("reason: hard failure"), "expected reason line, got: {msg}");
    }
}