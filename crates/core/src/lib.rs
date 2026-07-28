//! choragos-core — deterministic plan-cycle orchestration logic.
//!
//! This crate holds the pure/testable core of choragos: failure-class
//! mapping, the run-ledger, plan-title parsing, config resolution, the
//! `CommandRunner` I/O seam, and the orchestrator itself. The MCP server and
//! CLI binaries are thin adapters over this crate.

pub mod failure;
pub use failure::FailureClass;

pub mod error;
pub use error::CoreError;

pub mod config;
pub use config::Config;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert!(true);
    }
}
