# export DATABASE_URL := "postgres:///satan_memory_test?host=/run/postgresql"
# Tests treat DATABASE_URL purely as a server/role pointer: the harness
# (tests/common/mod.rs) ignores the db component and provisions a fresh
# disposable `satan_attrd_test_*` database per test.
export DATABASE_URL := "postgresql://postgres:postgres@127.0.0.1:54322/postgres"

default: check

# Build this root's quotient forms and link them where quotient looks
# (.quotient/forms/<capability>). A rebuild that changes a form needs a fresh
# `quotient allow`.
quotient-forms:
    mkdir -p .quotient/forms
    nix build -o .quotient/result-read .#quotient-read
    nix build -o .quotient/result-write .#quotient-write
    ln -sfn "$(readlink -f .quotient/result-read)/bin/quotient-read" .quotient/forms/read
    ln -sfn "$(readlink -f .quotient/result-write)/bin/quotient-write" .quotient/forms/write

# Full gate: lint + format + test
check: lint format test

# All tests (unit + integration). Integration tests require Postgres on $DATABASE_URL.
test: test-unit test-integration

test-unit:
  cargo test --lib --bins

test-integration:
  cargo test --test '*'

# Lint with zero tolerance (policy lives in Cargo.toml [lints] + clippy.toml).
lint:
  cargo clippy --all-targets --all-features -- -D warnings

# Format check
format:
  cargo fmt --all --check

# Format fix
format-fix:
  cargo fmt --all

# Build release binary
build:
  cargo build --release

# Run migrations against $DATABASE_URL (T-attr-1b)
migrate:
  cargo run --release -- migrate

# Run daemon (T-attr-1b+)
run:
  cargo run --release
