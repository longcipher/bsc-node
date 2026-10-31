# BSC Node Agent Instructions

## Non-negotiables

- Surgical edits only: every changed line must trace to the request. No drive-by refactors, no reformatting untouched code, no deleting pre-existing dead code (mention it instead).
- Fix root causes. Never silence a warning, weaken a check, or patch a symptom to make a gate pass.
- Never report completion while any `just` gate below fails. Report the failure verbatim and stop.
- Stop and ask when a request is ambiguous, conflicts with existing conventions, or requires credentials, publishing, or destructive operations.
- When a correction reveals a missing rule, add one specific rule here ("always X for Y", never "be careful with Y"); delete rules that never catch real errors.

## Scope

- This workspace cross-validates the two BSC client implementations: Go `bsc` (geth-bsc) and Rust `reth-bsc`. The only crate is `bin/bsc-consistency-probe`; `crates/` is empty and reserved for future libraries.
- The probe has two subcommands. `compare` diffs block headers field by field at the same height (`stateRoot`, `receiptsRoot`, `transactionsRoot`); `truth` verifies with `eth_getProof` that account and storage leaves are really committed under the reported `stateRoot`.
- `privnet/` is the reproducible private-network lab: the generated `genesis*.json` and `accounts.json` (from `build-genesis.py`), `privnet.sh`, `ab-breathe-interval.sh`. `docs/bsc-diff.md` and `docs/privnet.md` hold the long-form dual-client analysis.
- The `*.zh.md` documents are deliberate Chinese translations. The English files stay English; keep the two in sync when one changes.
- English only in code, comments, docs, and commit messages.

## Execution

- Parallelize aggressively: dispatch subagents for independent research, exploration, and implementation.
- NEVER run two `cargo` commands at once; `target/` is file-locked. Finish edits first, then run cargo.
- NEVER use `git worktree`. Edit the current branch in place.
- TDD for behavior changes: failing test first, smallest change to pass, then run `just mutation` and fix surviving mutants.

## Verification loop

After each feature or bug fix, in order; any failure means the task is not done:

```bash
just format
just lint
just test
just mutation
```

`just lint` is the gate: `typos`, `rumdl check`, `cargo sort -w -g -c` for TOML key order, `cargo +nightly fmt --check`, `cargo +nightly clippy -D warnings` and `cargo shear` for unused dependencies. CI installs exactly `just`, `cargo-shear`, `cargo-sort`, `typos-cli`, `rumdl`, `cargo-llvm-cov` and `cargo-nextest`, so `just lint` must not call anything else.

## Rust probe rules

- `rust-toolchain.toml` pins nightly, so use `cargo +nightly fmt` and `cargo +nightly clippy`. Do not invent stable variants.
- `.cargo/config.toml` sets `rustc-wrapper = "kache"`, which breaks clippy's lints and `--fix`. Invoke clippy with `RUSTC_WRAPPER=` unset, exactly as the Justfile does.
- Workspace lints deny `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro` and `allow_attributes`. The CLI lifts the print lint with `#![expect(clippy::print_stdout)]` at the top of `main.rs`; use `#[expect(lint, reason = "...")]`, never a bare `#[allow(...)]`.
- `clippy.toml` carries the thresholds (`too-many-lines-threshold = 120`, `cognitive-complexity-threshold = 30`, `max-struct-bools = 4`). Keep new code inside them instead of widening the file.
- `.cargo/mutants.toml` deliberately has no `exclude_globs`: an earlier version excluded every source file and silently turned 168 mutants into zero. Do not add exclusion globs, and keep `skip_calls = ["with_capacity"]` as the only skip.

## Dependency choices

- Errors are `eyre` everywhere; this workspace has no `thiserror`. Never introduce `anyhow`.
- The HTTP client is `hpx` with rustls, declared per crate because it is not shared. `reqwest` is forbidden.
- Shared crates live in the root `[workspace.dependencies]` with numeric versions and no features, and are referenced as `dep = { workspace = true }`. Never hand-type a version.
- The trie and hashing stack is fixed: `rlp` for the encoding, `sha3` for hashing, `primitive-types` for `H256`/`U256`. Replacing any of them changes the proof semantics.
- `proptest` covers the MPT and protocol code; a regression file lives at `bin/bsc-consistency-probe/proptest-regressions/mpt.txt`. Keep it committed when a case shrinks.
- Forbidden by default: `anyhow`, `reqwest`, `log`, `dashmap`, `std::sync::mpsc`.

## Probe behaviour rules

