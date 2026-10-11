# BSC Private Networks (`privnet/`)

One shared genesis, one script, three network modes: Go-only, Rust-only, or mixed (Go + Rust).

- Shared chain: `genesis.json` — `chainId: 1337`, Parlia `period: 3s`, `epoch: 200`, single development validator `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A` (key `0x1111…1111`, **never use on mainnet/testnet**).
- Script: [`privnet.sh`](./privnet.sh) — starts/stops/checks the networks, wires peers automatically.

Full experiment report (setup, results, bugs found and fixed): [docs/privnet.md](../docs/privnet.md).

## Requirements

- A `geth` binary built from `bnb-chain/bsc` (`make geth`), or set `GETH_BIN`.
- For `rust`/`mixed` modes, a `reth-bsc` binary built from a source tree that contains the private-network fixes (Status-head fix, custom-chain defaults; see [docs/privnet.md](../docs/privnet.md)), or set `RETH_BIN`. Until upstream PR [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546) merges, build from our branch (`https://github.com/akfork/reth-bsc`, branch `fix/fix-privnet`); afterwards an upstream `develop` build works.
- `curl`, `python3`, standard Unix tools. Optional: `cast` (Foundry) for sending test transactions.

## Quick start

```bash
# Go-only: go1 (miner) + go2 (follower)
GETH_BIN=/path/to/geth ./privnet.sh go

# Rust-only: rust1 (miner) + rust2 (follower)
RETH_BIN=/path/to/reth-bsc ./privnet.sh rust

# Mixed: go1 (miner) + go2 + rust1 + rust2 (followers)
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc ./privnet.sh mixed

# Inspect / stop / wipe
./privnet.sh status
./privnet.sh handover-to-rust   # mixed only: rust1 takes over block production
./privnet.sh handover-to-go     # mixed only: go1 takes over block production
./privnet.sh stop
./privnet.sh clean              # stop + wipe chain data
```

`GETH_BIN`/`RETH_BIN` default to `geth`/`reth-bsc` on `PATH`. Runtime data lives in `./data` (override with `PRIVNET_DIR`).

## Topology and ports

| Node | Role | P2P | HTTP RPC | Notes |
|---|---|---|---|---|
| go1 | miner (go/mixed) | 30311 | 8545 | `--miner.recommit 1s` |
| go2 | follower | 30312 | 8547 | `--syncmode full` (see below) |
| rust1 | miner (rust) / follower (mixed) | 30313 | 8549 | authrpc 8555, `-d` (no discovery) |
| rust2 | follower | 30314 | 8553 | authrpc 8556, `-d`, trusted to rust1 |

Peering is automatic: Go nodes are linked with `admin.addPeer`; Rust nodes use `--trusted-peers` (static enodes captured at startup).

## Operational notes (all observed in testing)

- **Go follower needs `--syncmode full`.** The default snap mode fails on tiny private chains with `missing trie node ... layer stale`.
- **Rust nodes need `-d` (no discovery).** A custom chain has no bundled bootnodes, so upstream reth falls back to Ethereum-mainnet bootnodes; the node dials the public internet and gets banned. Fixed binaries also default custom chains to an empty bootnode set.
- **Rust mining must be enabled at startup** (`--mining.enabled --mining.private-key ...`). The `miner_start` RPC is accepted but does not start the sealing loop; Go's `miner_stop`/`miner_start` do work at runtime, which is what `handover-to-go` relies on.
- **`--bootnodes` is ignored under Go's `--nodiscover`.** Go↔Go peering here uses `admin.addPeer`.
- **Chain stalls near block 199.** The shared genesis deploys no system contracts, so neither client can assemble the epoch-200 validator set: Go logs `Failed to prepare header for sealing` and stalls at 199; Rust logs `failed to decode getValidators output` and stalls at 198.
  Both stay alive and keep serving RPC — this is graceful degradation, identical in spirit on both sides. For longer runs, restart from a fresh data directory (same genesis still works: chainId/genesis hash are unchanged, wall-clock timestamps make new blocks).
- **Do not run two miners on the same validator key.** Both clients seal every ~3s and the tip flaps between forks (it converges, but logs fill with reorg noise). Use the handover actions for a single producer.
