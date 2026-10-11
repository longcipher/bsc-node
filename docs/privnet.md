# BSC Private-Network Experiments: Go vs Rust Interop

中文版： [privnet.zh.md](./privnet.zh.md)

Reproducible setup: [`privnet/`](../privnet/) (shared `genesis.json` + `privnet.sh`).
Related analysis: [bsc-diff.md](./bsc-diff.md).

> Subjects (built 2026-10-11 from local checkouts):
>
> - Round 1: Go `bnb-chain/bsc` @ `c5533ab5b` (binary `v1.8.0-alpha`),
>   Rust `bnb-chain/reth-bsc` @ `98ef6c5` (binary `0.1.2-1-g98ef6c5`).
> - Round 2 (§10): Go @ `8cc60b246` (**develop**), Rust @ `349f55c` (**develop**,
>   binary self-reports `v0.0.8-alpha-198-…` because the `v0.1.x` tags exist only
>   on `main`). Three of the four fixes were re-applied (the decode panic was
>   already fixed upstream); `privnet.sh` ran all modes unchanged.
>
> Conclusions below rest on three pillars: source-code reading, **live private-network
> experiments**, and **live web research verified on 2026-10-11** (releases, fork schedule,
> validator set). The earlier "no live network research" caveat no longer applies.

## 1. Setup

Shared genesis (`privnet/genesis.json`): `chainId: 1337`, Parlia `period: 3s`, `epoch: 200`,
single development validator `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A` (key `0x1111…1111`).
No system contracts are deployed in `alloc` (this matters at the epoch boundary, §5).
Both clients consume the same file: Go via `geth init`, Rust via `--chain <file>`.

| Node | Role | P2P | HTTP RPC |
|---|---|---|---|
| go1 | miner (go/mixed modes) | 30311 | 8545 |
| go2 | follower | 30312 | 8547 |
| rust1 | miner (rust) / follower (mixed) | 30313 | 8549 |
| rust2 | follower | 30314 | 8553 |

Go↔Go peering uses `admin.addPeer` (`--bootnodes` is ignored under `--nodiscover`);
Rust uses `-d` (no discovery) plus `--trusted-peers` with static enodes. Rust mining is
enabled at startup (`--mining.enabled --mining.private-key`); Go mining is toggled at
runtime (`miner_stop`/`miner_start`).

## 2. Test 1 — Go-only network

`./privnet.sh go`: go1 mines, go2 follows.

- go2 synced to the tip within seconds and tracked it (`0x1c`/`0x1c` at check time).
- Legacy transaction (10 gwei gas price, pre-London genesis has no base fee) mined in
  block `0x20`, receipt `status: 0x1` on both nodes.
- Verdict: **pass**. Baseline for the other two modes.

## 3. Test 2 — Rust-only network

`./privnet.sh rust`: rust1 mines from genesis, rust2 follows over `eth/69`.

- **Pre-fix result: total deadlock.** Both nodes stayed at block 0 forever with
  `BlockhashZero` handshake rejections in a ban loop (§6.1). No peer ever connected, so
  the miner's peer gate (`num_peers > 0`) never opened.
- **Post-fix result: pass.** rust1 sealed from block 1 (7 system transactions, same
  structure as a Go-built block 1), rust2 followed (`0x11`/`0x11`, 1 peer each, zero
  `blockhash is zero` errors). Hashes differ from Go's historical run only by wall-clock
  timestamps, as expected.

## 4. Test 3 — Mixed network (Go + Rust)

`./privnet.sh mixed`: go1 mines; go2, rust1, rust2 follow.

- **Handshake matrix (measured from `admin_peers` and node logs):**
  Rust↔Go negotiates **`eth/68`** (Go offers `{70, 68}`, the Rust reth fork offers
  `{69, 68, 67, 66}`); Rust↔Rust negotiates **`eth/69`**. Both pairings complete the
  BSC `UpgradeStatus` (`0x0b`) round-trip on `eth/68`; `bsc/1` + `bsc/2` are also
  negotiated. `net_version` = 1337 on all nodes.
- **State convergence:** blocks 1–198 hash-identical across all four nodes; user
  transactions submitted via Go and via Rust both executed on all nodes with identical
  receipts, balances, and `stateRoot` (e.g. `0x5a` with one root on all four).
- **Miner handover both directions** (`handover-to-rust`, `handover-to-go`): after
  `miner_stop` on go1 and a mining restart of rust1, Rust sealed 13+ blocks that Go
  accepted (identical hashes/state roots everywhere); handing back to Go worked the
  same way. Rust-built blocks carry correct system transactions (`deposit 0xf340fa01`
  to `0x1000`, slash-indicator call to `0x1002`).
