# micos Minimal Harness Roadmap

This document is the working roadmap for the next `micos` releases. Use it as the default planning source for upcoming implementation work.

## Current Position

`micos` is now a usable local coding harness prototype. It has moved past a simple chat REPL and already owns several runtime responsibilities:

- model loop: OpenAI Responses, Chat Completions, DeepSeek reasoning replay, tool-call loop
- local tools: file and shell tools, tool result projection, full session event retention
- permissions: `safe`, `ask`, `auto`, project rules, TUI and console switching
- TUI: streaming output, tool cards, permission approval, slash popup, model picker, resume picker
- prompt governance: base prompt sections, runtime sections, project memory, active plan
- context governance: context snapshots, manual compact, summary validation, recent tail retention
- persistence: session JSONL, resume, project memory, active plan, handoff
- observability: `/status`, `/prompt`, `/context`, `/trace`, `/summary`, `/transcript`

The current project is around 70% of a minimal harness framework. The remaining work is mainly about stability, recovery, verification, and memory lifecycle.

## Definition of Minimal Harness

For this project, a minimal harness means the runtime clearly owns these responsibilities:

- control flow: turn loop, stop reasons, max steps, interrupt handling
- permissions: policy decision, user approval, persistent rules, audit trail
- tool execution: typed tools, safe path handling, bounded outputs, error classification
- context: prompt assembly, context accounting, compaction, resume view construction
- persistence: session logs, handoff, active plan, project memory
- verification: project-aware checks and honest reporting of skipped or failed validation
- operator UX: inspectable state, visible tool activity, recoverable failures

The goal is not to match Claude Code feature count. The goal is a small local runtime where each of those responsibilities has a concrete module, command, event, and test surface.

## Immediate Rule

Before adding new large capabilities, finish the current 0.4.2 working tree:

- run full unit tests
- run a no-network smoke path for core slash commands
- commit and push the current TUI, permission, handoff, resume, write policy, and compact fixes
- update this document if implementation changes the release boundary

## 0.4.3 Stabilization Release

Goal: make the current feature set reliable enough for daily local use.

Scope:

- compact retry and repair:
  - if summary validation fails, ask the model once to repair the summary using the validation error
  - normalize valid heading variants before writing session state
  - keep original transcript untouched after repeated failure
- TUI interaction stabilization:
  - verify slash popup behavior for command completion and argument candidate lists
  - keep `Tab` as completion only and `Enter` as execution
  - keep `/resume` and `/permission` second-stage candidates navigable
- permission UX:
  - ensure `/permission` status and mode updates behave the same in TUI and console
  - record permission mode changes in session JSONL
  - document default write policy clearly
- handoff UX:
  - ensure `/exit` and Ctrl-C wait for handoff
  - keep handoff running/success/failed visible like tool calls
- smoke coverage:
  - no-TUI command script covering `/status`, `/context`, `/permission`, `/compact`, `/summary`, `/exit`
  - TUI unit tests for slash popup and permission candidates

Acceptance:

- `cargo test` passes
- manual compact no longer fails on harmless Markdown heading variation
- failed compact leaves the runtime transcript unchanged
- changing permission mode persists to `.micos/config.toml`
- `/exit` writes handoff before stopping

## 0.4.4 Tool and Verification Release

Goal: make coding work safer and easier to validate.

Scope:

- edit workflow:
  - add a patch-oriented edit tool or structured edit path
  - keep raw write support but prefer scoped edits for existing files
  - reject symlink and path escape cases consistently
- tool output governance:
  - externalize or truncate large outputs with stable previews
  - keep full output in session logs or artifact files
  - show output preview size and truncation status in trace
- tool error taxonomy:
  - classify permission denial, command failure, timeout, path error, schema error, and provider error
  - map each class to stop reason or recovery behavior
- verification profile:
  - add `.micos/verify.toml` or config section for project checks
  - support common commands such as `cargo test`, `cargo fmt --check`, `cargo build`
  - expose `/verify` to run configured checks
- reporting discipline:
  - final response should draw verification status from recorded checks when possible

Acceptance:

- large command output does not flood model-visible context
- failing verification is recorded with command, exit status, and short output preview
- `/verify` can run this repo's default Rust checks
- tool failures produce actionable trace lines

## 0.4.5 Memory and Context Lifecycle Release

Goal: close the loop between session work, durable memory, and future context selection.

Scope:

- memory promotion:
  - add candidate memory records derived from session events or handoff
  - require explicit user acceptance before durable memory enters startup context
  - store source session, timestamp, scope, and last validation state
- memory commands:
  - `/memory candidates`
  - `/memory promote <id>`
  - `/memory stale <id>`
  - `/memory forget <id>`
- context selection:
  - introduce categories: always include, active task, retrieved topic, compact summary, recent tail
  - show category token estimates in `/context`
  - avoid automatically injecting detailed topic files unless selected
- auto compact foundation:
  - add threshold config
  - dry-run context pressure warning before enabling automatic compaction
  - keep manual `/compact` as the primary path until retry behavior is proven

Acceptance:

- fresh sessions load only accepted durable memory plus active plan
- promoted memory has source and staleness metadata
- `/context` shows memory, active plan, compact summary, recent tail, and tool preview categories
- auto compact can be configured in warning-only mode

## 0.4.6 Recovery and Eval Release

Goal: make failures reproducible and regressions visible.

Scope:

- recovery reports:
  - add `/recover` for the last failed or interrupted turn
  - include attempted action, failure class, files touched, commands run, and next safe step
- checkpoints:
  - record dirty worktree summary before mutating tools when available
  - record changed files after tool execution
  - do not implement destructive rollback in this milestone
- local eval fixtures:
  - read-only question
  - small edit
  - denied tool
  - failing test recovery
  - compact and resume
- eval command:
  - `/eval` or `micos eval` for deterministic harness behavior tests
  - report pass/fail, stop reason, tool count, and trace path

Acceptance:

- interrupted or failed turns produce a useful recovery report
- deterministic evals catch regressions in permission, compact, resume, and stop handling
- trace output can explain why a turn stopped without replaying the full session manually

## Deferred Work

These areas matter, but they should wait until 0.4.3 through 0.4.6 are stable:

- MCP client integration
- hooks
- multi-agent delegation
- worktree isolation for mutating subagents
- cost accounting
- browser or GUI automation
- remote session sync

## Non-Goals for 0.4.x

- replacing the base prompt with a user-provided override
- automatic memory promotion without user review
- destructive rollback commands
- broad plugin ecosystem
- matching Claude Code's full UX surface

## Operating Principles

- Keep releases small and testable.
- Prefer deterministic runtime behavior over prompt-only fixes.
- Preserve original session JSONL even when model-visible context is compacted.
- Keep project-local durable state under `.micos/`.
- Treat failure paths as normal runtime paths with typed events and visible UI state.
- Avoid expanding context automatically unless the source, size, and reason are inspectable.

## Next Implementation Queue

Start here for the next coding session:

1. Finish and commit the current 0.4.2 working tree.
2. Add compact repair retry and tests.
3. Add no-TUI smoke script for the core slash commands.
4. Run manual TUI smoke for `/permission`, `/resume`, `/compact`, and `/exit`.
5. Cut 0.4.3 after stabilization passes.
