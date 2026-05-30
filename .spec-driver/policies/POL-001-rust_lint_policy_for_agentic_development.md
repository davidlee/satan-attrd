---
id: POL-001
title: "POL-001: Rust lint policy for agentic development"
status: required
created: "2026-05-31"
updated: "2026-05-31"
reviewed: "2026-05-31"
owners: [David Lee]
supersedes: []
superseded_by: []
standards: []
specs: []
requirements: []
deltas: []
related_policies: []
related_standards: []
tags: [lint, rust, clippy, agentic]
summary: 'Strict Rust/Clippy lints, zero-warning tree. Suppressions must be narrow, carry a reason, and serve better code. "Pre-existing" is not an exemption.'
---

# POL-001: Rust lint policy for agentic development

## Statement

The tree is warning-clean. `just check` passes with zero lint warnings, always.

1. **Zero tolerance.** Any lint in work you submit fails review — no warnings, no
   exceptions, no "fix it later".

2. **"Pre-existing" is not a disposition.** A lint that fires on a file you
   touched is yours to resolve, regardless of who wrote the line. You may not
   ship a touched file with a lint you chose to ignore. If a lint is genuinely
   out of scope for your change, raise it — do not silently leave or suppress it.

3. **Suppress only to make the code better, never to finish faster.** A
   suppression is a deliberate, documented tradeoff. Disabling a lint to clear a
   build you could have fixed is a policy violation, even with a reason attached.

4. **Use `#[expect]`, not `#[allow]`.** `clippy::allow_attributes` is denied:
   bare `#[allow]` will not compile. Suppress with `#[expect(..., reason = "…")]`,
   which is self-cleaning — it errors when the lint stops firing, so stale
   suppressions cannot rot in the tree.

5. **Every allowance carries a `reason`.** (`allow_attributes_without_reason` is
   denied — it is mechanically required, not merely good manners.) The reason
   states the invariant or tradeoff — why the lint is wrong *here* or why obeying
   it yields worse code. "Clippy complained" is not a reason.

   ```rust
   #[expect(
       clippy::indexing_slicing,
       reason = "Length checked by the parser two lines above; get() would hide that invariant."
   )]
   let tag = bytes[0];
   ```

6. **Narrowest scope.** Attach the attribute to the smallest item, expression, or
   statement. Crate-level (`#![expect]`/`#![allow]`) and module-wide suppressions
   are prohibited without maintainer permission (see Scope).

## Rationale

Strict lints catch correctness, safety, and maintainability defects before
review. Agents under task pressure systematically take the cheap exit —
suppressing a lint, or disowning it as "pre-existing" — which converts a caught
defect back into an uncaught one and erodes the whole gate over time.

This policy makes the cheap exit unavailable while preserving a narrow,
honest escape hatch for the cases where a lint really is wrong. The cost of a
reasoned, scoped allowance is small; the cost of a quietly weakened lint baseline
compounds.

## Scope

Applies to all Rust in the crate. The lint policy is declared in `Cargo.toml`
`[lints.rust]` / `[lints.clippy]` plus `clippy.toml`. Discretion differs by code
class.

**Requires maintainer permission before suppressing** (production code):

```toml
unsafe_code = "forbid"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
as_conversions = "deny"
cast_possible_truncation = "deny"
cast_sign_loss = "deny"
indexing_slicing = "deny"
disallowed_types = "deny"
```

A production suppression of these must explain why error handling, pattern
matching, checked conversion, or safe indexing is the *worse* option here.

**Always requires maintainer permission**, any code:

- editing `[lints]` or `clippy.toml`;
- downgrading any lint (`deny` → `warn`/`allow`);
- crate-level `#![expect(...)]`/`#![allow(...)]` or module-wide suppression.

**Agent discretion** (no prior ask), when scoped to one item *and* reasoned:

- Local allowances that don't weaken safety, security, correctness, or public
  API guarantees.
- Test code may allow `unwrap_used`/`expect_used`/`panic` where a fixture should
  fail loudly — still narrow, still reasoned. Never `todo!()`/`unimplemented!()`
  in committed tests; delete dead helpers rather than allowing `dead_code`.

**Resolution order** when a lint fires: (1) fix in the spirit of the lint;
(2) refactor to make the invariant explicit; (3) use a safer API or tighter
type; (4) add a narrow reasoned `#[expect]` only if the lint is wrong here;
(5) ask permission if the exception is broad or policy-level; (6) propose a
policy change only if repeated legitimate cases prove the rule too strict.

**Proposing a policy change** (don't edit policy files unless asked): state the
lint, current level, proposed level, the bad incentive it creates, affected
examples, the risk of weakening, and a stricter alternative.

## Crate-specific divergences

This crate's lint config is derived from forgettable's `[workspace.lints]` but
diverges in one place, recorded here so the divergence is deliberate, not drift:

- **`disallowed_types` does not ban `HashMap`/`HashSet`.** forgettable bans both
  (BTreeMap iteration determinism). satan-attrd uses `HashMap` intentionally for
  the per-attribute `Counter` and projection maps, where iteration order is not
  observable. The `disallowed_types` lint itself stays `deny` (it still enforces
  the `clippy.toml` `disallowed-methods` list); only the type entries are omitted.

## Verification

- **Gate:** `just check` denies warnings; CI fails on any lint. This is the
  enforcement of zero tolerance — a clean gate means no lint can be "pre-existing".
- **Final-summary disclosure:** any change that adds, removes, or alters a lint
  allowance must report it, e.g. `Lint notes: added local #[expect] for
  clippy::indexing_slicing in parser.rs (length checked above); no crate
  lint levels changed.` If none: `Lint notes: no suppressions or policy changes.`
- **Review rejects:** weakening a lint to pass the build; unexplained suppression;
  broad suppression where narrow would do; a lint hidden rather than fixed; an
  allowance standing in for an unfinished implementation.

## References

- Clippy lint reference: <https://rust-lang.github.io/rust-clippy/>
