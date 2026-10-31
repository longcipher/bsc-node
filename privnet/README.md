# BSC Private Networks (`privnet/`)

Generated genesis files, one script, six network modes: Go-only, Rust-only and mixed
(Go + Rust) on 3-second blocks, plus Rust-only networks on mainnet timing (450 ms
blocks, Pasteur, and three validators with turn length 4) for block-builder end-to-end
tests.

- Chain: `chainId: 1337`, the BSC system contracts from a pinned
  `bnb-chain/bsc-genesis-contract` revision, and Luban-era BLS vote addresses, so fast
  finality runs and `finalized` advances. See [GENESIS.md](./GENESIS.md).
- Script: [`privnet.sh`](./privnet.sh) starts, verifies, stops and wipes networks and
  wires the peers.
- **DEV-ONLY keys.** Every validator, BLS, builder and test-account key is public
  ([`accounts.json`](./accounts.json)). Never fund them on BSC mainnet (56) or testnet
  (97). The scripts refuse those chain ids.

Full experiment report (setup, results, bugs found and fixed): [docs/privnet.md](../docs/privnet.md).

## Requirements

- For `go`/`mixed`: a `geth` binary built from `bnb-chain/bsc` (`make geth`), or set
  `GETH_BIN`. Without it these modes exit with code 77 (skipped) and say so.
- For every other mode: a `reth-bsc` binary that contains the private-network fixes
  (Status-head fix, custom-chain defaults; see [docs/privnet.md](../docs/privnet.md)),
  or set `RETH_BIN`. Until upstream PR
  [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546) merges,
  build from our branch (`https://github.com/akfork/reth-bsc`, branch
  `fix/fix-privnet`); afterwards an upstream `develop` build works.
- `bash` 4.4+, `curl`, `python3`, standard Unix tools. Optional: `cast` (Foundry) for
  test transactions.

## Quick start

```bash
# Rust-only, 3 s blocks: rust1 (miner) + rust2 (follower)
RETH_BIN=/path/to/reth-bsc ./privnet.sh rust

# Rust-only on mainnet timing (450 ms), with Pasteur (BEP-675), or three validators
RETH_BIN=/path/to/reth-bsc ./privnet.sh fast
RETH_BIN=/path/to/reth-bsc ./privnet.sh pasteur
RETH_BIN=/path/to/reth-bsc ./privnet.sh multi

# Go-only and mixed (need go-bsc)
GETH_BIN=/path/to/geth ./privnet.sh go
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc ./privnet.sh mixed

# Both clients as validators, like BSC testnet, with the builder whitelisted
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc \
  BUILDERS=0x7564105E977516C53bE337314c7E53838967bDaC ./privnet.sh canary

# Inspect / verify / stop / wipe
./privnet.sh status
./privnet.sh health             # exits non-zero unless the network is healthy
./privnet.sh env                # exports for a builder e2e script
./privnet.sh accounts           # the DEV-ONLY test accounts
./privnet.sh handover-to-rust   # mixed only: rust1 takes over block production
./privnet.sh handover-to-go     # mixed only: go1 takes over block production
./privnet.sh stop
./privnet.sh clean              # stop + wipe chain data, vote journals, IPC sockets
```

`GETH_BIN`/`RETH_BIN` default to `geth`/`reth-bsc` on `PATH`. Runtime data lives in
`./data` (override with `PRIVNET_DIR`). `start` returns only after the health check
below passes, and exits non-zero (stopping the nodes it started) if it does not.
Run `./privnet.sh --help` for every action and variable.

## Modes and genesis profiles

| Mode | Nodes | Genesis (profile) | Blocks | Notes |
|---|---|---|---|---|
| `go` | go1 (miner), go2 | `genesis.json` (classic) | 3 s | needs go-bsc |
| `rust` | rust1 (miner), rust2 | `genesis.json` (classic) | 3 s | |
| `mixed` | go1 (miner), go2, rust1, rust2 | `genesis.json` (classic) | 3 s | needs go-bsc; handovers |
| `fast` | rust1 (miner), rust2 | `genesis-fast.json` (fast) | 450 ms | Lorentz, Maxwell, Fermi at genesis |
| `pasteur` | rust1 (miner), rust2 | `genesis-pasteur.json` (pasteur) | 450 ms | + Osaka, Mendel, Pasteur: `BidBlockEnabled: true` |
| `multi` | rust1, rust2, rust3 (all miners) | `genesis-multi.json` (multi) | 450 ms | 3 validators, turn length 4, Pasteur forks |
| `canary` | rust1, rust2, go1 (all miners) | `genesis-multi.json` (multi) | 450 ms | needs go-bsc; `multi` with validator 3 on go-bsc |

