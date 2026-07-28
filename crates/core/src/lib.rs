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

pub mod ledger;
pub use ledger::LedgerRecord;

pub mod plan;

pub mod runner;
pub use runner::CommandRunner;

pub mod telegram;

pub mod orchestrator;

#[cfg(not(tarpaulin_include))]
pub mod real_runner;
#[cfg(not(tarpaulin_include))]
pub use real_runner::RealRunner;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert!(true);
    }
}
