# Working in this repo

## No "pre-existing" excuses

If you encounter a bug, lint warning, type issue, failing check, or any
other fixable imperfection while working here — fix it, in the same
branch, regardless of who introduced it or when. "Pre-existing" is not a
reason to leave something broken; it is a reason it has waited too long
already. "Out of scope", "unrelated to this change", and "someone else's
file" are not reasons either — topic has no bearing on whether a fix
belongs here, because drive-by fixes get their own commits (below) and
cost the feature diff nothing.

The only acceptable reason to leave an imperfection standing is that it is
genuinely out of reach: an external service, an upstream dependency bug,
or a fix too large to land as a self-contained commit. File an issue
before moving on and say so in the PR — an unfixed imperfection is always
visible, never silent.

Consequences in practice:

- `cargo clippy --all-targets` and the frontend `bun run check` are
  expected to be warning-free after your change, not merely "no worse".
- A red CI check on your branch gets diagnosed and fixed, even when the
  same failure exists on main.
- Stale comments, dead code, and wrong docs you walk past are yours.
- Drive-by fixes belong in the branch, but in their own commit(s), themed
  and kept out of the feature commits. A reviewer should be able to read
  "clear the clippy warnings" on its own, and revert either half without
  the other.
- A lint whose fix makes the code harder to read earns a narrow `#[allow]`
  with a one-line reason instead of a contorted rewrite. Clearing the
  warning is the goal; obeying every lint literally is not.

## Verification commands

- Backend: `cd src-tauri && cargo test` and `cargo clippy --all-targets`
- Frontend: `bun run build` (vue-tsc + vite), `bun run test:frontend`,
  `bun run check` (oxlint + oxfmt)
- TypeScript bindings are generated from the specta builder; the
  `bindings_are_current` test rewrites `src/bindings.ts` — commit what the
  generator emits verbatim (CI diffs against it byte-for-byte).

## Comments

Default to none, and apply one mechanical test to every one you keep:
**does it describe the code it sits on?**

A note about a local invariant survives, because it gets edited alongside
the code it constrains. Anything reaching outside its own lines has
nothing holding it true, and rots silently into a confident lie:

- what another module does, or how this one differs from it
- what callers pass in, or which callers exist
- what the previous approach was, or why it was replaced
- a restatement of a format, schema, or spec defined elsewhere

That material is real, and it goes in the PR body, the commit message, or
the issue — read once by the person who needs it, then allowed to go
stale honestly. Not in a file that outlives it.

Delete on sight:

- a doc line paraphrasing the signature under it (`/// Fold one record
  into the running facts` over `fn push_record`)
- a doc on a type alias repeating the doc on its only user
- a module header restating a file format a sibling module already
  documents — "repeated here so this file reads on its own" is the tell
- prose asserting a guarantee a test already covers: the test name says
  it and *breaks* when it stops being true

What survives is short, and answers "why not the obvious thing?":

```rust
// Keyed on the raw f32 bits, not the values: NaN != NaN and -0.0/0.0
// differ, so a naive == would split or merge vertices.
type VertexKey = (u32, u32, u32);
```

Two lines, not twenty. If the reasoning genuinely needs a paragraph, the
*design* needs the paragraph — write it in the issue and link it.

This binds `///` and `//!` exactly as much as `//`, including the doc
comments that end up copied into `src/bindings.ts`. Volume is a smell on
its own: a file that is a fifth comment by line count is mostly narration.

## House conventions worth knowing

- Long-running work follows the catalog job pattern: registered job id
  with a kind prefix, cancel flag, throttled progress events, per-item
  failure isolation, and persisted-as-you-go results so any interrupted
  run resumes for free on rerun.
- Derived/scanned data always defers to user curation (`model_user_meta`
  overrides scanner inference); new metadata follows the same precedence.
- Schema changes are additive (`CREATE TABLE IF NOT EXISTS` /
  `ALTER TABLE ... ADD COLUMN` guarded) — no migrations framework.
