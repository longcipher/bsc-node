# Dependencies

What `privnet.sh` and the builder end-to-end tests run against, and where each fix lives
upstream. Update this file whenever a pinned branch, commit or pull request changes.

## Versions

| Component | Used by | Source |
|---|---|---|
| reth-bsc | every mode except `go` | [`akfork/reth-bsc` branch `integration/develop`](https://github.com/akfork/reth-bsc/tree/integration/develop), commit `d11e769`, end-to-end tested (the full matrix on `62b4ce1`, before the two state-RPC commits) |
| go-bsc (`geth`) | `go`, `mixed`, `canary` | [`bnb-chain/bsc` v1.8.1](https://github.com/bnb-chain/bsc/releases/tag/v1.8.1) (`947ba1cb1ef05d4a5f71b738113a3d8b4c16f6b7`), unmodified |
| rbuilder | builder end-to-end tests | BSC fork of `flashbots/rbuilder`, branch `bsc-only`: `scripts/privnet-e2e.sh` with `BSC_NODE_DIR` set to this repository. It pins its `reth_bsc` library to `62b4ce1`. |
| System contracts | genesis | pinned `bnb-chain/bsc-genesis-contract` revision, see [GENESIS.md](./GENESIS.md) |

Build reth-bsc:

```bash
git clone -b integration/develop https://github.com/akfork/reth-bsc
cd reth-bsc && cargo build --release --bin reth-bsc
export RETH_BIN=$PWD/target/release/reth-bsc
```

## reth-bsc: `integration/develop`

Upstream `develop` (`738307f`) plus the commits below, grouped by change. Every change to
upstream code is either an open upstream pull request or marked akfork only.

| Commits | Change | Needed for | Upstream |
|---|---|---|---|
| `3509cc3` `0c0e5f0` `926eabf` | A fresh private network starts: the eth Status carries the real launch head, custom chains get their own defaults, bundled chain specs are told apart by genesis | every reth-bsc mode | [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546), open |
| `333f998` `a5513c7` | `rbuilder_*` state RPC: account, code, block hash, state root | rbuilder with `STATE=ipc` or the extended HTTP state source | akfork only |
| `185d81b` | Debug logs for best-bid eviction and lookups | diagnosing bid selection | akfork only |
| `e8b2ff6` `0f32c12` `1d8e985` | `mev_params` reports the bid simulator's real reserve (`BidSimulationLeftOver`: it starts nothing within 120 ms of the block) | builders time their last bid without extra configuration | [bnb-chain/reth-bsc#604](https://github.com/bnb-chain/reth-bsc/pull/604), open |
| `c1617c8` `6bc8721` | A validator reads the BEP-322 bid store again after the submission-deadline wait, and deduplicates candidates by built block | reth-bsc validators seal blocks from `mev_sendBid` bids | [bnb-chain/reth-bsc#599](https://github.com/bnb-chain/reth-bsc/pull/599), open |
| `8eb0604` | A block body carries withdrawals only when its header has a `withdrawalsRoot` | go-bsc nodes syncing from reth-bsc (`canary`) | [bnb-chain/reth-bsc#600](https://github.com/bnb-chain/reth-bsc/pull/600), open |
| `6a04ff5` `7af1ee3` | A bid's block is built towards the miner's gas ceiling, not reth's 30M default, and `mev_params` reports the live ceiling | reth-bsc validators accept full bids (filled to `GasCeil`) | [bnb-chain/reth-bsc#601](https://github.com/bnb-chain/reth-bsc/pull/601), open |
| `c3e171a` | The bid simulator leaves its tokio worker (`block_in_place`) while it re-simulates the best bid, so `mev_sendBid` requests keep moving | a builder's last bid reaches the simulator before its cutoff | [bnb-chain/reth-bsc#603](https://github.com/bnb-chain/reth-bsc/pull/603), open |
| `62b4ce1` | Bids are admitted on their own task, so a better bid stops the re-simulation of the best one, as go-bsc's `newBidLoop` does | full blocks (heavy rows) bid last are simulated before the cutoff | [bnb-chain/reth-bsc#605](https://github.com/bnb-chain/reth-bsc/pull/605), open (stacked on #603) |
| `8a3b53f` `d11e769` | `rbuilder_*` reads that run into an on-disk unwind are retried for 150 ms, then answered with `-32052`: when a reorg replaces a block the node had persisted, reth truncates the changeset files while older reads still use their length, so a read fails ("failed to fill whole buffer") or reth's static-file reader panics ("slice index starts at … but ends at 0"); the reads run on a blocking thread, so the panic no longer drops the connection | the builder's node-side state roots on a network that reorgs persisted blocks (`multi`) | akfork only (the race itself is in reth's storage, not reported upstream) |

All pull requests target upstream `develop`. Once they merge, an upstream `develop`
build runs every mode; the builder tests still need the akfork-only commits above.

## This repository

`master` carries the builder end-to-end work, fast-forwarded from branch
`builder-e2e-hardening` (2026-10-31): pinned genesis profiles with working fast finality,
the hardened `privnet.sh`, BEP-322 bids on go-bsc miners, go-bsc miners that vote, and the
`canary` mode with a configured builder whitelist.

## Verification

Last full pass: 2026-11-01, with reth-bsc `62b4ce1`, go-bsc v1.8.1, this repository's
`master` (`3429df6` plus this file) and rbuilder `bsc-only` (`694c7d0`). All 27 rows of
rbuilder's release matrix (`docs/testing.md` there) pass (the 4-hour soak beside the
others, which ran one at a time): every mode (`fast`, `multi`, `pasteur`, `go`, `mixed`,
`canary`), HTTP and IPC state, `self` and `pay_account` payment, BEP-322 bids and BEP-675
BidBlocks (JSON-RPC and gRPC), blobs, breathe and finality-reward blocks, the BEP-703
payment lane with blocks of 7M-gas orders, node freezes, network faults, a block-producer
crash that loses a block, a 30-minute `canary` soak (2 075 blocks sealed from the
builder's BidBlocks by both clients) and a 4-hour `fast` soak at 50 orders/s (698 220
orders landed, every valid one; 32 006 blocks sealed from the builder's bids, 1 of its
64 007 bids late; builder memory 116–136 MiB). No row had more than 3 builder bids that
a reth-bsc validator never simulated for lack of time (3 of 272, in the IPC heavy-order
row).

That pass found four bugs, now fixed. reth-bsc's `mev_params` reported go-bsc's simulation
reserve, not its own 120 ms cutoff (`e8b2ff6`, #604), and its bid simulator held a tokio
worker while it re-simulated, which held up `mev_sendBid` (`c3e171a`, #603): every last
bid came too late. A better bid could not stop reth-bsc's re-simulation of the best one,
so a full last bid waited behind it (`62b4ce1`, #605). rbuilder sent a full last bid too
late for its 75–106 ms simulation (rbuilder `3117909`).

Since then (2026-11-01), rbuilder's parallel builder, which had never run, was fixed
(rbuilder `96fd974`, `ecffc96`) and passed 21 rows of the matrix and a 20-minute soak with
reth-bsc `62b4ce1`. With the ordering and the parallel builders together,
`multi` with BidBlocks and load failed on reth-bsc's `rbuilder_calculateStateRoot`
answering "failed to fill whole buffer", or dropping the connection when reth's
static-file reader panicked, during an on-disk unwind; with `d11e769` and rbuilder
`1fe99dc` that row passed four times in a row.

The pass before (2026-10-31, reth-bsc `6a04ff5`) found three: rbuilder built for an
in-turn validator Parlia barred after its own out-of-turn block; rbuilder dropped orders
of blocks a reorg (or a crashed node) took back; and reth-bsc built a bid's block towards
30M instead of its gas ceiling (`6a04ff5` above).
