# micos Project Memory

This file is the checked-in project memory index for durable `micos` context.

Official Codex generated memories normally live under `CODEX_HOME`, usually `~/.codex/memories/`. Keep this repository-level file small and stable so `AGENTS.md` can point to the docs directory without carrying changing details in the prompt prefix.

## Durable Pointers

- [Harness roadmap](./harness-roadmap.md): next-phase roadmap for the `micos` agent harness.
- [What is harness](./what%20is%20harness/): local background material used to derive the roadmap.
- [Repository instructions](../AGENTS.md): contributor guidance and the minimal project docs pointer loaded by Codex.

## Current Direction

The next `micos` work should treat the harness as the runtime owner of permissions, context selection, persistence, stop reasons, recovery, verification, delegation, and observability.

Before adding large features, check whether the roadmap already names the relevant milestone and acceptance criteria.
