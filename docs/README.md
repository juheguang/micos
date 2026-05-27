# micos Documentation

This directory is the stable entry point for project docs, roadmap notes, and durable context pointers.

## Core Index

- [Harness roadmap](./harness-roadmap.md): next-phase plan for tool permissions, context governance, memory, recovery, evals, multi-agent work, hooks, MCP, and TUI/operator control.
- [Minimal harness roadmap](./minimal-harness-roadmap.md): current 0.4.x execution plan and release acceptance criteria.
- [Claude Code 权限与 Trace 调研](./claude-code-policy-trace-research.md): 面向 v0.3.1 的 permission policy 与 trace foundation 实现调研。
- [Claude Code Context Governance 调研](./claude-code-context-governance-research.md): 面向 v0.3.4 的 prompt/context/token/compact 设计输入。
- [长程任务状态持久化设计判断](./long-running-state-persistence.md): 对 Claude Code、harness 文章和 micos memory/resume/compact/active plan 路线的综合结论。
- [Project memory](./project-memory.md): compact pointer file for durable repository context.
- [What is harness](./what%20is%20harness/): local background material on harness engineering.

## Notes

- Keep `AGENTS.md` short and stable for prompt-cache friendliness.
- Put changing project context in this directory instead of expanding `AGENTS.md`.
- Reserve `.codex/` for official Codex project configuration, such as `.codex/config.toml`, when the project needs it.
