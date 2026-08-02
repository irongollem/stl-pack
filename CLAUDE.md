# Working in this repo

## No "pre-existing" excuses

If you encounter a bug, lint warning, type issue, failing check, or any
other fixable imperfection while working here — fix it, in the same
branch, regardless of who introduced it or when. "Pre-existing" is not a
reason to leave something broken; it is a reason it has waited too long
already. The only acceptable reasons to leave an imperfection in place
are: it is genuinely out of reach (external service, upstream dependency
bug), or fixing it would balloon the diff so much it deserves its own PR —
in which case file an issue for it before moving on, and say so.

Consequences in practice:

- `cargo clippy --all-targets` and the frontend `bun run check` are
  expected to be warning-free after your change, not merely "no worse".
- A red CI check on your branch gets diagnosed and fixed, even when the
  same failure exists on main.
- Stale comments, dead code, and wrong docs you walk past are yours.

## Verification commands

- Backend: `cd src-tauri && cargo test` and `cargo clippy --all-targets`
- Frontend: `bun run build` (vue-tsc + vite), `bun run test:frontend`,
  `bun run check` (oxlint + oxfmt)
- TypeScript bindings are generated from the specta builder; the
  `bindings_are_current` test rewrites `src/bindings.ts` — commit what the
  generator emits verbatim (CI diffs against it byte-for-byte).

## House conventions worth knowing

- Comments state constraints the code can't show — never narrate the next
  line, never address a reviewer.
- Long-running work follows the catalog job pattern: registered job id
  with a kind prefix, cancel flag, throttled progress events, per-item
  failure isolation, and persisted-as-you-go results so any interrupted
  run resumes for free on rerun.
- Derived/scanned data always defers to user curation (`model_user_meta`
  overrides scanner inference); new metadata follows the same precedence.
- Schema changes are additive (`CREATE TABLE IF NOT EXISTS` /
  `ALTER TABLE ... ADD COLUMN` guarded) — no migrations framework.
