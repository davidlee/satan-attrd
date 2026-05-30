---
id: mem.pattern.project.rust-lint-footguns
name: Rust lint footguns (POL-001 strict clippy)
kind: memory
status: active
memory_type: pattern
updated: "2026-05-31"
verified: "2026-05-31"
confidence: high
tags: [rust, clippy, testing, pol-001, footgun]
summary: "POL-001 strict clippy denials that bite when writing satan-attrd code/tests, plus the integration-test target-ordering trap that hides a second wave of lints until the lib compiles clean."
priority:
  severity: high
  weight: 8
provenance:
  sources:
    - kind: file
      note: Cargo.toml [lints.rust]/[lints.clippy] + clippy.toml (POL-001)
      ref: Cargo.toml
    - kind: policy
      note: lint policy
      ref: POL-001
---

# Rust lint footguns (POL-001 strict clippy)

`Cargo.toml` `[lints]` enables `warnings = "deny"` plus a wide clippy
pedantic/cargo/all set ([[POL-001]] — derived from forgettable's lint config).
`just lint` = `cargo clippy --all-targets --all-features -D warnings`. `just
check` adds `fmt --check` + the PG test suite. The denials bite in test code too
(no blanket relaxation). Crate-specific divergence: `HashMap`/`HashSet` are NOT
banned (forgettable bans them; satan uses them intentionally for Counter /
projection maps).

## Clippy denials that catch you

- `indexing_slicing`: no `slice[i]` / `value["k"]`. On slices use
  `.first()`/`.get(n)`; on `serde_json::Value` reads use `.pointer("/a/0")` /
  `.get(k)`, writes use `.as_object_mut().insert(...)`. To walk parallel
  `[f64; 8]` delta rows, `zip` `ATTR_ORDER` with the row instead of
  `enumerate()` + `row[idx]`.
- `unwrap_used` / `expect_used` / `panic` / `unreachable`: also in tests. Prefer
  `?` (make the test fn `-> Result<()>`) or `unwrap_or(...)`. A documented
  contract-violation panic is the one legitimate `#[expect]` case.
- `as_conversions` + the cast lints: no `as`; use `TryFrom`/`try_into`.
- `float_cmp`: `==` on `f32`/`f64` (incl. `[f64; N]` arrays). Compare exact
  table constants only under a reasoned `#[expect]`; otherwise use an epsilon.
- `module_name_repetitions`: e.g. `SystemClock` in `clock`, `create_pool` in
  `pool`. Idiomatic names need a reasoned `#[expect]`.
- `pub_use`: the `lib.rs` facade re-exports trip this — reasoned `#[expect]`
  per `pub use` statement.
- `map_err_ignore`: `.map_err(|_| ...)` discarding the source error — include
  it in the message.
- doc lints: `doc_markdown` (backtick identifiers), `missing_errors_doc` /
  `missing_panics_doc` (add the `# Errors` / `# Panics` section).

## `#[expect]`, never `#[allow]`

`clippy::allow_attributes` + `allow_attributes_without_reason` are denied:
`#[allow(...)]` will not compile. Use `#[expect(LINT, reason = "...")]`. NB
`#[expect]` is self-cleaning — it *errors* if the named lint does NOT fire in
its scope ("unfulfilled expectation"), so place it only where the diagnostic
truly occurs and list exactly the lints that fire. A `#[cfg(test)] mod tests`
module-level `#[expect]` covering the fixture lints (indexing/unwrap/float_cmp
that should "fail loudly") is the practical line; per [[POL-001]] crate-/module-
wide suppression wants maintainer sign-off, so disclose it.

## Integration-test target-ordering trap

`cargo clippy --all-targets` compiles each `tests/*.rs` as its own crate that
links the library. If the **lib** (or lib-test) has clippy errors it fails to
compile, and the integration-test targets never lint — so the first clippy run
only shows `src/` errors. A **second wave** of lints in `tests/*.rs` surfaces
only after `src/` is clean. Always re-run `just lint` to convergence; do not
assume the first error set is the whole job. `tests/harness.rs` historically
carried a crate-level `#![allow(clippy::unwrap_used, clippy::expect_used)]` —
that itself becomes an `allow_attributes` error and must become a reasoned
`#![expect(...)]` (or per-item).

## satan test layout (differs from forgettable)

Each test DB is disposable and provisioned per-test (DE-002) — there is no
shared-DB natural-key collision hazard. `tests/common` helpers unused in a given
test binary still trip `dead_code`; fix with `#[expect(dead_code, reason = "...")]`
on **that binary's `mod common;` declaration**, not on the fns.
