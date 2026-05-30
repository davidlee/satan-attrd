# Implementation Notes — DE-001 Lint Cleanup

## Gate results

- `just lint`: **PASS** (zero warnings)
- `cargo fmt --all --check`: **PASS**

## Changes summary

### Cargo.toml
- Added `repository`, `keywords`, `categories` to `[package]`

### src/lib.rs
- 6 × `#[expect(clippy::useless_attribute, clippy::pub_use, reason = "crate facade")]` — dual-expect to resolve circular clippy pass ordering between `pub_use` and `useless_attribute`

### src/clock.rs
- `#[expect(clippy::module_name_repetitions)]` on `SystemClock` and `FakeClock`

### src/pool.rs
- `#[expect(clippy::pub_use, clippy::module_name_repetitions)]` on `PgPool` re-export
- `#[expect(clippy::module_name_repetitions)]` on `create_pool`
- doc_markdown: backtick `LISTENer`

### src/store.rs
- `impl Default for Counter` (to support `.or_default()`)
- `missing_panics_doc`: added `# Panics` to `Counter::next`
- 3 × `type` aliases: `AttributeLookupTuple`, `EventQueryTuple`, `RebuildRowTuple`
- `shadow_unrelated`: renamed closure params `scope`/`name` → `r_scope`/`r_name`
- doc_markdown: backtick `LISTENer`, `run_id`
- Test module: `#[expect(clippy::indexing_slicing)]` — remove `unwrap_used` (did not fire)

### src/decay.rs
- doc_markdown: backtick `run_id`, `DECAY_TARGETS`, `EventInsert`, `insert_event`
- `#[expect(clippy::module_name_repetitions)]` on `decay_threshold`, `DecayScheduler`
- `clone_on_copy`: `snapshot.clone()` → `snapshot` (field init shorthand)
- `unnested_or_patterns`: merge `Some("a") | Some("b")` → `Some("a" | "b")`

### src/dispatcher.rs
- doc_markdown: backtick `evidence_json`, `intervention_id`
- `indexing_slicing` (real src): zip across 4 dispatch functions + revision
- `indexing_slicing` (dispatch_maintenance / contract §17.8): `#[expect]` on projection index
- `indexing_slicing` (outcome_evidence): `.as_object_mut().insert()` instead of `ev["key"] =`
- `shadow_unrelated` (tests): renamed `aff`→`aff2`, `plan`→`plan2`
- Test module: `#[expect(clippy::unwrap_used, clippy::indexing_slicing, clippy::float_cmp)]`

### src/run_loop.rs
- `.or_insert_with(Counter::new)` → `.or_default()`
- `missing_errors_doc`: `# Errors` on `run`, `drain_outcome_inbox`, `drain_audit_replies`
- `indexing_slicing` (union_affected): zip pattern
- Test module: `#[expect(clippy::unwrap_used, clippy::indexing_slicing)]`

### src/rpc.rs
- doc_markdown: backtick `LISTENer`
- `missing_errors_doc`: `# Errors` on `check_schema_major`
- `map_err_ignore`: include source error in message
- Test module: `#[expect(clippy::unwrap_used, clippy::indexing_slicing)]`

### src/notify_stream.rs
- doc_markdown: fenced JSON block
- `missing_errors_doc`: `# Errors` on `run`
- `needless_pass_by_value`: `io_to_err` takes `&Error`; call sites updated

### src/tuning.rs
- `match_same_arms`: merged `Written|Renamed` and `Overwritten|Deleted`

### src/types.rs
- Test module: `#[expect(clippy::unwrap_used, clippy::single_element_loop)]`

### src/main.rs
- `#[expect(clippy::print_stderr)]` on `print_usage`
- `#[expect(clippy::disallowed_methods)]` on `std::env::var`
- `map_err_ignore`: include source error

### tests/harness.rs
- `#![expect(clippy::expect_used, clippy::tests_outside_test_module, clippy::disallowed_methods)]`
- `#[expect(dead_code)]` on `mod common`

### tests/common/mod.rs
- `#![expect(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods, unreachable_pub)]`
- `#[expect(clippy::let_underscore_must_use)]` on DROP DATABASE
- `#[expect(clippy::panic)]` on CREATE DATABASE privilege failure

### tests/dispatcher.rs
- doc_markdown: backtick `range_clamp`, `gather_prior_actuals`, `insert_event`, `friction_cap`, `run_id`
- `#![expect(clippy::unwrap_used, clippy::tests_outside_test_module, clippy::indexing_slicing)]`
- `#[expect(dead_code)]` on `mod common`

### tests/run_loop.rs
- `#![expect(clippy::unwrap_used, clippy::tests_outside_test_module, clippy::indexing_slicing)]`
- `#[expect(dead_code)]` on `mod common`

### tests/store.rs
- `#[expect(clippy::too_many_arguments)]` on `insert_raw_event`
- `#![expect(clippy::unwrap_used, clippy::tests_outside_test_module, clippy::indexing_slicing, clippy::panic)]`
- `#[expect(clippy::as_conversions, clippy::cast_possible_wrap)]` on `i as i64`
- `shadow_unrelated`: renamed `value`→`value2`

### tests/decay.rs
- doc_markdown: backtick `run_id`
- `#![expect(clippy::unwrap_used, clippy::tests_outside_test_module, clippy::indexing_slicing, clippy::as_conversions, clippy::cast_possible_wrap, clippy::cast_possible_truncation, clippy::clone_on_ref_ptr)]`
- `#[expect(dead_code)]` on `mod common`

## Lint notes (POL-001 §Verification)

No config/lint-level changes. All suppressions are narrow, reasoned `#[expect]` per policy.

## Lint notes per POL-001 §Verification

```
Lint notes: added local #[expect] annotations across 16 files.
No crate lint levels changed. No config files edited.
All site-specific suppressions carry a reason per POL-001 §5.
Crate-level expects in tests/*.rs integration test crates disclosed per handoff.
```
