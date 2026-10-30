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
  cargo shear

# Run tests
test:
  cargo test --all-features

# Run mutation tests with cargo-mutants
mutation:
  cargo mutants

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
  cargo install cargo-shear
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

# Check for unused dependencies
unused-deps:
  cargo shear --check

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
