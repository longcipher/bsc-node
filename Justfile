# Default recipe to display help
default:
  @just --list

# Format all code
format:
  rumdl fmt .
  cargo sort -w -g
  cargo +nightly fmt --all

# Auto-fix linting issues
fix:
  rumdl check --fix .
  RUSTC_WRAPPER= cargo +nightly clippy --fix --all --allow-dirty

# Run all lints
lint:
  typos
  rumdl check .
  cargo sort -w -g -c
  cargo +nightly fmt --all -- --check
  RUSTC_WRAPPER= cargo +nightly clippy --all -- -D warnings
  just check-mutation-scope

# Run the prek commit hooks on every tracked file (requires prek)
hooks:
  prek run --all-files

# Run tests
test:
  cargo test --all-features

# Mutation testing is intentionally absent from the default loop and from CI.
#
# Whole-workspace mutation runs are PROHIBITED here: a `cargo mutants` run over
# the whole workspace takes hours and rarely finishes, so it is never a gate and
# never a completion check. Only the lines the current change touched are
# mutated, within the /pb-build budget: at most 3 files, 60 mutants, 300 s.
#
#   just mutation                            # every changed .rs file
#   just mutation crates/core/src/parser.rs  # one specific file
#
# The budget is verified before the run starts, so an over-large change fails
# immediately instead of grinding for hours. A surviving mutant is a test
# failure: strengthen the assertion, or record an exemption in
# `.cargo/mutants.toml` under `exclude_re` with a one-line reason.
#
# Mutate the lines this change touched, within the /pb-build budget.
mutation *args:
  #!/usr/bin/env bash
  set -euo pipefail
  max_files=3
  max_mutants=60
  max_seconds=300

  base=""
  upstream=$(git rev-parse --abbrev-ref --symbolic-full-name '@{upstream}' 2>/dev/null || true)
  for ref in "$upstream" origin/main origin/master main master; do
    [ -n "$ref" ] || continue
    if git rev-parse --verify --quiet "$ref" >/dev/null 2>&1; then
      base=$(git merge-base HEAD "$ref" 2>/dev/null || true)
      [ -n "$base" ] && break
    fi
  done
  [ -n "$base" ] || base=HEAD

  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  if [ "$#" -gt 0 ]; then
    printf '%s\n' "$@" | sed '/^[[:space:]]*$/d' > "$work/files"
  else
    git diff --name-only --diff-filter=ACMR "$base" -- '*.rs' | sort -u > "$work/files"
  fi
  file_count=$(grep -c . "$work/files" || true)
  if [ "$file_count" -eq 0 ]; then
    echo "error: no changed Rust files against $base; there is nothing to mutate." >&2
    echo "       Whole-workspace mutation is prohibited: it takes hours and rarely finishes." >&2
    exit 2
  fi
  if [ "$file_count" -gt "$max_files" ]; then
    echo "error: $file_count files exceed the ${max_files}-file mutation budget." >&2
    echo "       Mutate fewer files at a time; never raise the budget, never run the workspace." >&2
    exit 3
  fi

  : > "$work/diff"
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    git diff --no-ext-diff --no-textconv "$base" -- "$file" >> "$work/diff" || true
  done < "$work/files"
  if ! grep -q '^diff --git ' "$work/diff"; then
    echo "error: the diff against $base is empty; there is nothing to mutate." >&2
    exit 2
  fi
  RUSTC_WRAPPER= cargo mutants --workspace --in-diff "$work/diff" --list --no-times > "$work/list" 2>&1 \
    || { cat "$work/list" >&2; echo "error: cargo-mutants could not list the mutants." >&2; exit 1; }
  mutant_count=$(grep -Ec ':[0-9]+:[0-9]+:' "$work/list" || true)
  if [ "$mutant_count" -gt "$max_mutants" ]; then
    echo "error: $mutant_count mutants exceed the ${max_mutants}-mutant budget." >&2
    echo "       Narrow the change; never raise the budget, never fall back to a workspace run." >&2
    exit 3
  fi
  if [ "$mutant_count" -eq 0 ]; then
    echo "ok: no mutants in the changed lines."
    exit 0
  fi

  echo "mutating $mutant_count mutant(s) in $file_count file(s) (budget: $max_mutants mutants / ${max_seconds}s)"
  if command -v timeout >/dev/null 2>&1; then
    timeout "$max_seconds" env RUSTC_WRAPPER= cargo mutants --workspace --in-diff "$work/diff" --timeout-multiplier 2 --no-times
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$max_seconds" env RUSTC_WRAPPER= cargo mutants --workspace --in-diff "$work/diff" --timeout-multiplier 2 --no-times
  else
    echo "warning: neither 'timeout' nor 'gtimeout' is available; the ${max_seconds}s wall-clock cap is not enforced." >&2
    echo "         Install coreutils ('brew install coreutils') to get 'gtimeout'." >&2
    env RUSTC_WRAPPER= cargo mutants --workspace --in-diff "$work/diff" --timeout-multiplier 2 --no-times
  fi

