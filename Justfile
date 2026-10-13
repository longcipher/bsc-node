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
  cargo workspace-inheritance-check --fix

# Run all lints
lint:
  typos
  rumdl check .
  cargo sort -w -g -c
  cargo +nightly fmt --all -- --check
  RUSTC_WRAPPER= cargo +nightly clippy --all -- -D warnings
  cargo shear
  cargo workspace-inheritance-check --promotion-failure

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

# Run the Go vs Rust consistency probe (compare recent block headers, one-click).
# Override RPC endpoints with GO_RPC / RUST_RPC, or the block count with BLOCKS.
# TIMEOUT_MS is the per-request timeout; lower it to fail fast on a wedged node.
probe GO_RPC="https://bsc-mainnet.bnbchain.org" RUST_RPC="http://localhost:8545" BLOCKS="20" WORKERS="8" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- compare \
    --go {{GO_RPC}} --rust {{RUST_RPC}} \
    --blocks {{BLOCKS}} --workers {{WORKERS}} --timeout-ms {{TIMEOUT_MS}}

# Run the probe over an explicit height range START..=END.
probe-range START END GO_RPC="https://bsc-mainnet.bnbchain.org" RUST_RPC="http://localhost:8545" WORKERS="8" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- compare \
    --go {{GO_RPC}} --rust {{RUST_RPC}} \
    --start {{START}} --end {{END}} --workers {{WORKERS}} --timeout-ms {{TIMEOUT_MS}}

# Run cryptographic truth verification (eth_getProof Merkle proofs) against a Go endpoint.
# Override with GO_RPC / BLOCK / STORAGE_SLOTS / TIMEOUT_MS
# (uses the probe's default major-contract addresses).
probe-truth GO_RPC="https://bsc-mainnet.bnbchain.org" BLOCK="latest" STORAGE_SLOTS="0,1,2" TIMEOUT_MS="10000":
  cargo run -q -p bsc-consistency-probe -- truth \
    --go {{GO_RPC}} --block {{BLOCK}} --storage-slots {{STORAGE_SLOTS}} --timeout-ms {{TIMEOUT_MS}}

# ============================================================
# BSC Private Networks (privnet/)
# ============================================================

# Start a private network: MODE is go, rust, or mixed (binaries via GETH_BIN / RETH_BIN or PATH).
privnet MODE="mixed":
  ./privnet/privnet.sh {{MODE}}

# Show block height / peer count of a running private network.
privnet-status:
  ./privnet/privnet.sh status

# Hand the block producer role to rust1 (mixed mode).
privnet-handover-to-rust:
  ./privnet/privnet.sh handover-to-rust

# Hand the block producer role back to go1 (mixed mode).
privnet-handover-to-go:
  ./privnet/privnet.sh handover-to-go

# Stop a running private network.
privnet-stop:
  ./privnet/privnet.sh stop

# Stop and wipe private-network chain data.
privnet-clean:
  ./privnet/privnet.sh clean

# Publish all crates to crates.io (dry run)
publish-check:
  cargo publish --workspace --dry-run --allow-dirty

# Publish all crates to crates.io
publish:
  cargo publish --workspace

# Check for Chinese characters in the English-only sources
#
# The `.zh.md` documents are deliberately Chinese, so they are excluded: without
# this the recipe reports ~300 intentional hits and never comes back clean,
# which makes it useless for finding a stray CJK comment in a Rust file. The
# few hits it does report are the lines that name each English document's
# translation, which cannot be written in English and are meant to be there.
check-cn:
  rg --line-number --column "\p{Han}" -g '!*.zh.md'

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
  cargo install typos-cli
  cargo install rumdl
  cargo install leptosfmt
  cargo install cargo-llvm-cov
  cargo install cargo-nextest
  cargo install cargo-deny
  cargo install cargo-audit

# Generate documentation for the workspace
docs:
  cargo doc --no-deps --open

# Run security audit
audit:
  cargo audit

# Check for outdated dependencies
outdated:
  cargo outdated

# Run clippy with all lints
clippy-all:
  cargo +nightly clippy --all-targets --all-features -- -W clippy::pedantic -W clippy::nursery -W clippy::cargo

# Run tests with nextest
test-nextest:
  cargo nextest run --all-features

# Run miri on unsafe code
miri:
  cargo +nightly miri test --all-features

# Check for unused dependencies
unused-deps:
  cargo shear --check

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
