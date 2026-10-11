# BSC Node Rust Workspace

## Overview

This repository is a Rust workspace around BNB Chain, used to compare, verify, and experiment with BSC's two client implementations: the official Go implementation `bsc` (geth-bsc) and the official Rust implementation `reth-bsc`.

The workspace follows strict Rust conventions (edition 2024, `eyre`, `tracing`, mandatory clippy lints) and ships a consistency probe, `bsc-consistency-probe`, which cross-checks block headers (notably the `stateRoot`) returned by both clients at the same height, and—in `truth` mode—verifies that account/storage leaves from `eth_getProof` are genuinely committed under the reported `stateRoot` via Merkle proof.

中文版： [README.zh.md](./README.zh.md)

## Workspace Layout

The workspace uses a Cargo workspace layout: executable crates live under `bin/*`, reusable libraries under `crates/*`, and the root `Cargo.toml` aggregates them via `members = ["bin/*", "crates/*"]` while centralizing shared dependencies in the root `[workspace.dependencies]`.

## Crates

- `bin/cli-app` — Example CLI (`clap`) demonstrating workspace conventions.
- `crates/common` — Shared utility library used across crates.
- `bin/bsc-consistency-probe` — Go vs Rust consistency probe (see next section).

## Consistency Probe

### Background

Go `bsc` is the authoritative client actually run by BSC mainnet's 21 validators; Rust `reth-bsc` is currently experimental—it can sync and serve read RPC, but cannot act as a block-producing validator, lacks some finality APIs (e.g. `eth_getFinalizedHeader`), and treats the `safe`/`finalized` tags as `latest`.

Therefore, any critical workload that trusts a Rust node as a state source (e.g. a MEV searcher / DEX aggregator running in-process revm simulation) should cross-validate the `stateRoot` against Go before trusting its data.

### Subcommands

- `compare` — In the height range `[start, end]`, compares Go vs Rust block headers field-by-field at the same height (including `stateRoot` / `receiptsRoot` / `transactionsRoot`), printing per-block diffs and a summary.
- `truth` — Pulls account/storage proofs via `eth_getProof` and verifies, per MPT rules, that the leaves are genuinely committed under the reported `stateRoot`. Pass only `--go` (Go as the trusted source) or both `--go` and `--rust` for a three-way trusted cross-check.

### Examples

```bash
# Cross-validate block headers for heights 38_000_000..=38_000_100 (Go vs Rust)
cargo run -p bsc-consistency-probe -- compare \
  --go   https://bsc-mainnet.bnbchain.org \
  --rust https://your-reth-bsc.example.com \
  --start 38000000 --end 38000100 --workers 8

# Verify a Merkle proof for an account and its storage slots, trusting Go
cargo run -p bsc-consistency-probe -- truth \
  --go https://bsc-mainnet.bnbchain.org \
  --address 0xbb4C0b1500D34CC1b8d8e5f0b1f26a0aE0c1E2d3 \
  --slots 0x0,0x1 --block 38000100

# Three-way trusted check: both Go and Rust act as proof sources
cargo run -p bsc-consistency-probe -- truth \
  --go   https://bsc-mainnet.bnbchain.org \
  --rust https://your-reth-bsc.example.com \
  --address 0xbb4C0b1500D34CC1b8d8e5f0b1f26a0aE0c1E2d3 \
  --slots 0x0 --block 38000100
```

## Development Workflow

This workspace uses `just` for common tasks.

```bash
just setup     # install toolchain and git hooks
just check     # type-check
just test      # run tests (incl. proptest)
just lint      # fmt + sort + clippy -D warnings + typos + rumdl
just build     # build the workspace
just mutation  # cargo-mutants mutation testing
```

## Conventions

The workspace enables clippy `pedantic` (warn) and denies several panic-prone patterns: `unwrap` / `expect` / `panic`, `todo`, `dbg!`, `println!`/`eprintln!` in library code, and `#[allow(...)]` attributes; `unused_must_use` is denied. Library code must use `tracing` instead of `println`. A CLI binary may lift the print lint via `#![expect(clippy::print_stdout)]` at the top of `main.rs`. The HTTP client is uniformly `hpx` (rustls-tls); `reqwest` is forbidden.

## Documentation

In-depth BSC dual-client (Go vs Rust) analysis:

- English: [docs/bsc-diff.md](./docs/bsc-diff.md)
- 中文: [docs/bsc-diff.zh.md](./docs/bsc-diff.zh.md)

## License

Apache-2.0
