# micos Roadmap

This document is the single source of truth for micos planning. It supersedes
the archived `harness-roadmap.md` and `minimal-harness-roadmap.md` (see
[archive/](./archive/)).

## Current Baseline — v0.7.1

micos is a functioning local coding agent harness (~17K lines, 143 tests, 5 eval fixtures).
The runtime currently owns:

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

## Current Gaps (as of v0.7.1)

Most v0.5 and v0.6 items are complete. Remaining gaps:

- **Shell output is batch-collected.** stdout/stderr are read only after the process exits. No streaming progress for long-running commands like `cargo build` or `cargo test`.
- **No CI.** Tests run only locally via `cargo test`. No automated gate on push/PR.
- **No hooks system.** No lifecycle events for project-specific rules to hook into.
- **No MCP client.** No external tool server integration.

## Milestones

### v0.5 — Production Hardening ✅ (done in v0.5.x)

Target: make the existing feature set robust for regular daily use.

**Structured Error Handling:** ✅
- `ToolExecError` has 7 typed variants: `Timeout`, `Io`, `Parse`, `Permission`, `ProcessExit`, `Utf8`, `Other`
- `ToolErrorKind` enum propagates to session events for recovery classification
- Error types map to retryable/terminal classification

**Tool Enrichment:** ✅
- `grep` — ripgrep-backed search with regex, file:line matches, 64KB limit, 500 max matches
- `edit` — search/replace (first occurrence) + line-based range replacement, write_file-level safety
- `glob` — recursive file pattern matching, 32KB limit, paths relative to cwd
- All tools follow the same permission/audit/truncation path

**Auto-Compression:** ✅
- Threshold trigger with `Off`/`Warn`/`Auto` modes
- Write-Before-Compaction: extract memories before compacting
- Circuit breaker: 3 consecutive failures → disable
- Configurable via CLI (`--auto-compact`), env (`MICOS_AUTO_COMPACT`), or config file

**Prompt Professionalization:** ✅
- 11 base sections (up from 7): Identity, Task discipline, Tool selection, File editing, Permissions, Error recovery, Verification, Context governance, Safety, Tool reference, Reporting style
- ~160 lines of behavioral instructions with concrete patterns

**Shell Streaming:** ⬜ (deferred to v0.7.2)
- Streaming stdout/stderr capture during execution
- Progress indicators: elapsed time, output bytes accumulated

### v0.6 — Memory and Context Lifecycle ✅ (done in v0.6.x)

Target: close the loop between session work, durable memory, and context budget.

> **Detailed plan:** [docs/roadmap-0.6.md](./roadmap-0.6.md)

**Memory Enhancement:** ✅
- Model-driven candidate extraction triggers at session end and on `/memory refresh`
- Candidate deduplication: title/body comparison against existing entries (new/similar/duplicate)
- Auto-staleness: `/memory sweep` checks `last_validated_at`, marks stale entries
- Four memory types: User, Feedback, Project, Reference with structured frontmatter

**Context Budget Management:** ✅
- 7-layer context assembly: base_prompt → runtime → memory_index → active_plan → compact_summary → recent_tail → tool_schemas
- `/context` shows per-layer and per-category token utilization
- `ContextLayer` struct with name + token count for each layer

**Three-Tier Compression:** ✅
- Micro-compaction: time-based (600s idle), clears old tool outputs in-place, zero API calls
- Auto-compact: threshold-triggered model summarization with Write-Before-Compaction
- Manual `/compact`: structured summary with 8 validated headings
- Compact preserves `function_call`/`function_call_output` pairs

**MEMORY.md Auto-Maintenance:** ✅
- `promote_candidate()` auto-updates MEMORY.md index
- `mark_entry_status(Forgotten)` auto-removes from index
- Index trimmed to 200 lines on each write

### v0.7 — Context Refinement 🟡 (in progress)

Target: close remaining context governance gaps and address engineering foundations.

**Completed:**
- ✅ Micro-compaction (`v0.7.0`-`v0.7.1`): time-based (600s idle) lightweight tool output cleanup, zero API calls, KV-cache-aware design
- ✅ Context architecture documentation (`docs/context-architecture.md`)

**In Progress / Planned:**
- ⬜ Shell streaming output (`v0.7.2`): replace batch-collected shell output with real-time streaming to TUI and console
- ⬜ CI workflow (`v0.7.2`): GitHub Actions with `cargo test` + `cargo fmt --check`
- ⬜ Roadmap + doc updates (`v0.7.2`): reflect actual completion status

### v0.8 — Extensibility (deferred)

Target: expose controlled lifecycle points and external tool integration.

**Hooks System:**
- Lifecycle events: `SessionStart`, `BeforeTurn`, `BeforeToolUse`, `AfterToolUse`, `OnError`, `BeforeCompact`, `AfterCompact`, `BeforeStop`
- Hook actions: allow, deny, modify (add context, add warning, suggest permission)
- Project-local config: `.micos/hooks.toml`
- Hook sandboxing: timeout per hook, failure isolation (one hook crash doesn't kill the runtime)

**MCP Client:**
- Connect to MCP servers for external tool schemas
- External tools follow the same permission → audit → trace pipeline as built-in tools
- MCP resources and prompts as context sources

**Multi-Agent Delegation Foundation:**
- Define delegation protocol: task brief, allowed tools, context pack, expected artifact, timeout, success criteria
- Subagent isolation: separate transcript, bounded tool access, independent stop conditions
- Result merge policy: how subagent output integrates into the main session transcript

## Deferred Work (Non-Goals for current versions)

- Full plugin ecosystem (hooks are the extension point, not a plugin system)
- Remote session sync / collaboration
- GUI or browser automation
- Destructive rollback (git checkpoints remain audit-only)
- Automatic memory promotion without user review
- Claude Code UX parity
- SQLite or vector DB for memory storage (file system is sufficient)

## Operating Principles

Before starting feature work each session:
1. Run `cargo test` and all smoke scripts (`smoke-no-tui.sh`, `smoke-verify.sh`, `smoke-memory.sh`, `smoke-eval.sh`)
2. Do a short manual TUI smoke for `/permission`, `/resume`, `/compact`, `/verify`, `/exit` when touching UI paths
3. Update this document if implementation changes the release boundary
4. Prefer deterministic runtime behavior over prompt-only fixes
5. Keep the definition of "minimal harness" as the measuring stick: does the runtime clearly own control flow, permissions, tool execution, context, persistence, verification, and operator UX?