# Fail if an unbounded mutation command reappears in a build or CI file.
# Whole-workspace mutation runs take hours and rarely finish, so they are
# prohibited: the only permitted forms are a diff-scoped (`--in-diff`), a
# file-scoped (`--file`), or a branch-scoped (`--since`) run.
#
# Check that no build or CI file regains an unbounded mutation command.
check-mutation-scope:
  #!/usr/bin/env bash
  set -euo pipefail
  status=0
  for file in Justfile justfile Makefile makefile .github/workflows/*.yml .github/workflows/*.yaml; do
    [ -f "$file" ] || continue
    offenders=$(grep -nE 'cargo[[:space:]]+mutants' "$file" \
      | grep -vE '^[0-9]+:[[:space:]]*#' \
      | grep -vE -- '--in-diff|--file|--list|--since|-f[[:space:]]' \
      || true)
    if [ -n "$offenders" ]; then
      echo "error: $file runs an unbounded mutation command:" >&2
      printf '%s\n' "$offenders" >&2
      echo "       Whole-workspace mutation runs are prohibited. Use the diff-scoped 'just mutation'." >&2
      status=1
    fi
  done
  exit "$status"

# Run tests with coverage
test-coverage:
  mkdir -p coverage
  cargo llvm-cov --all-features --workspace --lcov --output-path coverage/lcov.info

# Build entire workspace
build:
  cargo build --workspace

# Check all targets compile
check:
  cargo check --all-targets --all-features

# ============================================================
# BSC Consistency Probe
# ============================================================

# Runs the consistency probe over the most recent blocks.
#
# Override RPC endpoints with GO_RPC / RUST_RPC, or the block count with BLOCKS.
# TIMEOUT_MS is the per-request timeout; lower it to fail fast on a wedged node.

# Compare recent block headers between a Go and a Rust endpoint
probe GO_RPC="https://bsc-mainnet.bnbchain.org" RUST_RPC="http://localhost:8545" BLOCKS="20" WORKERS="8" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- compare \
    --go {{GO_RPC}} --rust {{RUST_RPC}} \
    --blocks {{BLOCKS}} --workers {{WORKERS}} --timeout-ms {{TIMEOUT_MS}}

# Run the probe over an explicit height range START..=END.
probe-range START END GO_RPC="https://bsc-mainnet.bnbchain.org" RUST_RPC="http://localhost:8545" WORKERS="8" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- compare \
    --go {{GO_RPC}} --rust {{RUST_RPC}} \
    --start {{START}} --end {{END}} --workers {{WORKERS}} --timeout-ms {{TIMEOUT_MS}}

# Verifies eth_getProof Merkle proofs against a Go endpoint.
#
# Override with GO_RPC / BLOCK / STORAGE_SLOTS / TIMEOUT_MS
# (uses the probe's default major-contract addresses).

# Verify cryptographic truth against a Go endpoint
probe-truth GO_RPC="https://bsc-mainnet.bnbchain.org" BLOCK="latest" STORAGE_SLOTS="0,1,2" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- truth \
    --go {{GO_RPC}} --block {{BLOCK}} --storage-slots {{STORAGE_SLOTS}} --timeout-ms {{TIMEOUT_MS}}

# ============================================================
# BSC Private Networks (privnet/)
# ============================================================

# Start a private network: MODE is go, rust, mixed, fast, pasteur or multi (binaries via GETH_BIN / RETH_BIN or PATH).
privnet MODE="mixed":
  ./privnet/privnet.sh {{MODE}}

# Rust-only network on mainnet timing: Lorentz/Maxwell/Fermi at genesis, 450 ms blocks.
privnet-fast:
  ./privnet/privnet.sh fast

# Rust-only 450 ms network with Osaka/Mendel/Pasteur too, so BEP-675 BidBlockEnabled is true.
privnet-pasteur:
  ./privnet/privnet.sh pasteur

# Three Rust validators (turn length 4, 450 ms) for builder tests across turn boundaries.
privnet-multi:
  ./privnet/privnet.sh multi

# Show block height / finalized / peer count of a running private network.
privnet-status:
  ./privnet/privnet.sh status

# Verify a running network (head advances, peers, agreement, finality); non-zero if unhealthy.
privnet-health:
  ./privnet/privnet.sh health

# Print shell exports (RPC/IPC endpoints, DEV-ONLY test keys) for a builder e2e script.
privnet-env:
  ./privnet/privnet.sh env

# List the DEV-ONLY validator, builder and test accounts the genesis files fund.
privnet-accounts:
  ./privnet/privnet.sh accounts

# Hand the block producer role to rust1 (mixed mode).
privnet-handover-to-rust:
  ./privnet/privnet.sh handover-to-rust

# Hand the block producer role back to go1 (mixed mode).
privnet-handover-to-go:
  ./privnet/privnet.sh handover-to-go

# Regenerate every privnet genesis file from the pinned bnb-chain/bsc-genesis-contract (network stopped).
privnet-system-contracts:
  ./privnet/privnet.sh system-contracts

# Check the committed genesis files are reproducible from the pinned upstream revision.
privnet-genesis-check:
  python3 -I privnet/build-genesis.py --check

# Lint the privnet harness: shellcheck every script, compile and ruff-check build-genesis.py.
privnet-lint:
  git ls-files -z '*.sh' | xargs -0 shellcheck
  python3 -I -m py_compile privnet/build-genesis.py
  ruff check --line-length 120 privnet/build-genesis.py

# Stop a running private network.
privnet-stop:
  ./privnet/privnet.sh stop

# Stop and wipe private-network chain data, vote journals and IPC sockets.
privnet-clean:
  ./privnet/privnet.sh clean

# A/B regression for the daily breathe block.
#
# Case A shrinks the interval so the chain must stall; case B runs the shipped
# default and proves it keeps producing, with all four nodes agreeing. Needs
# go-bsc (exits 77 without it) and runs in its own temp dir on PORT_OFFSET 2000,
# so a network already running from privnet/data is untouched.
# See privnet/README.md.

# Run the breathe-block A/B regression check
privnet-verify-breathe:
  ./privnet/ab-breathe-interval.sh

# Only the "must keep producing blocks" side of the breathe-block check
# (~2 minutes). Use this when you only changed the fix itself.

# Run the fast half of the breathe-block regression check
privnet-verify-breathe-fast:
  ./privnet/ab-breathe-interval.sh --case-b

# Publish all crates to crates.io (dry run)
publish-check:
  cargo publish --workspace --dry-run --allow-dirty

# Publish all crates to crates.io
publish:
  cargo publish --workspace

# Scans the code and scripts for Chinese characters.
#
# The `*.zh.md` documents are deliberately Chinese, and the English files only
# name their translation, so the scan is limited to the code and the scripts.

# Fail if Chinese characters appear in the English-only sources
check-cn:
  ! rg --line-number --column "\p{Han}" --glob '*.rs' --glob '*.sh' bin privnet

# Full CI check
ci: lint test build

# ============================================================
# Maintenance & Tools
# ============================================================

# Clean build artifacts
clean:
  cargo clean

# Install all required development tools
setup:
  cargo install cargo-mutants
  cargo install cargo-sort
  cargo install cargo-workspace-inheritance-check
  cargo install cargo-llvm-cov
  cargo install cargo-nextest
  cargo install cargo-deny
  cargo install cargo-audit
  cargo install typos-cli
  cargo install rumdl

# Generate documentation for the workspace
docs:
  cargo doc --no-deps --open

# Run security audit
audit:
  cargo audit

# Checks for outdated dependencies (needs `cargo install cargo-outdated`)

# Check for outdated dependencies
outdated:
  cargo outdated

# Run clippy with all lints
clippy-all:
  RUSTC_WRAPPER= cargo +nightly clippy --all-targets --all-features -- -W clippy::pedantic -W clippy::nursery -W clippy::cargo

# Run tests with nextest
test-nextest:
  cargo nextest run --all-features

# Run miri on unsafe code (needs `rustup +nightly component add miri`)
miri:
  cargo +nightly miri test --all-features

# Check that workspace inheritance is used consistently (local only)
inheritance-check:
  cargo workspace-inheritance-check --promotion-failure

# Update all dependencies
update-deps:
  cargo update

# Check licenses
license-check:
  cargo deny check licenses

# Check advisories
advisory-check:
  cargo deny check advisories

# Check bans
ban-check:
  cargo deny check bans

# Full check (lint + test + build + audit)
full-check: lint test build audit