`PROFILE=fast` (or `pasteur`) runs `go`, `rust` or `mixed` on that genesis instead; it
is untested with go-bsc. The fast profiles reproduce mainnet's schedule: 1.5 s, 0.75 s
and then 0.45 s block intervals from genesis, a 500-block epoch from block 500 and a
1000-block epoch from block 1000. BidBlock admission needs Pasteur *at the head*, which
the genesis gates on London (block 8), so `mev_params` reports `BidBlockEnabled: false`
for the first 8 blocks.

In `multi`, blocks 1-200 rotate one block per validator (the genesis snapshot's turn
length is 1, as on BSC). From the first epoch boundary the set and turn length come
from `BSCValidatorSet`: each validator then seals 4 consecutive blocks, so a builder
sees a turn change every 1.8 s.

`canary` runs the `multi` chain with go1 as validator 3, so a builder bids to both
clients in one rotation, as on BSC testnet. rust1 and rust2 start the chain (two of
three validators keep it going), and go1 syncs it in full before `miner_start`: a go1
that seals from the start makes its own block 1 before it has peers and stays on that
fork. go-bsc can sync from reth-bsc only with reth-bsc that serves pre-London bodies
without withdrawals (akfork/reth-bsc `9c4f410`, branch `fix/fix-privnet`; upstream
[bnb-chain/reth-bsc#600](https://github.com/bnb-chain/reth-bsc/pull/600)). Older
builds serve them with `withdrawals: []`, and go-bsc drops the peer with `retrieved
block body is invalid`.

### Builder whitelist

`BUILDERS=0xA,0xB` whitelists builders the way a validator operator does, in every
mode: go-bsc miners get `[[Eth.Miner.Mev.Builders]]` in their config file, and reth-bsc
miners get `BSC_ALLOWED_BUILDERS` in their environment. The list is recorded with the
network, so the handovers restart nodes with it. `miner_addBuilder` still works, but a
restart forgets what it added.

## Topology, ports and IPC

| Node | P2P | HTTP RPC | authrpc | mev gRPC | IPC |
|---|---|---|---|---|---|
| go1 | 30311 | 8545 | - | - | `$IPC_DIR/go1.ipc` |
| go2 | 30312 | 8547 | - | - | `$IPC_DIR/go2.ipc` |
| rust1 | 30313 | 8549 | 8555 | 8552 | `$IPC_DIR/rust1.ipc` |
| rust2 | 30314 | 8553 | 8556 | 8554 | `$IPC_DIR/rust2.ipc` |
| rust3 | 30315 | 8557 | 8558 | 8559 | `$IPC_DIR/rust3.ipc` |

- `PORT_OFFSET` is added to every port, so a second network can run next to the first:
  `PRIVNET_DIR=/tmp/pn2 PORT_OFFSET=1000 ./privnet.sh fast` serves rust1 on 9549.
  `start` refuses a port that is already taken. If a BSC mainnet or testnet node
  answers on it, it says so and refuses that too.
- `IPC_DIR` defaults to `$PRIVNET_DIR/ipc`. Unix socket paths are limited to 107
  bytes, so `start` fails with a clear message when a socket path would be longer.
  Set `IPC_DIR` to a shorter directory if that happens.
- Every listener binds to 127.0.0.1 (P2P included), and discovery is off. Go peers use
  `admin_addPeer`. Rust peers use `--trusted-peers` plus `admin_addTrustedPeer`, with
  enodes read from `admin_nodeInfo` (a missing enode is fatal).
- The mev gRPC listener (BEP-675 `SendBidBlock`) runs on miners only.

## Health checks

`./privnet.sh health` (run at the end of every `start`) checks, and prints every
failure:

1. Every node's process is alive (pidfile, verified against `/proc`), its RPC answers,
   and it is on the genesis chain id (never 56/97).
2. The head advances `HEALTH_BLOCKS` (5) on every node within `HEALTH_TIMEOUT` (90 s at
   3 s blocks, 45 s at 450 ms).
3. Every node has its peers: 1, or 2 for go1 in `mixed` and for every node in `multi`
   and `canary`.
4. Every node has the same hash at a common recent height.
5. `multi`/`canary`: all three validators sealed blocks in the last 48.
6. `eth_getBlockByNumber("finalized")` is above 0 and advances on every node within
   `FINALITY_TIMEOUT` (120 s / 60 s), in every mode: the go-bsc miner votes too, with
   its validator's BLS key imported into its BLS wallet (`REQUIRE_FINALITY=0` makes it a
   warning).
7. `pasteur`/`multi`/`canary`: every miner reports `BidBlockEnabled: true` in
   `mev_params`.

Exit codes for every script: `0` ok, `1` failure, `2` usage, `77` skipped (go-bsc
missing). Other knobs: `START_HEALTH=0` (skip the check in `start`),
`KEEP_ON_FAILURE=1` (leave nodes running when `start` fails), `VOTE_WARMUP_BLOCKS`
(blocks before a miner votes, default 3), `ETH_PROOF_WINDOW` (reth
`--rpc.eth-proof-window`, default 100000, for builders that read state through
`eth_getProof`), `RETH_EXTRA_ARGS` and `GETH_EXTRA_ARGS` (e.g. `--verbosity 4`).

## Builder end-to-end tests

`./privnet.sh env` prints the exports a builder e2e script needs, after it checks that
the target is not chain 56/97:

| Variable | Meaning (default ports) |
|---|---|
| `PRIVNET_CHAIN_ID` | `1337` |
| `PRIVNET_RPC_URL` | first miner's HTTP RPC (rust1: `http://127.0.0.1:8549`; go1 in `go`/`mixed`) |
| `PRIVNET_IPC_PATH` | first miner's IPC socket (`$PRIVNET_DIR/ipc/rust1.ipc`) |
| `PRIVNET_MEV_GRPC` | first miner's BEP-675 gRPC (`127.0.0.1:8552`) |
| `PRIVNET_VALIDATOR_RPC_URLS`, `PRIVNET_VALIDATOR_IPC_PATHS` | every miner, validator 1 first (`multi`: 8549, 8553, 8557; `canary`: 8549, 8553, 8545) |
| `PRIVNET_FOLLOWER_RPC_URL` | a non-mining node (rust2: 8553) |
| `PRIVNET_BLOCK_INTERVAL_MS` | `3000` or `450` |
| `PRIVNET_GENESIS` | the genesis file the nodes run (also what a builder's `chain` should point to) |
| `PRIVNET_BUILDER_ADDRESS`, `PRIVNET_BUILDER_KEY` | funded builder account |
| `PRIVNET_USER{1,2,3}_ADDRESS`, `PRIVNET_USER{1,2,3}_KEY` | funded user accounts |

Send user transactions from the test accounts, not the validator key. The validator's
nonce also advances with every system transaction it signs, so a test that signs with
it races its own node.

### DEV-ONLY accounts

| Role | Address | Private key |
|---|---|---|
| validator1 (go1, rust1) | `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A` | `0x1111…1111`, BLS `0x0707…0707` |
| validator2 (rust2 in `multi`) | `0x1563915e194D8CfBA1943570603F7606A3115508` | `0x2222…2222`, BLS `0x0808…0808` |
| validator3 (rust3 in `multi`, go1 in `canary`) | `0x5CbDd86a2FA8Dc4bDdd8a8f69dBa48572EeC07FB` | `0x3333…3333`, BLS `0x0909…0909` |
| builder | `0x7564105E977516C53bE337314c7E53838967bDaC` | `0x4444…4444` |
| user1 | `0xe1fAE9b4fAB2F5726677ECfA912d96b0B683e6a9` | `0x5555…5555` |
| user2 | `0xdb2430B4e9AC14be6554d3942822BE74811A1AF9` | `0x6666…6666` |
| user3 | `0xAe72A48c1a36bd18Af168541c53037965d26e4A8` | `0x7777…7777` |

Every key is the byte shown repeated 32 times. Validators hold 1e9 BNB each, and the
builder and users hold 1e7 BNB each, in every genesis profile.

## Process management

- Each node runs with its datadir as its working directory. Its vote journal is
  `<datadir>/voteJournal` (`BSC_VOTE_JOURNAL_DIR`), and its logs are `node.out`,
  `node.log` and `logs/`. Nothing is written relative to the caller's cwd. Older
  versions let reth-bsc create `voteJournal/` in whatever directory the script was
  run from. A stale journal refuses votes on every height it lists, and `clean` now
  removes the legacy `privnet/voteJournal`.
- The script records `pid` and the canonical binary path (`pid.exe`) for every node.
  Before it sends any signal, it checks that `/proc/<pid>` runs that binary with
  `--datadir <that node's dir>`. It never pattern-matches process lists. `stop` sends
  SIGTERM, waits up to `STOP_TIMEOUT` (60 s) for the process to exit, and sends SIGKILL
  only as a last resort. `clean` refuses to wipe a datadir that any process still uses.
- Handovers stop rust1, wait for it to exit (so its database lock is released), restart
  it on the same datadir, and check that the chain advances.

## Operational notes (all observed in testing)

- **Go follower needs `--syncmode full`.** The default snap mode fails on tiny private
  chains with `missing trie node ... layer stale`.
- **Rust nodes need `-d` (no discovery).** A custom chain has no bundled bootnodes, so
  upstream reth falls back to Ethereum-mainnet bootnodes; the node dials the public
  internet and gets banned. Fixed binaries also default custom chains to an empty
  bootnode set.
- **Rust mining must be enabled at startup** (`--mining.enabled --mining.private-key
  ...`). The `miner_start` RPC is accepted but does not start the sealing loop; Go's
  `miner_stop`/`miner_start` do work at runtime, which is what `handover-to-go` relies
  on.
- **`--bootnodes` is ignored under Go's `--nodiscover`.** Go peering uses
  `admin_addPeer`.
- **Breathe blocks stop the chain, so the script disables them.** See
  [The daily breathe block](#the-daily-breathe-block) below. `BREATHE_BLOCK_INTERVAL`
  (default `1e15`) turns the rule off on every node.
- **Epoch boundaries and finality work.** The genesis deploys the system contracts,
  so the chain runs past every epoch (tested to block 1248 at 450 ms). Older genesis
  files had two problems. Those without contracts stalled at block 199. Those that
  registered upstream's BLS key in `BSCValidatorSet` lost fast finality at block 200:
  `cannot collect enough votes`, `finalized` stuck at 0. See [GENESIS.md](./GENESIS.md).
- **Do not run two miners on the same validator key.** Both clients seal on schedule,
  and the tip flaps between forks. It converges, but the logs fill with reorg noise.
  Use the handover actions for a single producer, or `multi` for distinct validators.

## The daily breathe block

A **breathe block** is one whose parent lies in an earlier UTC day. It is the only
place `updateValidatorSetV2` runs (`consensus/parlia/feynmanfork.go`), and it is
also the one system transaction a private network cannot serve, for two reasons
that compound:

1. `bsc-genesis-contract`'s `genesis-dev.json` ships a **pre-Feynman**
   `BSCValidatorSet` at `0x1000` — its runtime code has no `updateValidatorSetV2`
   selector (`0x6550d581`), only `deposit` and friends.
2. go-bsc would normally replace it, but its built-in upgrade table is keyed by
   genesis hash and only has `mainnet`/`chapel` entries
   (`feynmanUpgrade[defaultNet]` is `nil`, and `applySystemContractUpgrade`
   no-ops on a nil table). A custom chain is therefore never upgraded.

So the call jumps to the contract's dispatcher default and halts with
`invalid opcode: INVALID` **inside block assembly**. That is not a failed
transaction the node can skip — the block cannot be built at all, so the miner
retries forever and the chain is dead until the datadir is wiped. Note this is
not about pausing: crossing a UTC day is enough, running or not, so an ordinary
7×24 private network dies within 24h of block 1.

The fix is to turn the rule off. `isBreatheBlock` is consensus, so *every* node
must derive the same answer, and both clients expose an override for exactly
this:

| client | override |
|---|---|
| go-bsc | `--override.breatheblockinterval <seconds>` |
| reth-bsc | `BREATHE_BLOCK_INTERVAL=<seconds>` (env var) |

`privnet.sh` passes the same `BREATHE_BLOCK_INTERVAL` to all four nodes and
defaults it to `1e15`, which is larger than any real timestamp, so no pair of
blocks can ever land in different buckets. Concretely:

```bash
# default: breathe blocks off, runs indefinitely
./privnet.sh mixed

# production schedule (86400) if you specifically want the daily refresh;
# the chain will need a datadir reset at every UTC midnight
BREATHE_BLOCK_INTERVAL=86400 ./privnet.sh mixed
```

Verified end to end: with `--override.breatheblockinterval=120` the chain
stalls within two minutes of block 1; at `1e15` it runs past the same boundary
with four nodes agreeing byte-for-byte on hash and `stateRoot`, and both
`just e2e-evmdb-bsc` and `just e2e-fork-db-bsc` pass. Never change the interval
on an already-running chain — the block layout forks.

Re-run that verification with `just privnet-verify-breathe` (~8 min): it
starts a mixed network twice, once at a 120s interval where the stall must
appear, once at the shipped default where it must not, and then checks that all
four nodes still agree on hash and `stateRoot`. `--case-b` runs only the second
half. A node that stops answering counts as a failure, never as a stall. The
script runs in its own temporary directory on `PORT_OFFSET` 2000 (override with
`AB_PRIVNET_DIR` / `AB_PORT_OFFSET`). A network already running from `./data` is
therefore never touched, and a trap tears the test network down however the
script exits. It needs go-bsc and exits 77 (skipped) without it.
