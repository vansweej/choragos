//! Serde type mirroring ai-coding's S0 ledger-line schema (`golden-v1.jsonl`).
//!
//! This is the choragos half of a cross-repo contract test: both repos
//! deserialize the identical vendored fixture, so a schema drift in either
//! direction breaks CI instead of surfacing as a silent runtime mismatch.

use serde::{Deserialize, Serialize};

/// A single JSONL line as written by ai-coding's plan-cycle executor ledger.
///
/// Unknown `kind` values and unknown payload fields are tolerated
/// (forward-compat): `payload` captures whatever extra shape a newer
/// executor might emit as an opaque [`serde_json::Value`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerLine {
    /// Schema version of this ledger line.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// Identifier of the run that produced this line.
    #[serde(rename = "runId")]
    pub run_id: String,
    /// RFC 3339 timestamp of the event.
    pub ts: String,
    /// Event kind (e.g. `"run_started"`, `"run_finished"`). Unknown/future
    /// kinds are accepted as plain strings.
    pub kind: String,
    /// Phase number, if applicable to this event kind.
    #[serde(default)]
    pub phase: Option<u32>,
    /// Step number, if applicable to this event kind.
    #[serde(default)]
    pub step: Option<u32>,
    /// Operation identifier, if applicable to this event kind.
    #[serde(rename = "opId", default)]
    pub op_id: Option<String>,
    /// Catch-all for any additional/forward-compat fields not modeled above.
    #[serde(flatten)]
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::LedgerLine;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/ledger/golden-v1.jsonl");

    #[test]
    fn golden_fixture_lines_all_deserialize() {
        for line in FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
            let parsed: Result<LedgerLine, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "failed to deserialize golden fixture line: {line}\nerror: {:?}",
                parsed.err()
            );
        }
    }

    #[test]
    fn unknown_kind_and_extra_payload_field_still_deserializes() {
        let line = serde_json::json!({
        "schemaVersion": 1,
        "runId": "run-x",
        "ts": "2024-01-01T00:00:00Z",
        "kind": "some_future_event_kind",
        "phase": 2,
        "step": 3,
        "opId": "op-9",
        "extra_future_field": {"nested": true},
        })
        .to_string();

        let parsed: Result<LedgerLine, _> = serde_json::from_str(&line);
        assert!(
            parsed.is_ok(),
            "unknown kind / extra field must not fail deserialization: {:?}",
            parsed.err()
        );
        let decoded = parsed.unwrap();
        assert_eq!(decoded.kind, "some_future_event_kind");
    }
}
