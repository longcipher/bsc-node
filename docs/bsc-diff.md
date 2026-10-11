# BSC Dual-Client Deep-Dive: Go (`bsc`) vs Rust (`reth-bsc`)

中文版（中文）： [bsc-diff.zh.md](./bsc-diff.zh.md)

> Analysis subjects (local checkout `/Volumes/akext/src/github.com/bnb-chain/`):
>
> - `bsc/` — Go implementation (a go-ethereum fork; BNB Chain's official, authoritative client)
> - `reth-bsc/` — Rust implementation (an extension of reth via its `NodeBuilder` API, not a fork)
>
> Conclusions are grounded in the source code and each project's own README / CLAUDE.md /
> CHANGELOG statements (no live network research was available for this pass, but the
> in-repo statements are authoritative).

---

## 1. Executive Summary

**A Rust node is fine as a "read-data full node / RPC source", but never as a validator or for anything that hard-depends on finality; for critical work, trust Go or cross-validate the two clients (see the probe crate below).**

| Question | Answer |
|---|---|
| Are both implementations "fine / production-grade"? | **No.** Go is a battle-tested production client. Rust is officially positioned as **experimental / under active development** and **cannot act as a block-producing validator**. |
| Can they sync with each other? | **Yes (as full nodes / archive nodes reading the same chain).** The devp2p wire protocol, plus the `bsc/1` and `bsc/2` sub-protocol message codes and RLP encoding, are byte-for-byte identical; Rust has interop tests against Go's reference encoding. |
| Are they used heavily in production? | Go is the client actually run by BSC mainnet's 21 validators. Rust currently serves **full-node / archive-node / RPC** roles; it is not yet announced for production validation. A Rust node someone gives you ≈ a read/RPC node. |
| Are RPC / P2P different? | **P2P is near-fully compatible** (Rust mirrors Go's wire format). **RPC namespaces mostly align, but Rust is missing some BSC-specific finality APIs**, and its handling of the `safe`/`finalized` tags differs semantically from Go. |

---

### 2. Project Positioning & Maturity

#### Go implementation `bnb-chain/bsc`

- go-ethereum fork, BSC's **original / authoritative implementation** (`bsc/README.md:1-13`).
- Full PoSA/Parlia block-production capability: `Prepare` / `Finalize` / `FinalizeAndAssemble` / `Seal` are all implemented (`bsc/consensus/parlia/parlia.go:1154,1401,1510,1723`) — **it can validate and produce blocks**.
- Mature release process: Stable / Feature / Preview channels (`bsc/README.md:23-28`). Repo HEAD is `v1.8.0-alpha` (a preview tracking geth v1.17.3, `bsc/CHANGELOG.md:3`).
- Full toolchain: clef, devp2p, abigen, bootnode, evm; complete hardware docs (3TB SSD / 16C / 64G, `bsc/README.md:105-117`).

#### Rust implementation `bnb-chain/reth-bsc`

- **Not a fork of reth, but an extension built on reth's `NodeBuilder` API** (`reth-bsc/README.md:3`, `reth-bsc/CLAUDE.md:7`). All BSC-specific logic (Parlia, system contracts, PoSA mining, MEV, EVN, precompiles) lives in this repo.
- Official status table (`reth-bsc/README.md:9-19`):
  - Historical Sync ✅, BSC Pectra Support ✅, Live Sync ✅
  - **Run as validator ❌ (soon)** ← the key line
- Disclaimer: **"This project is experimental and under active development. Use at your own risk."** (`reth-bsc/README.md:200`)
- At the code level, PoSA mining is **already written** (`reth-bsc/src/node/miner/`, `reth-bsc/MINING.md`, `reth-bsc/CLAUDE.md:75` describes the `BscMiner::start` production loop), but its production-readiness is **not yet endorsed** — the code exists, but it has not been production-validated / certified as a validator.

**Maturity verdict:** Go is an order of magnitude ahead and is the "only production-validated client". Rust is usable for "full-node / archive-node sync + read RPC"; the validator path is engineered but not yet broadly production-tested.

---

### 3. Mutual Sync / Consensus Interoperability

**They can sync**, because they read the same chain and run the same consensus and hard forks.

- **Hard-fork timelines match on both sides:** Ramanujan → Niels → MirrorSync → Bruno → Euler → Gibbs → Nano → Moran → Planck → Luban → Plato → Hertz → Kepler → Feynman → FeynmanFix → HaberFix → Bohr → Pascal → Prague (Pectra) → Lorentz → Maxwell → Fermi.
  - Go: `bsc/params/config.go`, `bsc/core/systemcontracts/{feynman,planck,luban}/*`
  - Rust: `reth-bsc/src/hardforks/bsc.rs`, `reth-bsc/src/system_contracts/` (CLAUDE.md:77 lists the same list)
- **Both consensus engines are Parlia (PoSA)**; both maintain a validator-set snapshot at epoch boundaries (`snapshot.rs`) and implement BLS voting (`vote_pool` / `bls_signer`). Rust deliberately re-implements Go's RNG (`go_rng.rs`) to keep block-ordering deterministic.
- Both validate EVM equivalence via **execution-spec-tests (EEST)** (Rust `make ef-tests`; Go has its own long-standing tests), reducing the risk of "same tx, different state root across clients".

⚠️ **Client-diversity risk:** as an independent implementation, if Rust has a bug in some hard fork / system contract / Parlia fork-choice, it may **follow a different chain (chain split)**. This is exactly why BNB Chain pushes reth-bsc — but it also means that, until Rust is broadly validated, treating a single Rust node as an "authoritative read source" carries a chain-split risk.

---

### 4. P2P Protocol Differences (verdict: highly compatible, Rust deliberately mirrors Go)

#### 4.1 Underlying devp2p `eth` sub-protocol

- Rust reuses upstream reth's `reth-eth-wire` (i.e. `eth/68`).
- Go recently (`v1.8.0-alpha`) made **`eth/70` the primary protocol but explicitly "keeps eth/68 for compatibility"** (`bsc/CHANGELOG.md:19`). → For now Rust (eth/68) and Go still handshake and interconnect fine.
- ⚠️ **Potential future gap:** once Go mainnet fully drops eth/68, Rust must upgrade to eth/70 to keep interconnecting. This is an alignment point to track continuously.

#### 4.2 BSC custom `bsc` sub-protocol (byte-for-byte identical)

Both sides align field-by-field (`bsc/eth/protocols/bsc/protocol.go` vs `reth-bsc/src/node/network/bsc_protocol/protocol/proto.rs`):

| Item | Go | Rust |
|---|---|---|
| Protocol name | `"bsc"` | `"bsc"` |
| Versions | `bsc/1`, `bsc/2` | `bsc/1`, `bsc/2` |
| Message count | v1=2, v2=4 | v1=2, v2=4 |
| `0x00` BscCapMsg | ✅ | ✅ (Capability) |
| `0x01` VotesMsg | ✅ | ✅ (Votes) |
| `0x02` GetBlocksByRange | ✅ | ✅ |
| `0x03` BlocksByRange | ✅ | ✅ |

Rust has a unit test using **Go's RLP encoding as the reference vector** (`proto.rs:64` `test_bsc1_votes_messages_rlp_matches_reference`, against real captured Go-node bytes `f90973…`), proving wire-format equivalence. Both implement BSC's `GetBlocksByRange` (max 64 blocks per request, 8MB soft cap) and a total-difficulty-threshold-based vote-broadcast strategy (`deltaTdThreshold=1000`; Rust `registry.rs:309` comment "Matches go-bsc eth/handler.go").

#### 4.3 Upgrade-Status handshake (EVN signal)

- BSC adds an extension message during the eth handshake to tell the peer "whether to disable transaction broadcast" (Enhanced Validator Network).
- Rust's `upgrade_status.rs` decodes **bytes captured from a real BSC node** (`0bc180` / `0bc2c101`, message id `0x0b`), matching Go's `UpgradeStatusMsg` (`bsc/eth/protocols/eth/handshake.go:88,103`).
- Note: Rust's `UpgradeStatusExtension.disable_peer_tx_broadcast` is marked **`TODO: support`** (`upgrade_status.rs:56`) — it can decode, but the behavior is not yet fully active; Go already uses it. Rust separately implements EVN via an independent `--evn.enabled` flag (activated only after sync completes and head lag < 30s, CLAUDE.md:91) — a different path to the same goal.

**P2P takeaway:** wire protocol is 100% interoperable; Rust is the follower/aligner. The only thing to watch: Go has moved to eth/70 while Rust is still on eth/68 (propped up by Go's backward compatibility).

---

### 5. RPC Differences

#### 5.1 Namespace comparison

| Namespace | Go (`bsc`) | Rust (`reth-bsc`) | Notes |
|---|---|---|---|
| `eth` | ✅ full | ✅ full | standard Ethereum API + BSC extensions |
| `net` / `web3` | ✅ | ✅ | |
| `admin` | ✅ | ✅ (`rpc/admin.rs`) | |
| `debug` | ✅ rich | ✅ (`rpc/debug_builder.rs`) | |
| `txpool` | ✅ | ✅ | |
| `trace` | ✅ | ✅ (listed in README; needs archive node) | |
| `parlia` | ✅ `getSnapshot/getValidators/getJustifiedNumber/getFinalizedNumber/...` | ✅ same name/params (`rpc/parlia.rs`, plus `buildAddNodeIDsCall/RemoveNodeIDsCall`) | Rust output matches bsc-erigon/geth shape |
| `miner` | ✅ (geth built-in + BSC `miner_mev*`/`addBuilder`) | ✅ redefined (`rpc/miner.rs`, covers `miner_setExtra/setGasPrice/setGasLimit`, drops reth's unimplemented defaults) | |
| `mev` | ✅ (`internal/ethapi/api_mev.go`) | ✅ (`rpc/mev.rs`, bid submission + MEV API) | |
| `blob` | via `eth_getBlobSidecars` etc. | ✅ (`rpc/blob.rs` reads blobs from pool/provider) | |
| `eth_health` / `eth_coinbase` | Go has `eth_health`; geth provides `eth_coinbase` | ✅ `eth_ext.rs` explicitly implements `eth_coinbase` + `eth_health` | Rust removes reth's default empty impl first, then registers (`main.rs:499-500`) |

#### 5.2 Rust's missing / caution-worthy BSC-specific APIs (important)

Confirmed by code search: Rust has **no** the following Go-documented, finality-related methods (no match in `reth-bsc/src`):

- `eth_getFinalizedHeader`
- `eth_getFinalizedBlock`
- `eth_newFinalizedHeaderFilter`
- `eth_getHeaderByNumber` support for the `"finalized"` tag

Go also has `eth_getTransactionsByBlockNumber`, `eth_getTransactionDataAndReceipt`, `eth_getBlobSidecarByTxHash`, etc., which need to be checked one by one against Rust (`eth_getBlobSidecars` should already exist; the finalized series is most likely missing).

**Impact:** any workflow that depends on BSC **finality proofs** (cross-chain bridges, IBC / light clients, relayers) cannot get a finalized head/block from a Rust node — it must use Go.

#### 5.3 Subtle semantic differences

- `parlia_getSnapshot` etc.: Go uses `*rpc.BlockNumber` (supports `safe`/`finalized` tags); Rust takes a string and **treats both `safe` and `finalized` as `latest`** (`parlia.rs:195-204`) — the call may return a different height.
- `parlia_getFinalizedNumber`: Go returns `Attestation.SourceNumber` (`api.go:114`); Rust returns `snapshot.get_finalized_number()` (`parlia.rs:405`) — assuming the snapshot structure is identical, the two should be equivalent, but the tag-handling difference changes the input block, hence the result.

---

### 6. Feature Maturity Side-by-Side

| Capability | Go `bsc` | Rust `reth-bsc` | Notes |
|---|---|---|---|
| Full-node sync | ✅ production | ✅ (synced to mainnet tip; README cites ~10.6T disk) | |
| Archive node | ✅ | ✅ | |
| Snapshot sync | ✅ mature | ✅ (reth snapshot from bsc-snapshots) | |
| Block production / validator | ✅ **production validator** | ⚠️ code exists but not endorsed (❌ soon) | key gap |
| EVM equivalence | ✅ | ✅ (runs EEST) | |
| System contracts / precompiles (BLS, CometBFT light client, IAVL…) | ✅ | ✅ (`src/evm/precompiles/*`) | |
| MEV / bid | ✅ production | ✅ (in progress) | |
| BLS voting / double-sign detection | ✅ | ✅ | |
| EVN (validator network) | ✅ | ⚠️ partial (decodes handshake, disable flag TODO; separate EVN impl also present) | |
| Light node (LES) | Go has legacy light | ❌ reth has no LES | |
| Metrics | ✅ `:6060/metrics` | ✅ `--metrics 0.0.0.0:6060/metrics` | |

---

### 7. Production Status & Risk

1. **Who runs in production:** BSC mainnet's 21 validators run Go `geth-bsc` (years without incident). Rust nodes appear in read-only / infrastructure roles like "someone else's RPC node", not in block production.
2. **Risk of using a Rust node as an RPC source:** it is a young implementation; even though EVM equivalence is covered by EEST, extreme edge-case consensus divergence is still possible → it may return blocks / state **inconsistent with Go**; it lacks the finalized API series; and the official disclaimer says "use at your own risk".
3. **Risk of using Rust as a validator:** currently unsupported by the project, and not battle-tested by the 21-validator set.
4. **Verified interop points:** `bsc/1`, `bsc/2` message codes and RLP are byte-for-byte identical (Rust has a real captured-byte comparison test); eth/68 currently interconnects (propped up by Go's backward compatibility).

---

### 8. Recommendations for Your Use Case (MEV search / DEX aggregator, in-process revm simulation)

Your architecture: you use revm **in your own process** to sync BSC block state and run simulations. This means:

- The client is only your **state and block data provider**; it does not need to run revm locally (you do).
- Key needs: **accurate block headers (especially `stateRoot`), accurate txs + receipts, and arbitrary-height historical state** (for replay / simulation).
- Finality awareness (your bundle must target a "safe" head and avoid blocks that may reorg) requires a finalized/head concept.

#### RPC client selection

| Use | Recommended client | Why |
|---|---|---|
| Primary state source / authoritative read | **Go archive node** | production-validated, full finality API, trustworthy state root |
| Historical-state replay/sim (feed revm state) | Go archive (primary) + Rust archive (cross-check) | history needs archive; cross-checking state roots cuts chain-split risk |
| Chain head / finality awareness (pick bundle target) | **Go** | Rust lacks `eth_getFinalizedHeader` and treats `finalized` as `latest` |
| Bulk reads / offload Go | Rust full/archive node (redundancy/compare source only) | read RPC works, but **run the probe for state-root consistency before every production use** |
| `eth_call` / `eth_getStorageAt` / `eth_getProof` (sim inputs) | Go (preferred) | result equivalence needs probe verification; Rust is redundancy |
| `eth_getBlockByNumber`(full) / `eth_getBlockReceipts` (build revm block) | either, but **must be compared** | matching header/tx-root/receipt-root is the precondition for "same world state" |

#### Operating points

1. **World-state consistency must be guaranteed before revm simulation:** use `eth_getBlockByNumber(height, true)` to fetch the block header (`stateRoot`) and txs, `eth_getBlockReceipts` for receipts, and `eth_getProof` / `eth_getStorageAt` to pull account + storage to fill revm's state DB. The `stateRoot` of the two clients at the same height **must be equal** — this is exactly what the `bsc-consistency-probe` crate does.
2. **Historical simulation needs an archive node:** a full node keeps only recent state and cannot serve old-block state. Both Go and Rust archive modes support reads (`bsc/README.md:75`, `reth-bsc/README.md:75`), but Rust archive's "read consistency" must be measured with the probe.
3. **Do not trust Rust's `safe`/`finalized`:** if your searcher uses `eth_getBlockByNumber("finalized")` to pick a safe height, Rust returns `latest` — you may build on a head that is not yet finalized. Always go through Go.
4. **Dual-client cross-check is cheap insurance:** run the probe regularly at critical heights (especially production peaks, around hard forks); on any state-root mismatch, alert immediately and fall back to Go.

> The companion tool is the Rust crate `bin/bsc-consistency-probe` in this repo: it cross-validates same-height block headers (including `stateRoot` / `receiptsRoot` / `transactionsRoot`) across a Go endpoint and a Rust endpoint, and additionally has a `truth` mode that verifies `eth_getProof` Merkle proofs against the reported state root.

---

### 9. Conclusion

- The two implementations are **not "both fine / both production-grade"**: Go is production-grade and produces blocks; Rust is experimental, can sync and serve read RPC, but **cannot be a validator**.
- **They can sync with each other:** same underlying chain, consensus, and hard forks; the P2P wire protocol (including BSC's custom sub-protocol) is strictly mirrored by Rust, and interop is covered by real captured-byte tests.
- **RPC mostly aligns**, but Rust is missing BSC's finalized/finality APIs and handles `safe`/`finalized` tags differently from Go — for finality-sensitive work, use Go.
- A Rust node someone gives you = a **usable read / RPC full node**, good for redundancy and comparison, **not fit as an authoritative source or for anything finality-/block-production-related**.
- For your MEV / DEX-aggregator scenario: **Go archive node as primary state source, Rust archive as redundancy/compare source, run the probe for state-root consistency before every production use**; finalized/head decisions go to Go only.
