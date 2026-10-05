# hptx

Rust tools for transferring files between HP Saturn calculators (HP48/49,
later 38/39/40) and a modern computer over serial. MIT, see `AI_NOTICE`.

# Agents
Delegate implementation work to Opus agents (`model: opus`) whenever possible.
Give an agent the whole task in one message with a concrete finish line: the
iteration file, the kb docs to read, the acceptance criteria and the gates
that must pass. Check its evidence (diff, test output, CI) before accepting a
report. The main session reviews. Agents never edit `CLAUDE.md`.

Rules for every session and agent working in this repo:
- When a step needs no input from the user, keep going; put status notes in
  the same message as the next action. Stop and ask only when you cannot
  continue without the user, or before anything destructive: deleting data,
  force-pushing, or changing anything outside this repository.
- Do not write "think hard" or "think step by step", and do not ask for
  reasoning in replies; reasoning depth is set by the effort setting. Explain
  a design choice in three sentences at most.
- The task list lives in the iteration file: tick each task when its work has
  landed (`hyalo task toggle`), never speculatively, and add anything new you
  find.
- Mark anything you could not confirm, and say where you looked.
- End every run with three headings: Blocked on me, Changed, Found.
- Reviews list only problems that would block the merge, each with file and
  line, why it is wrong, and how to show it fails.

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
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace -q`,
  `cargo deny --locked check`.
- No `.unwrap()`/`.expect()` outside tests; `anyhow::Context` with `?`.
- Test policy in `kb/docs/test-policy.md`: fast unit tests, one e2e binary per
  crate, emulator tests gated by `HPTX_E2E_ADDR`.

# PR discipline
One iteration = one branch (`iter-N/short-description`) = one PR. Self-review
the diff. Commit messages end with
`Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` (or the model that
wrote it). Never commit secrets or ROM images; never push the emulator image.
