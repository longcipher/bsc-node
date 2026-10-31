# Dependencies

What `privnet.sh` and the builder end-to-end tests run against, and where each fix lives
upstream. Update this file whenever a pinned branch, commit or pull request changes.

## Versions

| Component | Used by | Source |
|---|---|---|
| reth-bsc | every mode except `go` | [`akfork/reth-bsc` branch `integration/develop`](https://github.com/akfork/reth-bsc/tree/integration/develop), commit `7af1ee3` (end-to-end tested at `6a04ff5`) |
| go-bsc (`geth`) | `go`, `mixed`, `canary` | [`bnb-chain/bsc` v1.8.1](https://github.com/bnb-chain/bsc/releases/tag/v1.8.1) (`947ba1cb1ef05d4a5f71b738113a3d8b4c16f6b7`), unmodified |
| rbuilder | builder end-to-end tests | BSC fork of `flashbots/rbuilder`, branch `bsc-only`: `scripts/privnet-e2e.sh` with `BSC_NODE_DIR` set to this repository. It pins its `reth_bsc` library to `6a04ff5`. |
| System contracts | genesis | pinned `bnb-chain/bsc-genesis-contract` revision, see [GENESIS.md](./GENESIS.md) |

Build reth-bsc:

```bash
git clone -b integration/develop https://github.com/akfork/reth-bsc
cd reth-bsc && cargo build --release --bin reth-bsc
export RETH_BIN=$PWD/target/release/reth-bsc
```

## reth-bsc: `integration/develop`

Upstream `develop` (`738307f`) plus the commits below, in this order. Every change to
upstream code is either an open upstream pull request or marked akfork only.

| Commits | Change | Needed for | Upstream |
|---|---|---|---|
| `3509cc3` `0c0e5f0` `926eabf` | A fresh private network starts: the eth Status carries the real launch head, custom chains get their own defaults, bundled chain specs are told apart by genesis | every reth-bsc mode | [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546), open |
| `333f998` `a5513c7` | `rbuilder_*` state RPC: account, code, block hash, state root | rbuilder with `STATE=ipc` or the extended HTTP state source | akfork only |
| `185d81b` | Debug logs for best-bid eviction and lookups | diagnosing bid selection | akfork only |
| `e8b2ff6` `0f32c12` | `mev_params` reports the bid simulator's real reserve (`BidSimulationLeftOver`) | builders time their last bid without extra configuration | akfork only, no pull request yet |
| `c1617c8` `6bc8721` | A validator reads the BEP-322 bid store again after the submission-deadline wait, and deduplicates candidates by built block | reth-bsc validators seal blocks from `mev_sendBid` bids | [bnb-chain/reth-bsc#599](https://github.com/bnb-chain/reth-bsc/pull/599), open |
| `8eb0604` | A block body carries withdrawals only when its header has a `withdrawalsRoot` | go-bsc nodes syncing from reth-bsc (`canary`) | [bnb-chain/reth-bsc#600](https://github.com/bnb-chain/reth-bsc/pull/600), open |
| `6a04ff5` `7af1ee3` | A bid's block is built towards the miner's gas ceiling, not reth's 30M default, and `mev_params` reports the live ceiling | reth-bsc validators accept full bids (filled to `GasCeil`) | [bnb-chain/reth-bsc#601](https://github.com/bnb-chain/reth-bsc/pull/601), open |

All pull requests target upstream `develop`. Once they merge, an upstream `develop`
build runs every mode; the builder tests still need the akfork-only commits above.

## This repository

`master` carries the builder end-to-end work, fast-forwarded from branch
`builder-e2e-hardening` (2026-10-31): pinned genesis profiles with working fast finality,
the hardened `privnet.sh`, BEP-322 bids on go-bsc miners, go-bsc miners that vote, and the
`canary` mode with a configured builder whitelist.

## Verification

Last full pass: 2026-10-31, with reth-bsc `6a04ff5`, go-bsc v1.8.1, this repository's
`master` (`d625c84` plus this file) and rbuilder `bsc-only` (`1fece6f`, the heavy-order
rows with its load tool from `4674f82`). All 27 rows of rbuilder's release matrix
(`docs/testing.md` there) pass (the 4-hour soak beside the others, which ran one at a
time): every mode (`fast`, `multi`, `pasteur`,
`go`, `mixed`, `canary`), HTTP and IPC state, `self` and `pay_account` payment, BEP-322
bids and BEP-675 BidBlocks (JSON-RPC and gRPC), blobs, breathe and finality-reward
blocks, the BEP-703 payment lane, node freezes, network faults, a block-producer crash
that loses two blocks, a 30-minute `canary` soak (1 876 blocks sealed from the builder's
BidBlocks by both clients) and a 4-hour `fast` soak at 50 orders/s (698 192 orders
landed, every valid one; 32 005 blocks sealed from the builder's bids; builder memory
152–165 MiB).

That pass found three bugs, now fixed: rbuilder built for an in-turn validator Parlia
barred after its own out-of-turn block; rbuilder dropped orders of blocks a reorg (or a
crashed node) took back; and reth-bsc built a bid's block towards 30M instead of its gas
ceiling (`6a04ff5` above).
