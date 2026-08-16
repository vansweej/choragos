//! JSONL run-ledger: record type, serialisation, and append helper.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Current schema version written by this build of choragos.
///
/// Bump this whenever [`LedgerRecord`]'s shape changes in a
/// backwards-compatible way (new optional field). Old ledger lines lacking
/// `schema_version` are treated as version 1 (see
/// [`default_schema_version`]).
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Default for [`LedgerRecord::schema_version`] when deserialising a line
/// written before the field existed (schema version 1).
fn default_schema_version() -> u32 {
    1
}

/// A single entry in the choragos run-ledger.
///
/// Every completed orchestrator run (including clean-start aborts) appends
/// one [`LedgerRecord`] as a compact JSON line to the ledger file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LedgerRecord {
    /// Opaque identifier for this run (distinct from `plan_id`).
    pub run_id: String,
    /// Opaque identifier for the plan (typically the branch slug).
    pub plan_id: String,
    /// Repository name (directory basename of the workspace).
    pub repo: String,
    /// Feature branch name used for this run.
    pub branch: String,
    /// Pipeline profile that was passed to the plan-cycle executor.
    pub profile: String,
    /// Raw process exit code returned by the plan-cycle executor.
    pub exit_code: i32,
    /// Number of plan-cycle attempts made in this run.
    pub attempts: u32,
    /// Derived failure classification.
    pub failure_class: crate::FailureClass,
    /// SHA of `main` at the moment the feature branch was created.
    pub base_sha: String,
    /// SHA of `HEAD` on the feature branch after the run finished.
    pub head_sha: String,
    /// Number of commits on the feature branch ahead of `base_sha`.
    pub commits_ahead: u32,
    /// URL of the pull request opened on a green run, if any.
    pub pr_url: Option<String>,
    /// Human-readable explanation when no PR was opened or the run failed.
    pub reason: Option<String>,
    /// RFC 3339 timestamp recorded at the start of the run.
    pub started_at: String,
    /// RFC 3339 timestamp recorded at the end of the run.
    pub finished_at: String,
    /// Ledger record schema version. Lines written before this field
    /// existed are treated as version 1.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Opaque identifier correlating this record with a multi-repo change
    /// (Phase 5). `None` for single-repo runs.
    #[serde(default)]
    pub change_id: Option<String>,
}

impl LedgerRecord {
    /// Serialises this record as a compact JSON line with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::CoreError::Json`] if serialisation fails.
    pub fn to_jsonl_line(&self) -> Result<String, crate::CoreError> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

/// Returns the default path for the run-ledger file, or `None` if the
/// platform data directory cannot be resolved.
///
/// The path is `<data_dir>/choragos/ledger.jsonl` where `<data_dir>` is
/// determined by [`directories::ProjectDirs`].
pub fn default_ledger_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "choragos")
        .map(|dirs| dirs.data_dir().join("ledger.jsonl"))
}

/// Appends `line` to the file at `path`, creating any missing parent
/// directories first.
///
/// # Errors
///
/// Returns [`crate::CoreError::Io`] if directory creation or the file write
/// fails.
pub fn append_line(path: &Path, line: &str) -> Result<(), crate::CoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{append_line, default_ledger_path, LedgerRecord, CURRENT_SCHEMA_VERSION};
    use crate::FailureClass;

    fn sample_record() -> LedgerRecord {
        LedgerRecord {
            run_id: "run-choragos-v1-1".to_string(),
            plan_id: "choragos-v1".to_string(),
            repo: "choragos".to_string(),
            branch: "feat/choragos-v1".to_string(),
            profile: "default".to_string(),
            exit_code: 0,
            attempts: 1,
            failure_class: FailureClass::Green,
            base_sha: "abc123".to_string(),
            head_sha: "def456".to_string(),
            commits_ahead: 3,
            pr_url: Some("https://github.com/x/y/pull/42".to_string()),
            reason: None,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            finished_at: "2024-01-01T00:01:00Z".to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            change_id: None,
        }
    }

    #[test]
    fn round_trip_via_append_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sub").join("ledger.jsonl");

        let record = sample_record();
        let line = record.to_jsonl_line().expect("to_jsonl_line");

        // The line must end with a newline.
        assert!(line.ends_with('\n'), "line should end with newline");

        append_line(&path, &line).expect("append_line");

        let contents = std::fs::read_to_string(&path).expect("read_to_string");
        assert!(contents.ends_with('\n'), "file should end with newline");

        // Round-trip: deserialise the first (and only) line.
        let first_line = contents.lines().next().expect("at least one line");
        let decoded: LedgerRecord =
            serde_json::from_str(first_line).expect("deserialise LedgerRecord");

        assert_eq!(decoded.plan_id, record.plan_id);
        assert_eq!(decoded.repo, record.repo);
        assert_eq!(decoded.branch, record.branch);
        assert_eq!(decoded.profile, record.profile);
        assert_eq!(decoded.exit_code, record.exit_code);
        assert_eq!(decoded.attempts, record.attempts);
        assert_eq!(decoded.failure_class, record.failure_class);
        assert_eq!(decoded.base_sha, record.base_sha);
        assert_eq!(decoded.head_sha, record.head_sha);
        assert_eq!(decoded.commits_ahead, record.commits_ahead);
        assert_eq!(decoded.pr_url, record.pr_url);
        assert_eq!(decoded.reason, record.reason);
        assert_eq!(decoded.started_at, record.started_at);
        assert_eq!(decoded.finished_at, record.finished_at);
        assert_eq!(decoded.schema_version, record.schema_version);
        assert_eq!(decoded.change_id, record.change_id);
    }

    #[test]
    fn v1_line_missing_new_fields_defaults_schema_version_and_change_id() {
        // A line written before schema_version/change_id existed.
        let v1_line = serde_json::json!({
            "plan_id": "choragos-v1",
            "run_id": "run-choragos-v1-1",
            "repo": "choragos",
            "branch": "feat/choragos-v1",
            "profile": "default",
            "exit_code": 0,
            "attempts": 1,
            "failure_class": "green",
            "base_sha": "abc123",
            "head_sha": "def456",
            "commits_ahead": 3,
            "pr_url": null,
            "reason": null,
            "started_at": "2024-01-01T00:00:00Z",
            "finished_at": "2024-01-01T00:01:00Z",
        })
        .to_string();

        let decoded: LedgerRecord =
            serde_json::from_str(&v1_line).expect("deserialise v1 LedgerRecord");

        assert_eq!(decoded.schema_version, 1);
        assert_eq!(decoded.change_id, None);
    }

    #[test]
    fn default_ledger_path_does_not_panic() {
        // We only assert it does not panic; the result may be None on some CI
        // environments where no home directory is available.
        let _ = default_ledger_path();
    }
}