- **Same-key dual mining (misconfiguration probe):** running both miners on one key
  makes the tip flap between forks with `need_reorg` churn, but the chain converges
  within a block or two. Keep a single producer in practice.

## 5. Epoch-200 boundary on a contract-less chain

Neither client can assemble the epoch-200 validator set without deployed system contracts:

- Go stalls at block 199, logging `Failed to prepare header for sealing: abi:
  attempting to unmarshal an empty string` on every retry — alive, serving RPC.
- Rust **pre-fix crashed**: `abi_decode_output(&[]).unwrap()` panicked with
  `SolTypes(Overrun)` in the payload-build task at block 199, the engine task died
  (`Fatal error in consensus engine`), the process exited.
- Rust **post-fix degrades exactly like Go**: `failed to decode getValidators output:
  buffer overrun`, no panic, both processes alive and serving RPC (stalls at 198 vs
  Go's 199 — the validator-set cache refresh fires one block earlier on the Rust
  build path; same graceful outcome).

## 6. Bugs found and fixed in `reth-bsc`

All fixes are in the Rust repo; Go is the reference implementation and needed no changes.
They live on branch [`akfork:fix/fix-privnet`](https://github.com/akfork/reth-bsc/tree/fix/fix-privnet),
proposed upstream as [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546);
after the merge, an upstream `develop` build contains them.

### 6.1 Zero-hash initial Status deadlocks fresh networks (critical)

`src/node/network/mod.rs`: `BscNetworkBuilder` advertised the hardcoded
`BscChainSpec::head()` (future block number, **zero blockhash**) in the initial Status
message, ignoring the real launch head (`BuilderContext::head()`, genesis on a fresh
DB — the same default upstream reth uses). Any `eth/69+` peer rejects this with
`BlockhashZero`, so fresh Rust-only networks could never peer (§3). Fixed by
`.set_head(ctx.head())`; fork filter and fork id derive from the same head, so they
stay consistent. Companion fix: `BscChainSpec::head()` anchors custom chains at
genesis (`src/chainspec/mod.rs`), so the Status is correct even before the first block.

### 6.2 System-contract decode panics kill the node (critical)

Six `unpack_*` helpers in `src/system_contracts/mod.rs` used `.unwrap()` on
ABI-decoded contract output; an empty/malformed return (missing contract) panicked the
payload builder and, on import, the engine task (§5). All six now return
`Result<_, SystemContractError>` (new `DecodeOutput`/`MalformedOutput` variants);
`src/node/evm/pre_execution.rs` and `post_execution.rs` propagate the error, except
the EVN NodeID enrichment which stays best-effort. Block building/import now fails
with a log line instead of crashing — Go parity.

> Develop status: upstream fixed this independently (`Option`-based unpack plus an
> explicit `Empty eth call output` / `new_payload returned Invalid` path) — verified
> graceful in the §10 re-run, so this fix was not re-applied there.

### 6.3 `--chain local` advertises the wrong chain id

`src/chainspec/local.rs` hardcoded BSC mainnet (56) while its genesis says 12345, so
Status `networkID` mismatched any peer on the same genesis. Now derived from the
genesis file; dead `head()` removed; tests assert id 12345 and fork-id stability
(`e4d5334c`, unchanged).

### 6.4 Custom chains dial the public internet

`BscChainSpec::bootnodes()` returned `None` for unknown chains, which makes upstream
reth fall back to **Ethereum-mainnet** bootnodes (observed: dozens of outbound dials
and bans). Now resolves to an explicit empty set; `--bootnodes`/`--trusted-peers`
still override.

Refinement (PR review): chain kind alone cannot prove provenance — a custom genesis
that reuses a registered chain id (e.g. 56) still classifies as `ChainKind::Named`
via `Chain::from_id`. Both `bootnodes()` and `head()` are therefore gated on
`BscChainSpec::is_bundled()`, which compares the genesis hash against the bundled
mainnet/testnet/qanet hashes; anything else is treated as custom. Covered by
`test_custom_genesis_with_registered_chain_id_stays_custom`.

### Validation of the fixes

- `cargo test`: round 1 passed 449/449 lib tests; the develop re-run passes 755 lib
  tests (0 failed, 1 ignored), including 4 new regression tests (genesis-anchored
  head with empty custom bootnodes, bundled bootnodes unchanged, local chain id,
  chain-id-56 custom genesis) plus extended fork-id coverage; the
  empty-decode-is-Err case from round 1 is now covered upstream too.
- `cargo clippy -D warnings`: zero new lints in touched files (35 remaining findings
  pre-date the change).
- End-to-end: §3 (post-fix), §4, §5 (post-fix) all re-run against the fixed release
  binary via `privnet.sh` (`go`, `rust`, `mixed`, both handovers, `status`/`stop`/`clean`),
  then repeated on `develop` (§10).

## 7. Go-side observations (no changes needed)

- `authrpc.*` flags are compiled out (`cmd/geth/main.go` comments them out) — BSC embeds
  consensus, there is no beacon Engine API. By design.
- Default snap sync fails on tiny private chains (`missing trie node ... layer stale`);
  `--syncmode full` works. Operational note, already encoded in `privnet.sh`.
- `--bootnodes` is ignored under `--nodiscover`; static peering needs `admin.addPeer`.
- Go also ignores genesis `parlia.period/epoch` (`ParliaConfig` is an empty struct;
  200 blocks / 3000 ms hardcoded with fork-gated changes) — an initial suspicion of a
  Rust gap turned out to be exact parity after code comparison. The suspect code was
  reverted before testing.

## 8. Known remaining gaps (not fixed; out of private-net scope)

- **No seal-authorization check on import.** Rust never verifies that a received block's
  recovered proposer is an authorized validator (Go `verifySeal`: signer∉set,
  difficulty, coinbase match, double-sign cache). Harmless with honest peers; relevant
  for mainnet adversarial robustness.
- **No backfill from quiet peers.** A fresh Rust node does not staged-sync from a
  stalled peer's Status head; it needs live block announcements. Observed when peered
  to a stalled Go node (stayed at 0 until the producer resumed).
- `miner_start` RPC does not start the sealing loop (startup flags required);
  `eth_getTransactionsByBlockNumber` / `eth_getTransactionDataAndReceipt` are absent
  (verified missing; the earlier doc said "to be checked" — now checked).
- Neither client supports LES/light sync; Rust has no snap protocol, graphql,
  ethstats, clef, or devp2p tooling.

## 9. Live-source verification (checked 2026-10-11)

- Go: latest tag `v1.8.0-alpha` (2026-08-13) is an explicit **preview, not for
  production**; latest production line is `v1.7.8`. `v1.8.0-alpha` tracks
  go-ethereum `v1.17.3`, makes `eth/70` primary while keeping `eth/68`, no drop
  announced. (<https://github.com/bnb-chain/bsc/releases>)
- Rust: README still says `Run as validator ❌ (soon)`, but validator-capable
  releases shipped (`v0.0.6-beta`, 2025-12-10, "Validator-Enabling"; latest `v0.1.2`,
  2026-08-20, BEP-675 `mev_sendBidBlock` + Pasteur fix). No public evidence of
  mainnet validators running it; disclaimer still "experimental".
  (<https://github.com/bnb-chain/reth-bsc/releases>)
- Forks all activated on schedule: Fermi mainnet 2026-01-14, Osaka/Mendel 2026-04-28,
  Pasteur 2026-08-25 (testnets ~1–2 months earlier each).
  (<https://docs.bnbchain.org/announce/fermi-bsc/>,
  .../mendel-bsc/, .../pasteur-bsc/)
- Validator set is **45 active** (21 Cabinets + 24 Candidates), not "21 only"; the
  per-epoch consensus set is 21.
  (<https://docs.bnbchain.org/bnb-smart-chain/validator/overview/>)
- `bsc-snapshots` still publishes reth snapshots monthly (v2 layout, multi-TiB).
  (<https://github.com/bnb-chain/bsc-snapshots/>)

## 10. Re-run on `develop` branches (2026-10-11)

All of §2–§5 was repeated against Go @ `8cc60b246` and Rust @ `349f55c` (both
`develop`), re-applying the three still-needed fixes (§6.1, §6.3, §6.4):

- Mixed 2+2: identical results — same handshake matrix (`eth/68` vs Go, `eth/69`
  Rust-to-Rust), hash/stateRoot convergence, transactions both directions, both
  miner handovers via `privnet.sh`.
- Rust-only from genesis: peers and mines with zero `blockhash is zero` errors.
- Epoch-200 boundary: both sides stall gracefully with zero panics. The decode
  panic (§6.2) is **already fixed upstream** (`Option`-based unpack plus explicit
  `Empty eth call output` / `new_payload returned Invalid` logging — same graceful
  outcome as the fix validated in round 1, so it was not re-applied).
- New on develop, observed live: `parlia_getValidators`/`getFinalizedNumber` work;
  `admin_nodeInfo` answers; `traceBlock*` (OpenEthereum-style, `debug` module) and
  `eth_createAccessList` are wired (`eth_createAccessList` rejects pre-London types
  on this chain, as expected); `safe`/`finalized` parlia tags now resolve from real
  canonical markers instead of substituting `latest` (see bsc-diff §5.3).
- Jenner (BEP-706/714) exists on develop but is **qanet-only** (activated 2026-10-05);
  mainnet/testnet do not schedule it, and BEP-714 backoff changes are mirrored on
  both clients — no interop impact on these tests (no timestamp forks configured).
