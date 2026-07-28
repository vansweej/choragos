# Feature: choragos v1 — deterministic plan-cycle orchestrator (MCP + CLI)

Context for the human (ignored by the parser): this plan uses EXACTLY ONE step per
phase. Each step is a single self-contained patch that creates that phase's whole module
(with its tests) and does all its own wiring (Cargo.toml dependency lines and lib.rs
module declarations). No two steps ever edit the same file, and phases are
dependency-ordered so every referenced type already exists on disk before it is used.

## Phase 1: FailureClass and exit-code mapping

Commit message: feat(core): add FailureClass and exit-code mapping

### Step 1: Add FailureClass with tests and wiring

Make all of the following changes in this one step. Edit crates/core/Cargo.toml to add,
under [dependencies], `serde = { workspace = true }` and `serde_json = { workspace = true }`.
Create the file crates/core/src/failure.rs containing: a public enum FailureClass with
variants Green, Orange, Red, deriving Debug, Clone, Copy, PartialEq, Eq, serde::Serialize
and serde::Deserialize with #[serde(rename_all = "lowercase")] and a doc comment; an
associated function `pub fn from_exit_code(code: i32) -> FailureClass` mapping 0 => Green,
2 => Orange, 3 => Red and any other value => Red; an implementation of std::fmt::Display
rendering "green", "orange", "red"; and a #[cfg(test)] module asserting from_exit_code for
0, 2, 3 and 99 and the Display output for each variant. Edit crates/core/src/lib.rs to add
the lines `pub mod failure;` and `pub use failure::FailureClass;` while keeping the
existing file contents.

## Phase 2: CoreError type

Commit message: feat(core): add CoreError

### Step 1: Add CoreError with tests and wiring

