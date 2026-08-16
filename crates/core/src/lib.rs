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

pub mod cerebrum;
pub use cerebrum::CerebrumClient;

pub mod ledger;
pub use ledger::LedgerRecord;

pub mod plan;

pub mod ledger_line;
pub use ledger_line::LedgerLine;

pub mod runner;
pub use runner::{CommandRunner, Memory, Pipeline, Sink, Vcs};

pub mod telegram;

pub mod orchestrator;

pub mod change;
pub use change::{ChangeManifest, RepoJob};

#[cfg(not(tarpaulin_include))]
pub mod real_runner;
#[cfg(not(tarpaulin_include))]
pub use real_runner::RealRunner;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(1 + 1, 2);
    }
}
