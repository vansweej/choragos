# choragos

**choragos** is a deterministic MCP server and CLI wrapper around the
`ai-coding` plan-cycle executor.  It handles all the git ceremony so the
executor never has to:

- Creates a `feat/<slug>` branch derived from the plan title.
- Runs the `ai-coding` plan-cycle **unchanged** — no patching, no
  interception.
- Opens a pull request **only** when the run is green *and* produced at least
  one commit.
- Appends a structured JSON line to a local run-ledger after every run.
- Sends a Telegram notification (best-effort; failures are swallowed).
- **Never pushes `main`** and never merges anything automatically.

---

## Clean-start precondition

Before creating a branch or running the executor, choragos verifies all three
of the following.  If any check fails the run is aborted immediately with a
Red record and no branch is created.

| Check | Requirement |
|-------|-------------|
| Current branch | Must be `main` |
| Working tree | Must be clean (no uncommitted changes) |
| Local vs remote | `main` must match `origin/main` |

---

## Usage

### MCP tool — `choragos_run_plan`

Register `choragos-mcp-server` as an MCP server in your editor or agent
configuration.  The tool exposes a single operation:

```json
{
  "tool": "choragos_run_plan",
  "arguments": {}
}
```

All arguments are optional:

| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `plan_path` | `string` | `"PLAN.md"` | Path to the plan Markdown file, relative to the workspace root. |
| `profile` | `string` | `CHORAGOS_DEFAULT_PROFILE` | Pipeline profile to pass to the executor. |
| `slug` | `string` | derived from plan title | Override the auto-derived branch slug. |

The tool returns the [`LedgerRecord`](#ledger-schema) as a JSON string.

### CLI — `choragos`

```sh
# Run with all defaults (reads PLAN.md in the current directory)
choragos

# Specify a different plan file and profile
choragos --plan plans/my-feature.md --profile fast

# Override the branch slug
choragos --slug my-custom-slug
```

All flags are optional:

| Flag | Default | Description |
|------|---------|-------------|
| `--plan` | `PLAN.md` | Path to the plan Markdown file. |
| `--profile` | `CHORAGOS_DEFAULT_PROFILE` | Pipeline profile. |
| `--slug` | derived from plan title | Override the branch slug. |

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | Green — run succeeded. |
| `1` | Orange — recoverable failure (max attempts reached). |
| `2` | Red — hard failure or clean-start precondition not met. |

---

## Environment configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `AI_CODING_MONOREPO` | **yes** | — | Absolute path to the `ai-coding` monorepo checkout. |
| `CHORAGOS_DEFAULT_PROFILE` | **yes** | — | Default pipeline profile name (e.g. `"default"`). |
| `CHORAGOS_MAX_ATTEMPTS` | no | `3` | Maximum plan-cycle attempts before giving up with Orange. |
| `TELEGRAM_BOT_TOKEN` | no | — | Telegram bot token for run notifications. |
| `TELEGRAM_CHAT_ID` | no | — | Telegram chat ID for run notifications. |

Both Telegram variables must be set for notifications to be sent; if either is
absent the notification step is silently skipped.

---

## Failure-class rules

| Exit code | Class | Retry? | PR? |
|-----------|-------|--------|-----|
| `0` | **Green** | — | Yes, if commits ahead > 0 |
| `2` | **Orange** | Yes, up to `CHORAGOS_MAX_ATTEMPTS` | No |
| `3` | **Red** | No | No |
| any other | **Red** | No | No |
| `-1` (abort) | **Red** | No | No |

Special override: if the executor exits `0` but leaves the working tree dirty,
the result is overridden to **Red** with reason
`"executor left tree dirty (pipeline invariant violation)"` and no PR is
opened.

---

## Resume-on-existing-branch behaviour

If the feature branch `feat/<slug>` already exists locally when a run starts,
choragos switches to it with `git switch` rather than creating a new branch.
This allows interrupted runs to be resumed without manual cleanup.

---

## Run-ledger

Every completed run (including clean-start aborts) appends one compact JSON
line to the ledger file.

### Location

```
<platform data dir>/choragos/ledger.jsonl
```

| Platform | Typical path |
|----------|-------------|
| Linux | `~/.local/share/choragos/ledger.jsonl` |
| macOS | `~/Library/Application Support/choragos/ledger.jsonl` |

### Schema

| Field | Type | Description |
|-------|------|-------------|
| `plan_id` | `string` | Branch slug used as the plan identifier. |
| `repo` | `string` | Repository name (workspace directory basename). |
| `branch` | `string` | Feature branch name (`feat/<slug>`) or `"main"` on abort. |
| `profile` | `string` | Pipeline profile used for the run. |
| `exit_code` | `integer` | Raw exit code from the executor (`-1` on abort). |
| `attempts` | `integer` | Number of plan-cycle attempts made. |
| `failure_class` | `"green"` \| `"orange"` \| `"red"` | Derived failure classification. |
| `base_sha` | `string` | SHA of `main` at the moment the feature branch was created. |
| `head_sha` | `string` | SHA of `HEAD` on the feature branch after the run finished. |
| `commits_ahead` | `integer` | Commits on the feature branch ahead of `base_sha`. |
| `pr_url` | `string \| null` | Pull-request URL, or `null` when no PR was opened. |
| `reason` | `string \| null` | Human-readable explanation when no PR was opened or the run failed. |
| `started_at` | `string` | RFC 3339 timestamp recorded at run start. |
| `finished_at` | `string` | RFC 3339 timestamp recorded at run end. |

### Example record

```json
{
  "plan_id": "choragos-v1",
  "repo": "choragos",
  "branch": "feat/choragos-v1",
  "profile": "default",
  "exit_code": 0,
  "attempts": 1,
  "failure_class": "green",
  "base_sha": "a1b2c3d4",
  "head_sha": "e5f6a7b8",
  "commits_ahead": 3,
  "pr_url": "https://github.com/org/choragos/pull/42",
  "reason": null,
  "started_at": "2024-06-01T12:00:00Z",
  "finished_at": "2024-06-01T12:04:37Z"
}
```

---

## Short usage example

```sh
# 1. Make sure you are on a clean, synced main branch.
git checkout main
git pull --ff-only

# 2. Set required environment variables (add to your shell profile).
export AI_CODING_MONOREPO="$HOME/Projects/ai-coding"
export CHORAGOS_DEFAULT_PROFILE="default"

# 3. Place your plan at PLAN.md (or pass --plan <path>).
#    The first level-1 heading determines the branch slug:
#      # Feature: my cool feature
#    → branch: feat/my-cool-feature

# 4. Run choragos.
choragos

# On success the LedgerRecord is printed as pretty JSON and a PR is opened
# if commits were produced.  The CLI exits 0 (Green), 1 (Orange), or 2 (Red).
```