---
id: IP-001-P01
slug: lint_cleanup_make_tree_green_under_pol_001_strict_clippy
name: "Phase 1 - Fix all lint violations"
created: "2026-05-31"
updated: "2026-05-31"
status: draft
kind: phase
plan_ref: IP-001
phase: 1
---

# Phase 1 – Fix all lint violations

## 1. Entrance Criteria

- [x] DR-001 approved
- [x] POL-001 lint config installed
- [x] `LINT-CLEANUP-HANDOFF.md` available with work-item list A–J

## 2. Tasks

### A. Cargo.toml [package] metadata
- Add `repository`, `keywords`, `categories` fields
- Fixes 3 `cargo`-group errors

### B. Test module #[allow] → #[expect]
Files: `src/dispatcher.rs:568`, `src/store.rs:587`, `src/run_loop.rs:713`, `src/rpc.rs:104`, `src/types.rs:409`
- Convert `#[allow(clippy::unwrap_used)]` → `#[expect(LINT, reason = "...")]`
- Fold in fixture lints (indexing_slicing, float_cmp, shadow_unrelated, single_element_loop)
- Reason: "test fixtures index result vecs after length asserts, unwrap on known-good values, and compare exact table constants; failures should surface loudly"
- Adjust lint list to exactly what clippy reports

### C. doc_markdown
- Wrap bare identifiers in backticks in doc comments
- Files: `decay.rs:26,74,103,189,216,251(x2)`, `dispatcher.rs:224,313`, `notify_stream.rs:13`, `pool.rs:9`, `rpc.rs:11`, `store.rs:27,28,452`

### D. missing_errors_doc / missing_panics_doc
- Add `# Errors` section: `notify_stream.rs:26`, `rpc.rs:81`, `run_loop.rs:415,446,464`
- Add `# Panics` section: `store.rs:45` (Counter::next asserts on seq overflow)

### E. indexing_slicing (real source)
- `dispatcher.rs:241,333,393` — zip ATTR_ORDER with row
- `dispatcher.rs:272,273` — zip ATTR_ORDER, new_row, prior_row
- `run_loop.rs:705` — zip ATTR_ORDER, row_a, row_b
- `dispatcher.rs:502,503` — `.as_object_mut().insert()`
- `dispatcher.rs:453` — `#[expect]` (contract §17.8)

### F. pub_use
- `#[expect(clippy::pub_use, reason = "crate facade: deliberate public-API re-export")]` on:
  `lib.rs:22,23,24,31,32,37`, `pool.rs:3`

### G. module_name_repetitions
- `#[expect(clippy::module_name_repetitions, reason = "idiomatic name; ...")]` on:
  `clock.rs:18,31`, `pool.rs:3,14`, `decay.rs:87,114`

### H. shadow_unrelated (real src)
- `store.rs:280` — rename closure params `scope`/`name` → `r_scope`/`r_name`
- `store.rs:343` — same

### I. type_complexity
- `store.rs:262,306,516` — define `type` aliases for query_as tuple types

### J. One-offs
- `decay.rs:295` — drop `.clone()` (Copy type)
- `decay.rs:58` — `Some("a") | Some("b")` → `Some("a" | "b")`
- `run_loop.rs:79` — `.or_insert_with(Counter::new)` → `.or_default()`
- `rpc.rs:94` — include source error in map_err
- `notify_stream.rs:65` — take `&Error` not `Error`
- `tuning.rs:85,86` — merge identical match arms with `|`

### Second wave (tests/*.rs)
- `tests/harness.rs` — `#![allow]` → `#![expect]`
- `tests/store.rs`, `tests/decay.rs`, `tests/dispatcher.rs`, `tests/run_loop.rs` — indexing/unwrap in fixtures
- Run `just lint` to convergence after src/ is clean

## 3. Task Order

1. `Cargo.toml` package metadata (unblocks test-target compilation)
2. `src/` files (in dependency order): `lib.rs` → `types.rs` → `clock.rs` → `pool.rs` → `store.rs` → `decay.rs` → `rpc.rs` → `notify_stream.rs` → `run_loop.rs` → `dispatcher.rs` → `tuning.rs`
3. Re-run `just lint` — second wave surfaces in tests/*.rs
4. `tests/harness.rs` first (unblocks other test targets)
5. Remaining `tests/*.rs`
6. `cargo fmt --all` then `cargo fmt --all --check`
7. Final `just lint` must be silent

## 4. Verification

- [x] `just lint` exits 0 with zero warnings (after src/ clean AND after tests/ clean)
- [x] `cargo fmt --all --check` passes
- [x] Lint notes disclosure per POL-001 §Verification (list every #[expect])
- [x] No config/lint-level changes in diff

## 5. Exit Notes
