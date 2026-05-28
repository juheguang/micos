# micos Roadmap

This document is the single source of truth for micos planning. It supersedes
the archived `harness-roadmap.md` and `minimal-harness-roadmap.md` (see
[archive/](./archive/)).

## Current Baseline — v0.4.6

micos is a functioning local coding agent harness. The runtime currently owns:

**Model Loop:**
- OpenAI Responses API and Chat Completions, DeepSeek reasoning replay
- Tool-call loop with typed stop reasons: final answer, max steps, interrupt, tool denial, tool error, API error

**Built-in Tools:**
- `list_files`, `read_file`, `write_file`, `shell` — with path safety, symlink rejection, output truncation
- Tool result projection: model-visible preview separated from full session-log output

**Permission Engine:**
- Three modes: `safe`, `ask`, `auto`
- Policy decision pipeline: deny rules → ask rules → allow rules → permission hint → mode default
- Shell command classification: safe / dangerous / unknown
- Persistent rules in `.micos/config.toml`, session rules, project-approval workflow

**TUI + Console:**
- ratatui-based TUI with streaming output, tool cards, approval popups, slash command menu, model picker, resume session picker
- Non-TTY console fallback with spinner-based tool progress

**Session Persistence:**
- `.micos/sessions/*.jsonl` — 26 event types, append-only
- Resume from session JSONL (reconstruct model-visible transcript from compact boundary or raw events)
- Handoff written on `/exit`, Ctrl-C, compact success, and all non-final stops

**Context Governance:**
- Context snapshots with per-category token estimates
- Manual `/compact` with summary validation against 8 required headings
- One-shot repair on validation failure; original transcript preserved on repeated failure
- Recent tail retention preserving `function_call`/`function_call_output` pairs

**Prompt Management:**
- 7 base sections (identity, task discipline, tool use, permissions, context governance, verification, reporting style)
- Runtime sections: cwd/model/api/permission/context window/date, project memory, active plan, project append

**Project Memory:**
- `.micos/memory/` with `MEMORY.md` index, `topics/`, `candidates/`, `entries/`
- Candidate generation from session handoff (deterministic, not model-driven)
- Promote / stale / forget lifecycle with TOML serialization
- Fresh sessions load active memory index and entries into prompt

**Active Plan:**
- `.micos/plans/active.md` auto-loaded on session start
- Handoff drafts extracted deterministically from session logs: current state, next step, files touched, commands run, verification status, known failures

**Recovery & Verification:**
- `/recover` — recovery report with failure class, touched files, safe next steps
- Git checkpoints before/after mutating tools (lightweight audit, no rollback)
- `/verify` — runs configured checks through the shell tool, records results
- Defaults to `cargo test` when no `.micos/verify.toml` exists and `Cargo.toml` is present

**Eval Suite:**
- `micos eval` — 5 deterministic fixtures: read-only question, small edit, denied tool, failing tool recovery, compact & resume
- All fixtures use mock model client, verifying harness behavior without network

**Observability:**
- `/status`, `/prompt`, `/context`, `/trace`, `/summary`, `/transcript` slash commands
- Session events carry timestamps and structured metadata

## Design Principles

1. **The model proposes; the harness owns** control flow, permissions, persistence, and verification.
2. **Every failure mode has a name**, a stop reason, and a recovery path.
3. **Context is selected, compacted, and traced**; chat history is not memory.
4. **External capabilities are registered, permissioned, audited, and testable.**
5. **Multi-agent behavior adds isolation or independent verification**, not theatrical role labels.
6. **Human approval is informed, resumable, and low-friction.**
7. **Keep releases small and testable.** Prefer deterministic runtime behavior over prompt-only fixes.
8. **Preserve original session JSONL** even when model-visible context is compacted.
9. **Avoid expanding context automatically** unless the source, size, and reason are inspectable.

## Current Gaps (as of v0.4.6)

The baseline is functional but has clear weak points identified during review:

- **Error handling is stringly-typed.** `ToolExecError` has a single `Other(anyhow::Error)` variant. Tool failures carry no structured classification (timeout vs IO vs parse vs permission), making recovery and model guidance imprecise.
- **Only shell has a timeout.** `read_file`, `write_file`, `list_files` can block indefinitely on slow disks or large directories.
- **Shell output is batch-collected.** stdout/stderr are read only after the process exits. No streaming progress for long-running commands.
- **Only 4 tools.** No `grep`/search, no `edit`/patch, no `glob`/find. The model must use `shell` for these, but shell is restricted in safe mode and error-prone even in auto.
- **Memory is purely deterministic.** Candidate generation extracts facts from handoff fields with no model-driven semantic extraction, no deduplication, no clustering.
- **No automatic compaction.** `context_warning_percent` is configurable but only sets a flag in context snapshots; it never triggers compaction.
- **No hooks system.** No lifecycle events for project-specific rules to hook into.
- **Prompt is minimal.** 7 short behavioral paragraphs with no concrete examples, error recovery patterns, tool selection priorities, or file editing conventions.

## Milestones

### v0.5 — Production Hardening

Target: make the existing feature set robust for regular daily use.

**Structured Error Handling:**
- Replace `ToolExecError::Other(anyhow)` with typed variants: `Timeout`, `Io`, `Parse`, `Permission`, `ProcessExit`, `Utf8`
- Add configurable per-tool timeouts enforced by the harness (not just shell)
- Map error types to recovery hints in `/recover` and model-visible messages
- Classify errors as retryable or terminal at the harness level

