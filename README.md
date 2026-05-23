# satan-attrd

SATAN attribute layer daemon. Owns the global-attribute projection
(`satan_attributes`) + event log (`satan_attribute_events`), consumes
intervention outcome events from the broker, applies the outcome→delta
table from the design contract, and RPCs `attribute.delta_applied` audit
events back to the broker for transcript writing.

First Rust beachhead extracted out of `~/.emacs.d/satan/`. Greenfield —
no port from elisp; the broker never shipped an attribute store.

## Status

**Scaffold only.** T-attr-1b (migration + store) is the first
implementation slice. See [`HANDOVER.md`](HANDOVER.md) for full
context, design-contract pointer, and the three pinned daemon
design choices.

## Where the design lives

The normative design contract lives in the broker repo, not here:

- `~/.emacs.d/docs/satan/attributes/design-contract.md` — schema,
  validators, outcome→delta table, caps, rebuild semantics,
  multi-attribute snapshot rule, A3 boundary.
- `~/.emacs.d/docs/satan/refactor/T-attr-1-attribute-layer.md` —
  theme doc + PR log (1a–1e).
- `~/.emacs.d/docs/satan/refactor/extraction-policy.md` §"Active
  beachhead" — rationale for the daemon split.

`HANDOVER.md` in this repo is the working pointer + locally-decided
design notes; the contract above is authoritative on substance.

## Architecture

Single binary crate. Library half (`src/lib.rs`) is exposed so
integration tests can drive the store + dispatcher without booting
the daemon process.

| Module (planned) | Role |
|---|---|
| `error` | Typed errors (`thiserror`) |
| `pool` | Postgres pool lifecycle |
| `migrate` | `sqlx::migrate!` runner |
| `store` | `satan_attributes` UPSERT + event INSERT + lookup + counter |
| `dispatcher` | Consumes `intervention.outcome_classified` / `outcome_revised`; applies §6 + §7 |
| `rpc` | Broker ↔ daemon transport (event bus shape pinned in HANDOVER) |

Direct Postgres access. Daemon LISTENs on a broker-owned queue table
(`pg_notify` pattern, matches the existing patch-listener).

## Development

```bash
just check     # lint + format + test
just migrate   # run migrations against $DATABASE_URL
just run       # start daemon
```

Requires PostgreSQL reachable at `$DATABASE_URL`. The Justfile
defaults to the supabase-style local dev URL; override in your shell
or `.envrc` for any other target.

## License

MIT. See [`LICENSE.md`](LICENSE.md) once it exists; scaffold ships
without one yet.
