# Feature: Phase 3 CLI/MCP UX — plan-ref naming

## Phase 1: Rename the CLI --plan flag to --plan-ref

Commit message: refactor(cli): rename --plan to --plan-ref, require it unless --change-ref, add clap parse tests

Coverage: skip

### Step 1: Rename the flag, fix the reciprocal conflict, drop the PLAN.md default, add parse tests

In crates/cli/src/main.rs make all of the following edits in this one file:
1. Rename the clap field `plan: Option<String>` to `plan_ref: Option<String>` and change its attribute to `#[arg(long = "plan-ref", conflicts_with = "change_ref", required_unless_present = "change_ref")]`. Rewrite its doc comment to say it is a reference to a plan stored in cerebrum (a `plan:<id>` scope id), required unless `--change-ref` is given; remove all mention of the `"PLAN.md"` default and "backward compatibility".
2. CRITICAL: update the `change_ref` field's attribute from `#[arg(long, conflicts_with = "plan")]` to `#[arg(long, conflicts_with = "plan_ref")]`, because clap's `conflicts_with` references the arg id, which changed with the rename. Also update its doc comment "Mutually exclusive with `--plan`." to "Mutually exclusive with `--plan-ref`."
3. In the single-repo branch, replace `let plan_ref = args.plan.unwrap_or_else(|| "PLAN.md".to_string());` with `let plan_ref = args.plan_ref.expect("clap guarantees --plan-ref is present unless --change-ref");` (this line is only reached after the `--change-ref` branch returns, so `required_unless_present` guarantees `Some`).
4. Update the module-level `//!` doc comment to describe `--plan-ref` instead of `--plan` and remove any reference to a `PLAN.md` default.
5. Add a `#[cfg(test)] mod tests` at the end of the file with: `use super::Args; use clap::{CommandFactory, Parser};` and three tests — (a) `cli_definition_is_valid` calling `Args::command().debug_assert();` (builds the command and runs clap's internal assertions, incl. that every `conflicts_with` references a real arg id, catching a dangling reciprocal conflict after the rename); (b) `requires_plan_ref_or_change_ref` asserting `Args::try_parse_from(["choragos"]).is_err()`; (c) `plan_ref_and_change_ref_are_mutually_exclusive` asserting `Args::try_parse_from(["choragos", "--plan-ref", "p", "--change-ref", "c"]).is_err()`.

## Phase 2: Rename the MCP plan_path argument to plan_ref

Commit message: refactor(mcp): rename plan_path tool arg to plan_ref, drop PLAN.md default

Coverage: skip

### Step 1: Rename the RunPlanArgs field and require a plan reference

In crates/mcp-server/src/main.rs make all of the following edits in this one file:
1. Rename the `RunPlanArgs` field `plan_path: Option<String>` to `plan_ref: Option<String>` and rewrite its schemars doc comment to describe it as a reference to a plan stored in cerebrum (a `plan:<id>` scope id), mutually exclusive with `change_ref`; remove the `"PLAN.md"` default text.
2. Update the `change_ref` field's doc comment "Mutually exclusive with `plan_path`." to "Mutually exclusive with `plan_ref`."
3. In `choragos_run_plan`, update the mutual-exclusion guard to use `args.plan_ref` and change its error message string from "plan_path and change_ref are mutually exclusive" to "plan_ref and change_ref are mutually exclusive".
4. Replace `let plan_path = args.plan_path.unwrap_or_else(|| "PLAN.md".to_string());` with logic that takes `args.plan_ref` and, when it is `None`, returns `rmcp::ErrorData::invalid_params("plan_ref or change_ref is required".to_string(), None)`. Use the resolved value when constructing `RunInputs` (the `RunInputs.plan_ref` field name is already correct and does not change).

## Phase 3: Documentation

Commit message: docs: document plan-ref naming and the memorize-to-persist requirement

### Step 1: Update the README argument tables, examples, and add the memorize note

In README.md make all of the following edits, and DO NOT change any occurrence of `plan_ref` that refers to the `RunInputs` field / session prose or to the multi-repo change-manifest keys — only the CLI flag and MCP tool argument references change:
1. In the MCP tool argument table, rename the `plan_path` row to `plan_ref`, remove its `"PLAN.md"` default (default column becomes "—"), and change its description to "Reference to a plan stored in cerebrum (a `plan:<id>` scope id). Mutually exclusive with `change_ref`." Update the `change_ref` row's "Mutually exclusive with `plan_path`." to "`plan_ref`".
2. In the CLI flag table, rename the `--plan` row to `--plan-ref`, remove its `PLAN.md` default (default becomes "—"), and change its description to "Reference to a plan stored in cerebrum. Required unless `--change-ref`." Update the `--change-ref` row's "Mutually exclusive with `--plan`." to "`--plan-ref`".
3. In the usage examples, replace `choragos --plan plans/my-feature.md --profile fast` with `choragos --plan-ref <cerebrum-plan-id> --profile fast`, and replace the "Run with all defaults (reads PLAN.md ...)" comment and the "Place your plan at PLAN.md (or pass --plan <path>)" step with text that says the plan is fetched from cerebrum by its `plan:<id>` scope, passed via `--plan-ref`.
4. Add a short note (near the plan-reference docs) stating that a plan must be promoted to cerebrum's Cortex tier via `memorize` before choragos can fetch it — `remember` alone stores only in the ephemeral Synapse tier, which choragos's separately-spawned cerebrum process does not share.