**Tool Enrichment:**
- `grep` — search file contents with regex, return file:line matches with context lines, respect `.gitignore`
- `edit` — apply a unified diff or line-range replacement to an existing file, with pre-edit snapshot
- `glob` — recursive file pattern matching (`**/*.rs`, `src/**/*.md`)
- All new tools follow the same permission/audit/truncation path as existing tools

**Shell Improvements:**
- Streaming stdout/stderr capture — emit chunks to UI during execution
- Progress indicators: elapsed time, output bytes accumulated
- Output ring buffer: keep last N KiB in preview, full output in session log

**Auto-Compression:**
- Threshold trigger: when `usage_percent >= context_warning_percent`, warn and offer compaction
- Configurable mode: `off` / `warn` / `auto` (auto still asks before first compact)
- Circuit breaker: abort after N consecutive compaction failures
- Pre-compact snapshot saved to session log before any mutation

**Prompt Professionalization:**
- Add concrete behavior examples to base sections (good vs bad patterns)
- Document error recovery patterns: when a tool fails, what to check before retrying
- Add tool selection priorities: when to use `grep` vs `shell(rg)`, when to use `edit` vs `write_file`
- Include file editing conventions: read before edit, scope edits to the task, don't mix refactors
- Version the prompt and expose version in `/prompt` output

*Acceptance criteria:*
- All tool failures produce a typed error variant, not a generic string
- `grep`, `edit`, and `glob` pass deterministic eval fixtures
- Shell commands stream output to TUI in real time
- Auto-compact warns at threshold and asks before executing
- `/prompt` shows prompt version and per-section token estimates

### v0.6 — Memory and Context Lifecycle

Target: close the loop between session work, durable memory, and context budget.

**Memory Enhancement:**
- Model-driven candidate extraction: after session end (or on `/memory refresh`), ask the model to extract durable facts, decisions, and patterns from the session transcript
- Candidate deduplication: compare new candidates against existing entries; flag near-duplicates
- Auto-staleness: entries not referenced or validated in N sessions are suggested for review
- Topic clustering: group related entries under topic files in `topics/`

**Context Budget Management:**
- Per-category token budgets (system prompt, memory, plan, messages, tool schemas, tool outputs)
- Gradual degradation: when approaching budget, truncate tool output previews first, then compact, then warn
- `/context` shows budget utilization per category with color-coded pressure indicators

**Smarter Compact:**
- Task-aware preservation: always retain the current file being edited, verification commands and results, and the last 3 user requests
- Structured compact output with mandatory sections (already implemented), validated against critical context loss
- Compact preserves tool_call/tool_result pairs correctly (already implemented)

**MEMORY.md Auto-Maintenance:**
- On promote/forget/stale, update the index file to reflect current state
- Index regeneration command for consistency repair
- Topic files carry structured frontmatter (source session, timestamp, scope, last validated)

*Acceptance criteria:*
- `/memory refresh` produces model-extracted candidates (not just handoff field concatenation)
- Promoting a candidate updates both the entry TOML and the MEMORY.md index
- `/context` shows per-category budgets with utilization percentages
- Stale entries are detectable and reviewable

### v0.7 — Extensibility

Target: expose controlled lifecycle points and external tool integration.

**Hooks System:**
- Lifecycle events: `SessionStart`, `BeforeTurn`, `BeforeToolUse`, `AfterToolUse`, `OnError`, `BeforeCompact`, `AfterCompact`, `BeforeStop`
- Hook actions: allow, deny, modify (add context, add warning, suggest permission)
- Project-local config: `.micos/hooks.toml`
- Hook sandboxing: timeout per hook, failure isolation (one hook crash doesn't kill the runtime)
- Hook-added context is size-limited and recorded in session events

**MCP Client:**
- Connect to MCP servers for external tool schemas
- External tools follow the same permission → audit → trace pipeline as built-in tools
- MCP resources and prompts as context sources
- Capability registry: every external tool has a schema, permission class, and audit name

**Multi-Agent Delegation Foundation:**
- Define delegation protocol: task brief, allowed tools, context pack, expected artifact, timeout, success criteria
- Subagent isolation: separate transcript, bounded tool access, independent stop conditions
- Result merge policy: how subagent output integrates into the main session transcript

*Acceptance criteria:*
- Hooks can block or warn on tool calls matching project-specific patterns
- An MCP-provided tool is permissioned and audited identically to a built-in tool
- A delegation turn starts, runs, and merges results without corrupting the main session

## Deferred Work (Non-Goals for v0.5–v0.7)

- Full plugin ecosystem (hooks are the extension point, not a plugin system)
- Remote session sync / collaboration
- GUI or browser automation
- Destructive rollback (git checkpoints remain audit-only)
- Automatic memory promotion without user review
- Claude Code UX parity

## Operating Principles

Before starting feature work each session:
1. Run `cargo test` and all smoke scripts (`smoke-no-tui.sh`, `smoke-verify.sh`, `smoke-memory.sh`, `smoke-eval.sh`)
2. Do a short manual TUI smoke for `/permission`, `/resume`, `/compact`, `/verify`, `/exit` when touching UI paths
3. Update this document if implementation changes the release boundary
4. Prefer deterministic runtime behavior over prompt-only fixes
5. Keep the definition of "minimal harness" as the measuring stick: does the runtime clearly own control flow, permissions, tool execution, context, persistence, verification, and operator UX?
