# AGENTS.md — satan-attrd orientation

This daemon is a SATAN-orbit module extracted out of
`~/.emacs.d/satan/`. The broker (still elisp) is the trust boundary;
this daemon is a dumb transport + pure transform per the extraction
policy. Read `HANDOVER.md` first for the working state + design
pointers into the broker repo.

## Don'ts

- Don't fiddle with `git stash` — `$HOME` is itself a git repo;
  stashing from any subdir sweeps all untracked files in `~/`.
  Use `git show HEAD:path` for cross-checks instead.
- Don't commit until `just check` is green. Clippy is
  `-D unwrap_used -D expect_used`; that is not negotiable.
- Don't auto-migrate on daemon start. Migration is an explicit
  `satan-attrd migrate` invocation. Auto-migration races other
  agents.
- Don't introduce a wall-clock dependency in the dispatcher. The
  broker passes `:time_now` in the source event; use it. Contract
  §10.4 + §11.
- Don't take design liberties with attribute semantics. The
  contract at `~/.emacs.d/docs/satan/attributes/design-contract.md`
  is authoritative; amend its §16 change-history before changing
  schema, validators, deltas, caps, or rebuild semantics.

## When searching

- Use `rg` / `fd`, not `find` / `grep`.
- Don't search `/` or `~/` — scope to `./` or `~/.emacs.d/`
  explicitly.
- Cross-repo reads (`~/.emacs.d/`) are fine and expected;
  cross-repo *writes* belong in the broker's commit, not this
  one.

## Where the broker lives

`~/.emacs.d/` is a separate git repo (since 2026-05-23) at
`github:davidlee/emacs-config`. The flake input
`emacs-config` is `path:/home/david/.emacs.d`. The broker's
attribute-layer code (the elisp half that survives this
extraction) lives under `~/.emacs.d/satan/`.

## Layering rule

```
broker (elisp, ~/.emacs.d/satan/)
  ├── owns: capsule render glue, model-facing tool handlers,
  │         disable-switch defcustom, audit transcript writes,
  │         intervention.outcome_classified emit
  │
  ↓ PG queue + pg_notify  (event bus)
  ↑ RPC reply              (audit event back for transcript)
  │
satan-attrd (this repo, ~/dev/satan-attrd/)
  └── owns: satan_attributes / satan_attribute_events tables,
            outcome→delta dispatcher, caps, rebuild driver
```

Daemon never writes `transcript.jsonl`. Broker never writes
`satan_attributes`.

## Test rule

Integration tests require Postgres on `$DATABASE_URL`. Unit tests
(`cargo test --lib`) must stay pure — no DB, no clock, no I/O.
Mirror the design contract's test surface (§12) when adding cases.

## When implementation lands

When T-attr-1b's first code-bearing PR is ready, also amend the
broker-side theme doc + design-contract change-history in the
same human's session: contract becomes `status: merged`, and the
three pinned daemon design choices (audit path / event bus /
disable placement) get a new change-history row.
