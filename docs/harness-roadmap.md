# micos Harness Roadmap

This document turns the local notes in [`what is harness/`](./what%20is%20harness/) into a concrete engineering roadmap for `micos`.

The working definition is: a harness is the runtime layer that takes responsibility for what the model should not own by itself: state, permissions, stop conditions, context selection, recovery, verification, delegation, and observability.

## Research Inputs

Local material:

- [`what is harness/zhihu-harness.md`](./what%20is%20harness/zhihu-harness.md): frames harness work as structural responsibility allocation, not a temporary patch for weak models.
- [`what is harness/book1-claude-code.pdf`](./what%20is%20harness/book1-claude-code.pdf) and [`book2-comparing.pdf`](./what%20is%20harness/book2-comparing.pdf): retained as local reference material for Claude Code-style harness design and comparison notes.

External references:

- Anthropic, [Effective harnesses for long-running agents](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents): progress files, feature lists, init scripts, end-to-end verification, and common long-running failure modes.
- Anthropic, [Harness design for long-running application development](https://www.anthropic.com/engineering/harness-design-long-running-apps): initializer/coding-agent split, feature-sized work, and artifacts for carrying context across sessions.
- Anthropic, [Effective context engineering for AI agents](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents): context rot, retrieval, and balancing preloaded context with autonomous exploration.
- Anthropic, [Claude Agent SDK permissions](https://code.claude.com/docs/en/agent-sdk/permissions): ordered permission evaluation through hooks, deny rules, modes, allow rules, and runtime callbacks.
- Anthropic, [Claude Code hooks](https://code.claude.com/docs/en/hooks): lifecycle hooks, matchers, handler scopes, injected context, and permission updates.
- Anthropic, [How we built our multi-agent research system](https://www.anthropic.com/engineering/built-multi-agent-research-system): orchestrator-worker multi-agent design for parallel specialized work.
- Anthropic, [Demystifying evals for AI agents](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents): multi-turn evals, deterministic graders, LLM judges, browser agents, static analysis, and regression tracking.
- OpenAI, [A practical guide to building agents](https://openai.com/business/guides-and-resources/a-practical-guide-to-building-ai-agents/): orchestration patterns, layered guardrails, and human intervention.
- OpenAI Agents SDK, [Guardrails](https://openai.github.io/openai-agents-js/guides/guardrails/): input/output/tool guardrails and tripwire-style interruption.
- Model Context Protocol, [Specification overview](https://modelcontextprotocol.io/specification/2025-03-26/basic): tools, resources, prompts, roots, sampling, lifecycle, logging, and authorization as separable protocol concerns.

## Current Baseline

`micos` already has the first layer of a local coding harness:

- A CLI chat loop with TUI and non-TTY fallback.
- Session logs in `.micos/sessions`.
- Tool execution for file and shell operations.
- Permission modes: `safe`, `ask`, and `auto`.
- Stop reasons for final answer, max steps, interrupts, tool denial, tool error, and API error.
- Streaming assistant output and a TUI composer with slash commands.
- Model picker with thinking and reasoning effort controls.

The next phase should make these pieces more explicit, inspectable, recoverable, and composable.

## Roadmap Areas

### 1. Tool Permissions and Execution Policy

Goal: turn tool permission from a mode switch into a policy engine.

Build:

- A permission decision pipeline: hard deny rules, allow rules, mode defaults, and runtime approval.
- Pattern-scoped shell policy, such as read-only commands, test commands, network commands, destructive commands, and repo-mutating commands.
- Persistent approval rules stored under `.micos/config.toml` or `.micos/permissions.toml`.
- Tool summaries that explain risk before approval.
- Audit records for every decision: requested tool, arguments summary, policy hit, user decision, and elapsed time.

Acceptance criteria:

- A dangerous command is blocked even in permissive modes if it matches a deny rule.
- Repeated safe commands can be approved persistently.
- Session logs can explain why a tool was allowed, denied, or escalated.

### 2. Context Governance

Goal: make context a managed resource instead of an append-only transcript.

Build:

- A context budget model that tracks approximate token load by category: system, user, assistant, tool output, memory, repo facts, and plan.
- Tool-result compaction: keep short previews in active context and store full output on disk.
- A context pack format for stable repo facts, current task state, recent decisions, known failures, and verification commands.
- Explicit context selectors: always include, retrieve on demand, summarize, or omit.
- `/context` command showing current context composition and recent compaction decisions.

Acceptance criteria:

- Large tool outputs stop flooding the model context.
- A user can inspect what durable facts are being reintroduced into the next turn.
- Context decisions are deterministic enough to test.

### 3. Memory and State Persistence

Goal: move long-term task state out of chat history.

Build:

- `.micos/memory/` for project-local durable memory: decisions, conventions, recurring failures, runbooks, and task progress.
- `.micos/plans/` for active plans and completed plan summaries.
- A memory index file that links stable entries and records when they were last validated.
- Promotion flow: session note -> candidate memory -> accepted memory.
- A `/memory` command to inspect, add, search, and mark stale memory entries.

Acceptance criteria:

- A fresh session can resume from files without relying on hidden chat history.
- Memories include source pointers and staleness metadata.
- The agent can distinguish durable project facts from temporary task notes.

### 4. Error Recovery and Stop Control

Goal: treat failure paths as first-class runtime states.

Build:

- Typed recovery policies for tool timeout, permission denial, parse error, API error, repeated tool failure, context overflow, and user interrupt.
- Retry limits by failure class.
- Checkpoint before risky edits and restore guidance after failure.
- Stop-reason handling that records what was attempted, what failed, and what the next session should do.
- `/recover` command that summarizes the last failed turn and proposes safe next steps.

Acceptance criteria:

- Repeated tool failure stops with a useful recovery note.
- User interrupt preserves enough state to resume.
- API and parse errors do not leave the TUI in an ambiguous running state.

### 5. Verification, Evals, and Observability

Goal: make the harness measurable.

Build:

- A lightweight trace model: turn id, model call ids, tool calls, approvals, stop reason, token estimates, latency, and cost estimate.
- Deterministic eval fixtures for common workflows: read-only question, small edit, denied tool, failing test recovery, context compaction.
- Optional LLM-judge evals only where deterministic checks are insufficient.
- `/trace` and `/eval` commands.
- Regression reports that compare task success, tool count, elapsed time, and error rate across versions.

Acceptance criteria:

- `cargo test` covers deterministic harness behavior.
- A local eval run can detect regressions in permission, recovery, and context behavior.
- A failed turn can be debugged from trace data without replaying the whole session manually.

### 6. Multi-Agent Delegation

Goal: use multiple agents only when separation of responsibility is valuable.

Build:

- Role catalog: planner, implementer, reviewer, researcher, verifier.
- A delegation protocol: task brief, allowed tools, context pack, expected artifact, timeout, and success criteria.
- Worktree or sandbox isolation for agents that may mutate files.
- Reviewer/verifier agents that do not share the implementer’s generated chain of assumptions.
- Merge policy for subagent outputs into the main session.

Acceptance criteria:

- The main agent can delegate bounded research or review without losing control of the session.
- Mutating subagents cannot silently overwrite the main worktree.
- Multi-agent mode improves verification or parallel research, not just role-play.

### 7. Hooks, Extensions, and MCP

Goal: expose controlled lifecycle points without making the core runtime brittle.

Build:

- Hook events: `SessionStart`, `BeforeModelCall`, `BeforeToolUse`, `AfterToolUse`, `BeforeStop`, and `AfterStop`.
- Hook outputs: block, allow, add context, add warning, update permission suggestion.
- Project-local hook config under `.micos/hooks.toml`.
- MCP client support after local tool policy is stable: tools first, then resources/prompts, then roots and sampling if needed.
- A capability registry so every external tool has a schema, permission class, and audit name.

Acceptance criteria:

- Hooks can enforce project invariants before edits or shell commands.
- Hook-added context is size-limited and traceable.
- External tools follow the same permission and audit path as built-in tools.

### 8. User Experience and Operator Control

Goal: make long-running work observable and interruptible.

Build:

- TUI panes for transcript, current status, context, trace, and approvals.
- Search and filter over message history.
- Inline recovery actions for denied tools, failed tools, and stopped turns.
- Better session browser: recent sessions, stop reasons, changed files, and summary.
- Exportable transcript and trace bundles for debugging.

Acceptance criteria:

- A user can tell what the agent is doing, why it is blocked, and what will happen if they approve.
- Long outputs remain navigable.
- Session state survives restart and can be inspected outside the TUI.

## Suggested Milestones

### v0.3: Policy and Recovery

- Permission pipeline with allow/deny rules.
- Better shell command classification.
- Recovery notes for failed turns.
- Trace records for tool decisions.

### v0.4: Context and Memory

- Context budget display and compaction.
- Project-local memory directory.
- `/context` and `/memory` commands.
- Plan/progress files for multi-session continuity.

### v0.5: Evals and Hooks

- Local eval runner for harness behavior.
- Hook system for project rules and lifecycle automation.
- Trace viewer/export.

### v0.6: Delegation and MCP

- Bounded subagent delegation.
- Worktree/sandbox isolation for mutating delegation.
- MCP client integration through the existing permission and trace pipeline.

## Design Principles

- The model proposes; the harness owns control flow, permissions, persistence, and verification.
- Every failure mode should have a name, a stop reason, and a recovery path.
- Context should be selected, compacted, and traced; chat history should not be treated as memory.
- External capabilities should be registered, permissioned, audited, and testable.
- Multi-agent behavior should add isolation or independent verification, not theatrical role labels.
- Human approval should be informed, resumable, and low-friction.

