# hptx

Rust tools for transferring files between HP Saturn calculators (HP48/49,
later 38/39/40) and a modern computer over serial. MIT, see `AI_NOTICE`.

# Agents
Delegate implementation work to Opus agents (`model: opus`) whenever possible;
brief them with the iteration file, the kb docs to read and the acceptance
criteria. The main session reviews.

# Documentation
All project knowledge lives in `./kb/` as markdown with YAML frontmatter.
Read first: `kb/docs/architecture.md`, `kb/decision-log.md`,
`kb/docs/knowledge-sources.md`, `kb/docs/test-policy.md`,
`kb/docs/cli-conventions.md`, `kb/docs/calculator-quirks.md`.
- Iteration plans: `kb/iterations/iteration-N-slug.md`, tasks as checkboxes,
  status `planned` -> `in-progress` -> `completed`.
- Decisions: `kb/decision-log.md` (dated entries; never re-litigate silently).
- Research: `kb/research/`. Backlog: `kb/backlog/`.

Always use `hyalo` for kb interactions, never Read/Grep/Edit on kb files
except for body prose: `hyalo summary`, `hyalo find`, `hyalo read <path>`,
`hyalo set`, `hyalo task toggle`, `hyalo lint`. `.hyalo.toml` sets `dir = "kb"`;
do not pass `--dir`. Follow the hints hyalo prints. `hyalo lint` must be clean
before a PR. The calculator wiki at `~/devel/hp-literature/` is separate and
also hyalo-driven.

# Rust
- Edition 2024, stable. Windows, Linux, macOS.
- Before committing or a PR, in order: `cargo fmt`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace -q`.
- No `.unwrap()`/`.expect()` outside tests; `anyhow::Context` with `?`.
- Test policy in `kb/docs/test-policy.md`: fast unit tests, one e2e binary per
  crate, emulator tests gated by `HPTX_E2E_ADDR`.

# PR discipline
One iteration = one branch (`iter-N/short-description`) = one PR. Self-review
the diff. Commit messages end with
`Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` (or the model that
wrote it). Never commit secrets or ROM images; never push the emulator image.