- `--workers` bounds the load on a public endpoint: one block probe issues four JSON-RPC calls, so a wide `--start..--end` range consumes capacity in proportion to `--workers`.
- Results are reported in ascending height order no matter which endpoint answers first.
- A height whose probe fails is reported as a diff row of its own. Dropping it would understate the failures the caller asked about.
- Any inconsistent or unprobeable block makes the exit status non-zero, so `compare` works as a CI gate. The summary line is always printed first.
- `--timeout-ms` covers the whole exchange, not just the send, so a node that accepts the connection and then goes quiet fails the block instead of hanging the run.
- `--start` above `--end` is rejected: an empty range would print `0/0 blocks consistent` and exit zero, which is indistinguishable from a pass.
- In `truth` mode, trust comes from the endpoint that serves the proof. Pass `--go` alone for a Go-trusted check, or `--go` and `--rust` for a three-way cross-check.

## Code conventions

- Merkle proof verification follows the MPT rules exactly: `rlp` for the trie encoding, `sha3` for hashing, `primitive-types` for `H256`/`U256`. A shortcut here silently accepts an invalid proof.
- Storage slots and addresses are hex-encoded; keep the `0x` prefix handling in `parse_slot_list` and `parse_address_list` consistent.
- JSON-RPC payloads go through `serde_json::Value`; do not introduce a second JSON stack.
- The crate is a binary, so unit tests live in `#[cfg(test)]` modules and integration tests in `bin/bsc-consistency-probe/tests/`. `mock_rpc.rs` is the test-only server used by those tests.
- `crates/` stays empty until a real library boundary exists; the workspace `members` list is `bin/*`.

## Private network rules

- Genesis files are generated, never hand-edited: `python3 -I privnet/build-genesis.py` regenerates them from the pinned `bsc-genesis-contract` revision, and `--check` (run in CI) fails if a committed file drifts. Profiles: `classic` (`genesis.json`, 3 s, one validator), `fast` (450 ms), `pasteur` (450 ms plus Pasteur), `multi` (three validators, turn length 4). All share `chainId: 1337`.
- Every key in `accounts.json` is DEV-ONLY and public. Never reuse one on mainnet or testnet; the scripts refuse chain ids 56 and 97. Send test transactions from the user accounts, not the validator, whose nonce its own system transactions consume.
- `privnet.sh` modes are `go`, `rust`, `mixed`, `fast`, `pasteur`, `multi` and `canary` (`multi` with go1 as the third validator); the actions are `status`, `health`, `env`, `accounts`, `stop`, `clean`, `handover-to-rust`, `handover-to-go`, `system-contracts` and `genesis-check`. `GETH_BIN`, `RETH_BIN`, `PRIVNET_DIR`, `PORT_OFFSET` and `IPC_DIR` override the defaults. `start` exits non-zero unless `health` passes; exit 77 means go-bsc is missing.
- Never stop nodes with `pkill`/`pgrep` patterns: `privnet.sh stop` verifies each pidfile against `/proc` and waits for the exit. Test against a separate `PRIVNET_DIR` with a `PORT_OFFSET` when another network may be running.
- The Go follower needs `--syncmode full`; the default snap mode fails on tiny chains with `missing trie node`.
- Rust nodes need `-d` (no discovery): a custom chain has no bootnodes, so the node dials the public internet and gets banned.
- Rust mining must be enabled at startup with `--mining.enabled --mining.private-key`. The `miner_start` RPC is accepted but does not start the sealing loop, which is why `handover-to-go` restarts the miner instead.
- Breathe blocks kill the chain, so the script passes `BREATHE_BLOCK_INTERVAL=1e15` to every node. Never change that interval on a running chain: the block layout forks.
- The genesis deploys the system contracts, so the chain runs past every epoch. `BSCValidatorSet`'s init blob must carry the validators' BLS vote addresses, or fast finality stops at block 200 (`cannot collect enough votes`); `health` requires `finalized` to advance in the Rust-only modes.
- Run exactly one miner per validator key. Use the handover actions to move block production.
- `just privnet-verify-breathe` is the A/B regression for the daily breathe block: case A shrinks the interval and the chain must stall, case B proves the shipped default keeps producing with all four nodes agreeing. It runs in its own temp directory on `PORT_OFFSET` 2000 and needs go-bsc.
- Commit to `master`, not a side branch. Whenever an external pin changes (the reth-bsc branch or commit, the go-bsc version, an upstream pull request, the rbuilder branch), update `privnet/DEPENDENCIES.md` and its `.zh.md` in the same commit.