Make all of the following changes in this one step. Edit crates/core/Cargo.toml to add,
under [dependencies], `thiserror = { workspace = true }`. Create the file
crates/core/src/error.rs containing a public enum CoreError using thiserror::Error with
variants MissingEnv(String), Io(#[from] std::io::Error), Json(#[from] serde_json::Error),
Command { context: String, message: String } and Message(String), each with a descriptive
#[error("...")] attribute, plus a #[cfg(test)] module that constructs a
CoreError::MissingEnv, a CoreError::Command and a CoreError::Message and asserts each one's
Display string is non-empty. Edit crates/core/src/lib.rs to add the lines `pub mod error;`
and `pub use error::CoreError;` while keeping the existing file contents.

## Phase 3: Config with injectable env resolution

Commit message: feat(core): add Config

### Step 1: Add Config with tests and wiring

Make all of the following changes in this one step. Create the file
crates/core/src/config.rs containing a public struct Config with fields
ai_coding_monorepo: String, default_profile: String, max_attempts: u32,
telegram_bot_token: Option<String> and telegram_chat_id: Option<String>; a function
`pub fn from_getter<F: Fn(&str) -> Option<String>>(get: F) -> Result<Config,
crate::CoreError>` where AI_CODING_MONOREPO and CHORAGOS_DEFAULT_PROFILE are required and a
missing one returns crate::CoreError::MissingEnv, CHORAGOS_MAX_ATTEMPTS is optional and
parsed as u32 defaulting to 3 with a non-numeric value returning crate::CoreError::Message,
and TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID are optional; a thin
`#[cfg(not(tarpaulin_include))] pub fn from_env() -> Result<Config, crate::CoreError>`
delegating to from_getter over a closure calling std::env::var(name).ok(); and a
#[cfg(test)] module driving from_getter with a std::collections::HashMap-backed closure
covering all-present success, a missing AI_CODING_MONOREPO error, a missing
CHORAGOS_DEFAULT_PROFILE error, a non-numeric CHORAGOS_MAX_ATTEMPTS error, and absent
optionals yielding None with max_attempts defaulting to 3. Edit crates/core/src/lib.rs to
add the lines `pub mod config;` and `pub use config::Config;` while keeping the existing
file contents.

## Phase 4: LedgerRecord and JSONL run-ledger

Commit message: feat(core): add LedgerRecord and JSONL run-ledger

### Step 1: Add LedgerRecord with tests and wiring

Make all of the following changes in this one step. Edit crates/core/Cargo.toml to add
`directories = { workspace = true }` under [dependencies] and
`tempfile = { workspace = true }` under [dev-dependencies]. Create the file
crates/core/src/ledger.rs containing: a public struct LedgerRecord with fields
plan_id: String, repo: String, branch: String, profile: String, exit_code: i32,
attempts: u32, failure_class: crate::FailureClass, base_sha: String, head_sha: String,
commits_ahead: u32, pr_url: Option<String>, reason: Option<String>, started_at: String and
finished_at: String, deriving Debug, Clone, serde::Serialize and serde::Deserialize; a
method `pub fn to_jsonl_line(&self) -> Result<String, crate::CoreError>` returning the
compact serde_json serialization with a trailing newline appended; a function
`pub fn default_ledger_path() -> Option<std::path::PathBuf>` using
directories::ProjectDirs::from("", "", "choragos") and returning its data_dir joined with
"ledger.jsonl"; a function `pub fn append_line(path: &std::path::Path, line: &str) ->
Result<(), crate::CoreError>` creating any missing parent directories and appending the
line; and a #[cfg(test)] module using tempfile to write a LedgerRecord via
append_line(to_jsonl_line), read it back, assert the JSON round-trips and the file ends
with a newline, and call default_ledger_path once to confirm it does not panic. Edit
crates/core/src/lib.rs to add the lines `pub mod ledger;` and
`pub use ledger::LedgerRecord;` while keeping the existing file contents.

## Phase 5: Plan title parsing and slug

Commit message: feat(core): derive feat branch slug from plan title

### Step 1: Add plan parser with tests and wiring

Make all of the following changes in this one step. Create the file
crates/core/src/plan.rs containing: `pub fn parse_title(markdown: &str) -> Option<String>`
returning the text of the first level-1 heading (a line starting with "# "), stripping a
leading "Feature:" prefix and surrounding whitespace, and returning None if there is none;
`pub fn slugify(title: &str) -> String` lowercasing the input, replacing every run of
non-alphanumeric characters with a single '-' and trimming leading and trailing '-';
`pub fn branch_name(slug: &str) -> String` returning format!("feat/{slug}"); and a
#[cfg(test)] module asserting parse_title of "# Feature: choragos v1 — MCP!" yields
"choragos v1 — MCP!", slugify of that yields "choragos-v1-mcp", branch_name of
"choragos-v1-mcp" yields "feat/choragos-v1-mcp", and parse_title returns None for markdown
with no level-1 heading. Edit crates/core/src/lib.rs to add the line `pub mod plan;` while
keeping the existing file contents.

## Phase 6: CommandRunner trait, FakeRunner, and telegram render

Commit message: feat(core): add CommandRunner seam and telegram render

### Step 1: Add the CommandRunner seam, FakeRunner, and telegram render

Make all of the following changes in this one step. Create the file
crates/core/src/runner.rs defining a public trait CommandRunner with native async methods,
each returning Result<_, crate::CoreError> unless noted: read_to_string(&self, path: &str)
-> Result<String>; git_fetch(&self, remote: &str, branch: &str) -> Result<()>;
current_branch(&self) -> Result<String>; is_working_tree_clean(&self) -> Result<bool>;
local_matches_remote(&self, branch: &str) -> Result<bool>; branch_exists(&self, name: &str)
-> Result<bool>; create_branch(&self, name: &str) -> Result<()>; switch_branch(&self,
name: &str) -> Result<()>; head_sha(&self) -> Result<String>; commits_ahead(&self,
base_sha: &str) -> Result<u32>; run_plan_cycle(&self, workspace: &str, plan_path: &str,
profile: &str) -> Result<i32>; create_pr(&self, base: &str, title: &str, body: &str) ->
Result<String>; send_telegram(&self, text: &str) -> Result<()>; append_ledger(&self,
record: &crate::LedgerRecord) -> Result<()>. Declare in runner.rs
`#[cfg(any(test, feature = "test-support"))] pub mod fake;` and create the file
crates/core/src/runner/fake.rs holding a FakeRunner struct with std::sync::Mutex fields for
a std::collections::VecDeque<i32> of scripted run_plan_cycle exit codes, scripted plan-file
contents, booleans for current-branch-is-main / working-tree-clean / local-matches-remote /
branch-exists, a scripted head_sha String and commits_ahead u32, and recording Vecs for
sent telegram messages, appended ledger records and created-or-switched branch names, with
create_pr returning a fixed URL such as "https://github.com/x/y/pull/1", a Default impl,
builder-style setters, a full CommandRunner impl, and one test asserting a scripted exit
code is returned by run_plan_cycle. Create the file crates/core/src/telegram.rs with
`pub fn render(record: &crate::LedgerRecord) -> String` producing a one-message summary: a
green, orange or red circle emoji chosen from record.failure_class, followed by the repo,
branch, plan_id, attempts and commits_ahead, then "PR: <url>" when pr_url is Some otherwise
"reason: <reason>", plus tests asserting the emoji and PR-versus-reason line for a Green
record with a pr_url, an Orange record and a Red record. Edit crates/core/src/lib.rs to add
the lines `pub mod runner;`, `pub use runner::CommandRunner;` and `pub mod telegram;` while
keeping the existing file contents.

## Phase 7: Orchestrator with clean-start, git-peek, retry, PR, and ledger

Commit message: feat(core): add run orchestrator

### Step 1: Add the orchestrator and its tests

Make all of the following changes in this one step. Edit crates/core/Cargo.toml to add
`chrono = { workspace = true }` under [dependencies]. Create the file
crates/core/src/orchestrator.rs containing a public struct RunInputs with fields
workspace: String, repo: String, plan_path: String, profile: Option<String> and
slug_override: Option<String>, and a function `pub async fn run<R: crate::CommandRunner>(
runner: &R, cfg: &crate::Config, inputs: RunInputs) -> Result<crate::LedgerRecord,
crate::CoreError>` implementing the whole flow. The flow: record started_at via
chrono::Utc::now().to_rfc3339() and resolve the profile from inputs.profile or
cfg.default_profile. Clean-start gate — call git_fetch("origin", "main"); if
current_branch() is not "main", or the working tree is not clean, or
local_matches_remote("main") is false, build a Red LedgerRecord (exit_code -1, attempts 0,
branch "main", empty base_sha and head_sha, commits_ahead 0, pr_url None and a reason
naming the failed check), append it to the ledger, attempt a best-effort telegram render
and send, and return it WITHOUT creating any branch. Otherwise capture base_sha via
head_sha() while still on clean synced main and BEFORE creating any branch; read the plan
via read_to_string(inputs.plan_path); derive the title via crate::plan::parse_title
(mapping an absent title to crate::CoreError::Message); compute the slug from
inputs.slug_override or crate::plan::slugify(title); form the branch via
crate::plan::branch_name(&slug); if branch_exists(&branch) call switch_branch(&branch)
otherwise create_branch(&branch). Retry loop for attempt in 1..=cfg.max_attempts: set code
to run_plan_cycle(&inputs.workspace, &inputs.plan_path, &profile), record attempts as
attempt, break when code is 0 or 3, and otherwise (code 2) continue. After the loop capture
head_sha_final via head_sha() and commits_ahead via commits_ahead(&base_sha). Post-run
invariant: if code is 0 and is_working_tree_clean() is false, override the class to Red with
reason "executor left tree dirty (pipeline invariant violation)" and pr_url None; otherwise
set the class from crate::FailureClass::from_exit_code(code) with reason None when Green and
a short string otherwise. PR decision: only when Green and commits_ahead is greater than 0
call create_pr("main", &title, &body) storing the result in pr_url; when Green with
commits_ahead 0 leave pr_url None and set reason "no changes to land"; for non-green leave
pr_url None. Finalize: set finished_at to the current RFC3339 time, build the LedgerRecord
including base_sha, head_sha_final and commits_ahead, call append_ledger(&record), then call
send_telegram(render(&record)) best-effort by logging and swallowing any error and never
propagating it, and return the record. Also add a #[cfg(test)] module driving run() with the
FakeRunner from crate::runner::fake covering: a dirty tree yields Red and creates no branch;
being off main yields Red; main behind remote yields Red; a green first attempt with
commits_ahead greater than 0 gives attempts 1 and a Some pr_url; green with commits_ahead 0
gives Green with pr_url None and reason "no changes to land"; green with a dirty post-run
tree gives the Red override and no PR; three exit-2 attempts at max 3 give Orange with no
PR; exit 2 then exit 0 gives Green with attempts 2; exit 3 gives Red with attempts 1; and an
existing branch causes a switch rather than a create — asserting in every non-abort path
that append_ledger was called exactly once, send_telegram was attempted exactly once, and
base_sha was captured before the branch was created. Edit crates/core/src/lib.rs to add the
line `pub mod orchestrator;` while keeping the existing file contents.

## Phase 8: RealRunner backed by git, gh, bun, and reqwest

Commit message: feat(core): add RealRunner IO adapter

Coverage: skip

### Step 1: Add RealRunner and wiring

Make all of the following changes in this one step. Edit crates/core/Cargo.toml to add
`tokio = { workspace = true }` and `reqwest = { workspace = true }` under [dependencies].
Create the file crates/core/src/real_runner.rs with a struct RealRunner holding
ai_coding_monorepo: String, telegram_bot_token: Option<String> and telegram_chat_id:
Option<String>, and a complete impl of crate::CommandRunner annotated
#[cfg(not(tarpaulin_include))]: current_branch via `git rev-parse --abbrev-ref HEAD`;
is_working_tree_clean by checking that `git status --porcelain` output is empty; git_fetch
via `git fetch <remote> <branch>`; local_matches_remote by comparing `git rev-parse main`
with `git rev-parse origin/main` after a fetch; branch_exists via `git rev-parse --verify
--quiet refs/heads/<name>`; create_branch via `git switch -c <name>`; switch_branch via
`git switch <name>`; head_sha via a trimmed `git rev-parse HEAD`; commits_ahead via
`git rev-list --count <base_sha>..HEAD` parsed as u32 defaulting to 0 on error;
read_to_string via tokio::fs::read_to_string; run_plan_cycle spawning `bun run --cwd
<ai_coding_monorepo> pipeline plan-cycle <workspace> --plan <plan_path> --profile <profile>
--verbose` with inherited stderr, returning the process exit code and treating a missing
exit code as 3; create_pr running `gh pr create --base <base> --title <title> --body
<body>` and returning the trimmed stdout URL; send_telegram POSTing to
https://api.telegram.org/bot<token>/sendMessage a JSON body of chat_id and text via reqwest,
a no-op when either the token or chat id is None; and append_ledger writing self via
crate::ledger::LedgerRecord::to_jsonl_line and crate::ledger::append_line to
crate::ledger::default_ledger_path, doing nothing if that path cannot be resolved. Map
process and network failures to crate::CoreError::Command. Do not add unit tests. Edit
crates/core/src/lib.rs to add the line
`#[cfg(not(tarpaulin_include))] pub mod real_runner;` and re-export RealRunner, while
keeping the existing file contents.

## Phase 9: Expose choragos_run_plan over rmcp stdio

Commit message: feat(mcp): expose choragos_run_plan MCP tool

Coverage: skip

### Step 1: Implement the MCP server

Make all of the following changes in this one step. Edit crates/mcp-server/Cargo.toml to
add, under [dependencies], choragos-core as `{ path = "../core" }` and tokio, rmcp, serde,
serde_json, schemars and anyhow each as `{ workspace = true }`. Rewrite
crates/mcp-server/src/main.rs following the athenaeum-mcp server pattern at
~/Projects/athenaeum-mcp/crates/mcp-server/src/main.rs: a RunPlanArgs struct with
plan_path, profile and slug all Option<String>, deriving Deserialize and JsonSchema with
doc comments; a ChoragosServer struct holding a tool_router: ToolRouter<Self> and a
choragos_core::Config; the #[tool_router], #[tool(description = "...")] and #[tool_handler]
macros; a choragos_run_plan tool method that builds a choragos_core::RealRunner from the
Config, sets workspace to the current directory and repo to that directory's file name,
defaults plan_path to "PLAN.md", constructs choragos_core::orchestrator::RunInputs, calls
choragos_core::orchestrator::run and returns CallToolResult::success with the
serde_json-serialized LedgerRecord as text; a get_info advertising enable_tools() and
instructions; and a main() marked #[cfg(not(tarpaulin_include))] and #[tokio::main] that
builds the Config via choragos_core::Config::from_env(), serves over
rmcp::transport::stdio() and awaits waiting().

## Phase 10: choragos CLI mirroring the MCP tool

Commit message: feat(cli): add choragos CLI

Coverage: skip

### Step 1: Implement the CLI

Make all of the following changes in this one step. Edit crates/cli/Cargo.toml to add,
under [dependencies], choragos-core as `{ path = "../core" }` and tokio, clap, serde_json
and anyhow each as `{ workspace = true }`. Rewrite crates/cli/src/main.rs with a clap derive
Parser exposing --plan defaulting to "PLAN.md", --profile optional and --slug optional, and
a main() marked #[cfg(not(tarpaulin_include))] and #[tokio::main] that builds the Config via
choragos_core::Config::from_env(), constructs a choragos_core::RealRunner and
choragos_core::orchestrator::RunInputs with workspace set to the current directory and repo
to its file name, calls choragos_core::orchestrator::run, prints the LedgerRecord as pretty
JSON, and exits with 0 for Green, 1 for Orange and 2 for Red.

## Phase 11: Document choragos v1 usage, config, and the run-ledger

Commit message: docs: document choragos v1 usage and run-ledger

Coverage: skip

### Step 1: Rewrite the README

Rewrite README.md at the repo root to describe: what choragos is (a deterministic MCP and
CLI wrapper that branches feat/<slug>, runs the ai-coding plan-cycle UNCHANGED, opens a PR
only on a green run that produced commits, appends a run-ledger, sends a Telegram
notification and never pushes main); the clean-start precondition; the choragos_run_plan
MCP tool and the choragos CLI, both with zero required arguments; the environment
configuration keys AI_CODING_MONOREPO, CHORAGOS_DEFAULT_PROFILE, CHORAGOS_MAX_ATTEMPTS
defaulting to 3, TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID; the ledger location and its schema
including base_sha, head_sha and commits_ahead; the failure-class rules; the
resume-on-existing-branch behaviour; and a short usage example.
