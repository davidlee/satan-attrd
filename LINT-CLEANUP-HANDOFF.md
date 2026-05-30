# Lint cleanup handoff (deepseek)

Strict clippy policy (POL-001, derived from forgettable's lint set) was installed.
The build is intentionally RED under it. This is the task to make it GREEN.

## What's already done (do not redo)

- `Cargo.toml` `[lints.rust]` + `[lints.clippy]` — forgettable's set, de-worktree'd.
- `clippy.toml` — `disallowed-methods`; HashMap/HashSet ban deliberately OMITTED.
- `Justfile` `lint` recipe → `cargo clippy --all-targets --all-features -- -D warnings`.
- `.spec-driver/policies/POL-001-*.md` + `registry/policies.yaml` + AGENTS.md pointer.
- `.spec-driver/memory/mem.pattern.project.rust-lint-footguns.md`.

## Prompt

You are fixing clippy lint violations in the Rust crate at /home/david/dev/satan-attrd
so that `just lint` passes with zero warnings. A strict lint policy was just installed
(forgettable's lint set). The build is currently RED by design — your job is to make it
GREEN without weakening the policy.

GATE (DB-free): `just lint`  ==  `cargo clippy --all-targets --all-features -- -D warnings`
Run it repeatedly; iterate to zero. After green, run `cargo fmt --all` then
`cargo fmt --all --check`. (NOTE: `just check`/tests need Postgres on $DATABASE_URL —
out of scope; do not run them. Lint + fmt only.)

AUTHORITATIVE POLICY — .spec-driver/policies/POL-001-rust_lint_policy_for_agentic_development.md.
Hard rules:
  1. DO NOT edit Cargo.toml [lints.*] or clippy.toml. Do not downgrade any lint.
     The config is intentional. (HashMap/HashSet are already deliberately NOT banned —
     do not migrate to BTreeMap.)
  2. Fix in the SPIRIT of the lint first (refactor, safer API). Suppress ONLY when the
     lint is genuinely wrong for that site.
  3. Suppress with #[expect(clippy::LINT, reason = "...")] — NEVER #[allow]. Bare #[allow]
     does not compile here (allow_attributes is denied). Every expect needs a real reason
     stating the invariant/tradeoff, not "clippy complained".
  4. Narrowest scope — smallest item/statement. No crate-level #![expect].
  5. GOTCHA: #[expect] is self-cleaning — it ERRORS if the named lint does NOT fire in its
     scope. When you add a module-level #[expect] to a test module, list EXACTLY the lints
     that fire there; if clippy says "this lint expectation is unfulfilled", remove that
     lint from the list. Trust clippy output over any list below.

WORK ITEMS (file:line from clippy; line numbers drift as you edit — re-run clippy to relocate):

A. Cargo.toml [package] — add `repository`, `keywords`, `categories` (fixes 3 `cargo`-group
   errors). Use repository = the broker/daemon git remote if known, else a placeholder
   github URL; keywords/categories: short relevant lists.

B. Test modules — convert each `#[allow(clippy::unwrap_used)]` above a `#[cfg(test)] mod tests`
   to a reasoned #[expect] that also folds in the test-fixture lints firing inside (indexing
   into result vecs after a length assert, exact-constant float array compares — these are
   fixtures that should fail loudly per POL-001). Sites:
     - src/dispatcher.rs:568  → expect: unwrap_used, indexing_slicing, float_cmp, shadow_unrelated
     - src/store.rs:587       → expect: unwrap_used, indexing_slicing
     - src/run_loop.rs:713    → expect: unwrap_used, indexing_slicing
     - src/rpc.rs:104         → expect: unwrap_used, indexing_slicing
     - src/types.rs:409       → expect: unwrap_used, single_element_loop
   reason e.g. "test fixtures index result vecs after length asserts, unwrap on known-good
   values, and compare exact table constants; failures should surface loudly".
   (Adjust each lint list to exactly what clippy reports for that module.)
   Alternatively, the two dispatcher test shadows (aff/plan rebinds ~620/670) and the
   types single-element loop (~493) may instead be fixed in spirit (rename / `let x = one;`)
   if you prefer — your call, but keep it clean.

C. doc_markdown — wrap bare identifiers in backticks in these doc comments:
   decay.rs:26,74,103,189,216,251(x2); dispatcher.rs:224,313; notify_stream.rs:13;
   pool.rs:9; rpc.rs:11; store.rs:27,28,452.

D. missing_errors_doc / missing_panics_doc — add the doc section:
   notify_stream.rs:26 (# Errors); rpc.rs:81 (# Errors); run_loop.rs:415,446,464 (# Errors);
   store.rs:45 (# Panics — `Counter::next` asserts on seq overflow; document it).

E. Real-source indexing_slicing — fix in spirit:
   - dispatcher.rs:241,333,393 — `row[idx]` inside `for (idx,name) in ATTR_ORDER.iter().enumerate()`.
     Rewrite as `for (name, base) in ATTR_ORDER.iter().zip(row)` (row is [f64;8]; lengths equal).
   - dispatcher.rs:272,273 — two arrays indexed by same idx (new_row[idx], prior_row[idx]).
     Zip all three: ATTR_ORDER.iter().zip(new_row).zip(prior_row).
   - run_loop.rs:705 — same enumerate+index pattern in `union_affected` (row_a[idx]/row_b[idx]);
     zip the same way.
   - dispatcher.rs:502,503 — serde_json `ev["revises"] = ...` IndexMut. Use
     `if let Some(obj) = ev.as_object_mut() { obj.insert("revises".into(), ...); obj.insert("prior_actual".into(), ...); }`.
   - dispatcher.rs:453 — `input.projection[&input.target]` is a DOCUMENTED contract-violation
     panic (caller must seed projection, §17.8). This is the legitimate escape hatch: add
     #[expect(clippy::indexing_slicing, reason = "contract §17.8: caller seeds projection[target]; missing key is a contract violation and the panic is the intended signal")].

F. pub_use — crate facade re-exports. Add per-statement
   #[expect(clippy::pub_use, reason = "crate facade: deliberate public-API re-export")]:
   lib.rs:22,23,24,31,32,37 (each `pub use {...}` block); pool.rs:3 (`pub use sqlx::PgPool`).

G. module_name_repetitions — names are idiomatic; add per-item
   #[expect(clippy::module_name_repetitions, reason = "idiomatic name; <short>")]:
   clock.rs SystemClock(18), FakeClock(31); pool.rs PgPool re-export(3), create_pool(14);
   decay.rs decay_threshold(87), DecayScheduler(114).
   (pool.rs:3 needs BOTH pub_use and module_name_repetitions in one #[expect].)

H. shadow_unrelated (real src) — rename the inner binding:
   store.rs:280 (closure params `scope`/`name` shadow fn args in the `.map(|(scope,name,..)|)` —
   rename closure bindings, e.g. `(r_scope, r_name, ...)`, map into struct fields);
   store.rs:343 (`name` — same idea). (The two dispatcher test shadows are handled in B.)

I. type_complexity — store.rs:262,306,516 are big query_as tuple types. Define `type` aliases
   near the top of store.rs (e.g. `type AttributeRowTuple = (String, String, f64, ...);`) and
   use them at the query_as sites. Name them for what they are.

J. One-offs:
   - decay.rs:295 — clone_on_copy: `snapshot.clone()` where Snapshot: Copy → drop `.clone()`.
   - decay.rs:58 — unnested_or_patterns: `Some("a") | Some("b")` → `Some("a" | "b")`.
   - run_loop.rs:79 — `.or_insert_with(Counter::new)` → `.or_default()` (impl Default for Counter
     if it isn't already; Counter::new likely == Default::default()).
   - rpc.rs:94 — map_err_ignore: `.map_err(|_| format!(...))` discards the parse error. Include
     it: `.map_err(|e| format!("malformed schema_version major: {s:?}: {e}"))`.
   - notify_stream.rs:65 — needless_pass_by_value: `io_to_err(e: std::io::Error)` only formats e.
     Take `&std::io::Error` and update call sites.
   - tuning.rs:85,86 — match_same_arms in hippocampus_base_deltas: merge identical arms with `|`
     (Written|Renamed share one row; Overwritten|Deleted share another). Preserve the column
     comment.

CONSTRAINTS:
  - Match surrounding style (2-space indent, comment density, naming).
  - Don't touch migrations, tests' behavior, or business logic — lint fixes only.
  - When done, end with a "Lint notes:" line per POL-001 §Verification listing every #[expect]
    you added (lint + file + one-word reason) and confirming no config/lint-level changes.

Start by running `just lint` to see the live error set, then work file by file, re-running
after each file. Finish only when `just lint` is silent and `cargo fmt --all --check` passes.

ADDENDUM — second wave in tests/*.rs:
`--all-targets` compiles each tests/*.rs as its own crate linking the lib. While the
lib has errors, those test targets fail to compile and never lint — so your first
`just lint` shows only src/ errors. After src/ is green, a NEW batch surfaces in
tests/{harness,store,decay,dispatcher,run_loop}.rs (indexing/unwrap in fixtures, and a
crate-level `#![allow(clippy::unwrap_used, clippy::expect_used)]` in tests/harness.rs that
must become a reasoned `#![expect(...)]`). Keep running `just lint` until it is fully
silent — do not stop at the first clean pass of src/. Project lint notes live in
.spec-driver/memory/mem.pattern.project.rust-lint-footguns.md.
