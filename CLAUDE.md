# micos

A minimal Rust-based agent harness. micos provides a CLI REPL (chat loop with
TUI and console fallback) that calls LLM APIs, executes local tools, and records
sessions in `.micos/sessions/`.

**Version:** 0.5.2 — see [docs/roadmap.md](./docs/roadmap.md) for planned milestones.

## Build, Test, and Development Commands

- `cargo run -- chat` — start the interactive REPL (TUI by default, console with `--no-tui`)
- `cargo run -- chat --permission safe|ask|auto --model <model>` — override runtime settings
- `cargo test` — run the full test suite
- `cargo build` — create debug build artifacts
- `cargo fmt` — format Rust source
- `cargo run -- eval` — run deterministic harness eval fixtures
- `scripts/smoke-no-tui.sh` — no-TUI smoke test for core slash commands
- `scripts/smoke-verify.sh` — verification subsystem smoke test
- `scripts/smoke-memory.sh` — memory subsystem smoke test
- `scripts/smoke-eval.sh` — eval subsystem smoke test

## Architecture Overview

micos is organized around explicit module boundaries in `src/`:

| Module | Responsibility |
|---|---|
| `agent/` | Turn loop, tool dispatch, stop handling, compaction, checkpoints |
| `context/` | Model-visible context construction, token estimation, budget stats |
| `model/` | LLM API providers (OpenAI Responses, Chat Completions, DeepSeek) |
| `tools/` | Built-in tool definitions, execution, permission policy engine |
| `session.rs` | Session persistence, JSONL event log (26 event types) |
| `session_replay.rs` | Resume from session JSONL, reconstruct model-visible transcript |
| `prompt.rs` | Prompt assembly with section metadata and runtime context |
| `memory.rs` | Project memory index, topics, candidates, entries lifecycle |
| `plan.rs` | Active plan and handoff draft generation |
| `recovery.rs` | Recovery reports and failure classification |
| `verify.rs` | Project verification profile and check execution |
| `eval.rs` | Deterministic eval fixtures and runner |
| `config.rs` | Configuration parsing (file, env, CLI), permission rule persistence |
| `tui.rs` | Terminal UI (ratatui + crossterm) |
| `ui.rs` | Shared UI primitives, events, slash commands, console fallback |

Model providers, tool execution, permission policy, session persistence, CLI
wiring, and UI rendering are kept in separate modules with small public surfaces.

## Rust Architecture Guidelines

- Prefer typed interfaces over shared mutable state:
  - Use traits for replaceable dependencies (model providers, tool registries, permission policies, session stores).
  - Use enums for finite runtime states and stop reasons.
  - Use structs for data that crosses module boundaries.
  - Add generics when they make a dependency swappable in tests or future extensions.
- Avoid collecting unrelated behavior in a single large file.
- Place new functionality in the closest existing subsystem module.
- Introduce a new submodule when the behavior has its own lifecycle, tests, or ownership boundary.

## Coding Style

- Follow `cargo fmt` for Rust formatting.
- Keep files focused; choose descriptive names over abbreviations.
- Documentation and asset files: lowercase, hyphenated names (`docs/api-overview.md`).
- Source files follow Rust conventions (`snake_case` for files, `CamelCase` for types).

## Testing Guidelines

- Add tests with every behavioral change.
- Keep test files close to the behavior they verify (inline `#[cfg(test)] mod tests` or under `tests/`).
- Name tests by behavior: `parses_valid_config`, `returns_error_for_missing_token`.
- Include regression tests for bug fixes.
- Prefer deterministic runtime behavior tests over LLM-judge evals.

## Commit and PR Guidelines

- Use short, imperative commit messages: `Add config parser`, `Document setup commands`.
- Pull requests: concise description, reason for change, test results, screenshots or recordings for UI changes.
- Link related issues when available.

## Documentation

- [docs/README.md](./docs/README.md) — entry point for all project documentation
- [docs/roadmap.md](./docs/roadmap.md) — current and planned milestones
- [docs/archive/](./docs/archive/) — historical research and completed roadmap docs

## Security

- Keep secrets out of the repository.
- Use `.env` (gitignored) for credentials; `.env.example` for checked-in template.
- This project is MIT licensed; preserve the license notice in redistributed portions.
