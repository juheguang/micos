# Repository Guidelines

## Project Structure & Module Organization

This repository contains a root-level Rust CLI crate for the `micos` agent harness. Keep files organized as follows:

- `src/` for application or library source code.
- `tests/` for automated tests that mirror `src/` structure.
- `assets/` for static images, fixtures, sample data, or other non-code resources.
- `docs/` for design notes, API references, and contributor-facing documentation.

Place package manifests, linters, formatters, and build scripts at the repository root. Avoid committing generated output unless required for distribution.

## Build, Test, and Development Commands

Canonical commands are runnable from the repository root:

- `cargo run -- chat` to start the interactive REPL.
- `cargo run -- chat --permission safe|ask|auto --model <model>` to override runtime settings.
- `cargo test` to run the full test suite.
- `cargo build` to create debug build artifacts.
- `cargo fmt` to format Rust source.

Avoid duplicate command paths for the same task unless the reason is documented.

## Coding Style & Naming Conventions

Follow the formatter and linter once introduced. Until then, use consistent indentation within each language ecosystem, keep files focused, and choose descriptive names over abbreviations.

Use lowercase, hyphenated names for documentation and asset files, for example `docs/api-overview.md`. Use native language conventions for source files, classes, functions, and tests.

## Rust Architecture Guidelines

Treat `micos` as a minimal agent harness with explicit runtime boundaries. Keep model providers, tool execution, permission policy, session persistence, CLI wiring, and UI rendering in separate modules with small public surfaces.

Prefer typed interfaces over shared mutable state: use traits for replaceable dependencies such as model providers, tool registries, permission policies, and session stores; use enums for finite runtime states and stop reasons; use structs for data that crosses module boundaries. Add generics when they make a dependency swappable in tests or future harness extensions.

Avoid collecting unrelated behavior in a single large file. When adding functionality, place it in the closest existing subsystem module, and introduce a new submodule when the behavior has its own lifecycle, tests, or ownership boundary.

## Testing Guidelines

Add tests with every behavioral change. Keep test files close to the behavior they verify, either under `tests/` with matching paths or in Rust module test blocks next to the implementation.

Name tests by behavior, such as `parses-valid-config` or `returns-error-for-missing-token`. Include regression tests for bug fixes.

## Commit & Pull Request Guidelines

Git history currently shows only `Initial commit`, so no project-specific convention is established. Use short, imperative messages such as `Add config parser` or `Document setup commands`.

Pull requests should include a concise description, reason for the change, test results, and screenshots or recordings for UI changes. Link related issues when available.

## Security & Configuration Tips

Keep secrets out of the repository. Use ignored local environment files for credentials, and provide checked-in examples such as `.env.example` when configuration is required. This project is MIT licensed; preserve the license notice in substantial redistributed portions.

## Project Documentation

Use [docs/README.md](docs/README.md) as the entry point for project docs, roadmap notes, and durable context pointers.
