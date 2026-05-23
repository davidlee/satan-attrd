# satan-attrd — handover

## Where this leaves things

**T-attr-1b shipped 2026-05-23** — migration + store + per-run seq
counter + projection rebuild driver + 22 tests (11 unit + 11
integration). `migrate` and `rebuild` CLI subcommands. No dispatcher
yet (T-attr-1c). No LISTENer / RPC loop yet (T-attr-1c).

Test DB: `satan_memory_test` on the local PG socket
(`postgres:///satan_memory_test?host=/run/postgresql`). Same DB the
broker's memory + intervention tests target — the attribute layer
shares the SATAN database with the broker (contract §4 — "two
Postgres tables in the existing SATAN database").

Local gate:

```bash
DATABASE_URL='postgres:///satan_memory_test?host=/run/postgresql' \
  cargo test --offline
```

(`just test` once the devshell is in place.)

## Read first

The substance of this daemon lives in the broker repo at
`~/.emacs.d/`. Read in this order before writing code:

1. `~/.emacs.d/docs/satan/refactor/extraction-policy.md` §"Active
   beachhead" — why this daemon exists, what stays elisp, what
   moves here.
2. `~/.emacs.d/docs/satan/refactor/T-attr-1-attribute-layer.md` —
   theme doc + PR log + amendment block dated 2026-05-23 that
   redirects T-attr-1b/1c/1e here.
3. `~/.emacs.d/docs/satan/attributes/design-contract.md` —
   **authoritative on substance.** Schema, validators, outcome→delta
   table, caps (`friction_cap`, `range_clamp`), pre-dispatch
   snapshot rule, A3 boundary, rebuild modes, disable-switch
   semantics.
4. `~/.emacs.d/docs/satan/attributes/patterns_attributes.design_note.md` —
   architectural grounding for "attributes are global by
   architecture, not v1 narrowing." Read this before any reviewer
   pushes back on global Shame/Suspicion.
5. `~/.emacs.d/docs/satan/attributes.brief.md` §0–§6 — conceptual
   intent the contract narrows from.

## What the daemon owns

- Postgres migration `0007_attributes.sql` — `satan_attributes`
  projection + `satan_attribute_events` append-only log + indices
  (per contract §4). Migrates against the existing SATAN database
  the broker already uses; we are adding to its schema, not
  creating a new database.
- `store` — UPSERT projection, INSERT event, per-run seq counter,
  lookup by `(scope, name)`, lookup-prior-event-by-intervention
  (for §6.2 revision algorithm).
- `dispatcher` — consumes `intervention.outcome_classified` /
  `intervention.outcome_revised`, applies the §6 table + §6.1
  confidence weighting + §6.3 pre-dispatch snapshot + §7 caps,
  emits `attribute.delta_applied`.
- `rebuild` — projection-from-events replay driver
  (`--include-disabled` mode per contract §10.2).
- `rpc` — server side. Broker is the only client.

## What the broker keeps (stays elisp)

- Capsule render (T-attr-1d). Broker queries the daemon for a
  current attribute snapshot pre-spawn; capsule glue stays in
  `~/.emacs.d/satan/`.
- Disable-switch defcustom (`dl-satan-attribute-updates-enabled`).
  Broker emits the source event regardless; daemon checks the
  switch state (received as part of the source-event payload) and
  records `disabled: true` when the broker reports disabled.
  Daemon-side check is preferred so the event log preserves
  "would have applied X but disabled" for `--include-disabled`
  replay — see "Three pinned design choices" below.
- Tool handlers exposed to the model. None planned for v1; the
  layer is read-only to the model via the capsule.
- Audit transcript writing. Daemon RPCs the event back; broker
  writes `transcript.jsonl` — see "Audit transcript path" below.

## Three pinned design choices (per amendment, not yet in contract)

The theme doc amendment block of 2026-05-23 pins three choices. The
contract will adopt them in its next change-history row alongside
T-attr-1b's first code-bearing PR.

1. **Audit transcript path.** Daemon writes the
   `satan_attribute_events` row, then RPCs the event back to the
   broker which writes the `transcript.jsonl` line. Preserves the
   existing "transcript.jsonl is audit truth" convention. The
   alternative (daemon writes table only) is simpler but diverges
   from convention.
2. **Event bus shape.** Broker emits intervention outcome events via
   a PG queue table + `pg_notify`. Daemon `LISTEN`s. Matches the
   existing `dl-satan-patch-listener.el` pattern. The alternative
   (direct broker→daemon RPC on each emit) is simpler but couples
   the broker's outcome-classifier path to daemon availability.
3. **Disable-switch placement.** Daemon-side. Broker reports the
   switch state in the source-event payload; daemon writes the
   event with `disabled: true` and skips the UPSERT. Cleaner than
   broker-side filtering because the event log retains the would-
   have-applied delta for `--include-disabled` replay.

## Scaffolding source

Cargo.toml deps + Justfile + crate layout lifted from
`~/dev/vk/db/` (bough's data crate). Differences from bough:

- Single crate, not a workspace. One daemon = one binary. If a CLI
  or separate sub-tool appears later, workspace-ify then.
- `sqlx` `json` feature added (we serialize/deserialize
  `evidence_json` and `caps_applied` as `serde_json::Value`).
- `tracing` added — daemon needs structured logs, not `println!`.
- No `rrule` / `nanoid` — neither is relevant.
- `tokio` features: `signal` (graceful shutdown), `sync` (broker
  RPC channels), `time` (LISTEN backoff / cap timers).

## What's NOT in this scaffold

- `flake.nix`. Deferred until the daemon is integrated into the
  user's home-manager wiring. Today: build with the host toolchain
  / direnv / rustup. Cargo-level deps are pinned; that is enough.
- `LICENSE.md`. Crate metadata claims MIT; the file will land with
  T-attr-1b or earlier if useful.
- `.envrc` / direnv layer. Add when a devshell appears.
- Any CI workflow. Justfile gates (`just check`) are the local CI
  contract; remote CI deferred.

## Halt conditions for the next session

- **Reviewer suggests scoping Shame/Suspicion/Doubt to a cue,
  hypothesis, or pattern.** Stop. Surface
  `patterns_attributes.design_note.md` and push back. Attributes
  are global by architecture; per-cue consequences live in pattern
  records (separate theme, not part of T-attr-1). Recorded in
  project memory: `project-satan-attributes-global`.
- **A code change crosses contract surface.** Amend the contract
  first (`design-contract.md` §16 change-history row), then code.
  T-attr-1a's contract was patched twice during external review;
  treat it as the authority.
- **`sqlx::migrate!` macro complains about a not-yet-migrated
  database.** Daemon must NOT auto-migrate at startup in
  production — migration runs explicitly via `satan-attrd migrate`
  (or `just migrate`). Auto-migration on boot would race other
  brokers / agents.
- **Replay determinism failure.** Sort key is `(ts, run_id, seq)`,
  not `id` (lexicographic sort breaks at `attr10` vs `attr9`).
  Contract §10.4.

## First concrete step (T-attr-1c)

1. `src/dispatcher.rs` — consume the broker's intervention outcome
   event (received via the §17.3 PG queue + `pg_notify` bus, NOT
   yet wired here; T-attr-1c lands both pieces). Apply contract §6
   delta table + §6.1 confidence weighting + §6.3 pre-dispatch
   snapshot + §7 caps. For each affected attribute: call
   `store::insert_event` + (when not disabled) `store::upsert_attribute`.
2. `src/rpc.rs` — RPC the constructed `attribute.delta_applied`
   event back to the broker for transcript writing (contract §17.4).
   Pick a transport; the existing `dl-satan-patch-listener.el` uses
   PG NOTIFY for the broker-to-daemon direction — match it for the
   reverse direction (daemon emits to a `satan_audit_inbox` queue
   table, broker LISTENs).
3. `src/main.rs` — gain a `run` subcommand that opens the pool,
   LISTENs on the broker's outcome-event channel, and dispatches
   each notification through `dispatcher::handle`. Reuse the
   `Counter` state per `run_id` via a `HashMap<String, Counter>`
   (drop entries when the run ends — broker emits a run-end
   signal? if not, last-touched LRU).
4. Tests — `tests/dispatcher.rs` with the §12 contract test
   surface: golden 15-case delta table (5 classifications × 3
   confidences); pre-dispatch snapshot ordering;
   revision-against-actual-prior-deltas (seed a prior outcome that
   hit `range_clamp`, then revise; assert revision_delta computed
   against actual not theoretical); revision chain. `friction_cap`
   tests use a **direct-store helper** to synthesise a positive
   friction delta (no v1 outcome can produce one — §7.1
   forward-compat note).

T-attr-1c is the first PR with model-influencing behaviour. Pin
the broker-side wiring (queue table schema, channel names, the
broker's outcome emitter changes) in the contract before code if
they require a new audit-event shape.

`just check` must stay green at every step. `cargo clippy` is
`-D unwrap_used -D expect_used`.
