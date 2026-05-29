# export DATABASE_URL := "postgres:///satan_memory_test?host=/run/postgresql"
export DATABASE_URL := "postgresql://postgres:postgres@127.0.0.1:54322/postgres"

default: check

# Full gate: lint + format + test
check: lint format test

# All tests (unit + integration). Integration tests require Postgres on $DATABASE_URL.
test: test-unit test-integration

test-unit:
  cargo test --lib --bins

test-integration:
  cargo test --test '*'

# Lint
lint:
  cargo clippy --all-targets --all-features -- \
  -D clippy::unwrap_used \
  -D clippy::expect_used \
  -W clippy::pedantic \
  -A clippy::too_many_lines

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
