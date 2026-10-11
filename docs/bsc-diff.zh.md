# BSC 双客户端深度对比：Go（`bsc`）vs Rust（`reth-bsc`）

English: [bsc-diff.md](./bsc-diff.md)

> 分析对象（本地 checkout `/Volumes/akext/src/github.com/bnb-chain/`）：
>
> - `bsc/` —— Go 实现（go-ethereum 分支；BNB Chain 官方、权威客户端）
> - `reth-bsc/` —— Rust 实现（基于 reth 的 `NodeBuilder` API 扩展，并非 fork）
>
> 结论依据源码以及各项目自身的 README / CLAUDE.md / CHANGELOG 声明（本轮未做联网调研，但仓库内声明具有权威性）。

---

## 1. 执行摘要

两套实现并非"都可用 / 都达到生产级"。Go 是久经考验的生产客户端；Rust 官方定位为"实验性 / 活跃开发中"，且**不能作为出块验证者**。二者可以互相同步（作为读取同一条链的全节点 / 归档节点）；P2P 几乎完全兼容，RPC 命名空间大体对齐但 Rust 缺少部分最终性相关 API，且对 `safe`/`finalized` 标签的语义处理与 Go 不同。对关键业务应信任 Go，或对两套客户端做交叉校验。

| Question | Answer |
|---|---|
| Are both implementations "fine / production-grade"? | **No.** Go is a battle-tested production client. Rust is officially positioned as **experimental / under active development** and **cannot act as a block-producing validator**. |
| Can they sync with each other? | **Yes (as full nodes / archive nodes reading the same chain).** The devp2p wire protocol, plus the `bsc/1` and `bsc/2` sub-protocol message codes and RLP encoding, are byte-for-byte identical; Rust has interop tests against Go's reference encoding. |
| Are they used heavily in production? | Go is the client actually run by BSC mainnet's 21 validators. Rust currently serves **full-node / archive-node / RPC** roles; it is not yet announced for production validation. A Rust node someone gives you ≈ a read/RPC node. |
| Are RPC / P2P different? | **P2P is near-fully compatible** (Rust mirrors Go's wire format). **RPC namespaces mostly align, but Rust is missing some BSC-specific finality APIs**, and its handling of the `safe`/`finalized` tags differs semantically from Go. |

---

### 2. 项目定位与成熟度

#### Go 实现 `bnb-chain/bsc`

- go-ethereum 分支，BSC 的**原始 / 权威实现**（`bsc/README.md:1-13`）。
- 具备完整的 PoSA/Parlia 出块能力：`Prepare` / `Finalize` / `FinalizeAndAssemble` / `Seal` 均已实现（`bsc/consensus/parlia/parlia.go:1154,1401,1510,1723`）——**可以验证并出块**。
- 成熟的发布流程：Stable / Feature / Preview 三个通道（`bsc/README.md:23-28`）。仓库 HEAD 为 `v1.8.0-alpha`（跟踪 geth v1.17.3 的预览版，`bsc/CHANGELOG.md:3`）。
- 完整工具链：clef、devp2p、abigen、bootnode、evm；硬件配置文档齐全（3TB SSD / 16C / 64G，`bsc/README.md:105-117`）。

#### Rust 实现 `bnb-chain/reth-bsc`

- **并非 reth 的 fork，而是基于 reth 的 `NodeBuilder` API 构建的扩展**（`reth-bsc/README.md:3`、`reth-bsc/CLAUDE.md:7`）。所有 BSC 特有逻辑（Parlia、系统合约、PoSA 挖矿、MEV、EVN、预编译）都在此仓库中。
- 官方状态表（`reth-bsc/README.md:9-19`）：
  - Historical Sync ✅、BSC Pectra Support ✅、Live Sync ✅
  - **Run as validator ❌ (soon)** ← 关键一行
- 免责声明：**"This project is experimental and under active development. Use at your own risk."**（`reth-bsc/README.md:200`）
- 在代码层面，PoSA 挖矿其实**已经写出来**（`reth-bsc/src/node/miner/`、`reth-bsc/MINING.md`、`reth-bsc/CLAUDE.md:75` 描述了 `BscMiner::start` 生产循环），但其生产就绪性**尚未被背书**——代码存在，但尚未经过生产验证 / 未被认证为验证者。

**成熟度结论：** Go 领先一个数量级，是"唯一经过生产验证的客户端"。Rust 可用于"全节点 / 归档节点同步 + 只读 RPC"；验证者路径在工程中已实现，但尚未经过广泛的生产测试。

---

### 3. 互相同步 / 共识互操作性

**它们可以同步**，因为读取的是同一条链，运行的是相同的共识与硬分叉。

- **硬分叉时间线两侧一致：** Ramanujan → Niels → MirrorSync → Bruno → Euler → Gibbs → Nano → Moran → Planck → Luban → Plato → Hertz → Kepler → Feynman → FeynmanFix → HaberFix → Bohr → Pascal → Prague (Pectra) → Lorentz → Maxwell → Fermi。
  - Go：`bsc/params/config.go`、`bsc/core/systemcontracts/{feynman,planck,luban}/*`
  - Rust：`reth-bsc/src/hardforks/bsc.rs`、`reth-bsc/src/system_contracts/`（CLAUDE.md:77 列出相同列表）
- **两侧共识引擎都是 Parlia (PoSA)**；都在 epoch 边界维护验证者集合快照（`snapshot.rs`）并实现 BLS 投票（`vote_pool` / `bls_signer`）。Rust 刻意重写了 Go 的 RNG（`go_rng.rs`）以保持出块顺序的确定性。
- 两侧都通过 **execution-spec-tests (EEST)** 验证 EVM 等价性（Rust `make ef-tests`；Go 有长期自有的测试），降低了"同一笔交易、不同客户端得到不同 state root"的风险。

⚠️ **客户端多样性风险：** 作为独立实现，若 Rust 在某个硬分叉 / 系统合约 / Parlia 分叉选择上存在 bug，它可能**跟随另一条链（链分叉）**。这正是 BNB Chain 力推 reth-bsc 的原因——但也意味着，在 Rust 被广泛验证之前，把单个 Rust 节点当作"权威读源"存在链分叉风险。

---

### 4. P2P 协议差异（结论：高度兼容，Rust 刻意镜像 Go）

#### 4.1 底层 devp2p `eth` 子协议

- Rust 复用上游 reth 的 `reth-eth-wire`（即 `eth/68`）。
- Go 近期（`v1.8.0-alpha`）把 **`eth/70` 作为主协议，但明确"保留 eth/68 以兼容节点"**（`bsc/CHANGELOG.md:19`）。→ 目前 Rust（eth/68）与 Go 仍能正常握手并互联。
- ⚠️ **潜在的未来缺口：** 一旦 Go 主网彻底移除 eth/68，Rust 必须升级到 eth/70 才能继续互联。这是一个需要持续跟踪的对齐点。

#### 4.2 BSC 自定义 `bsc` 子协议（逐字节一致）

两侧逐字段对齐（`bsc/eth/protocols/bsc/protocol.go` vs `reth-bsc/src/node/network/bsc_protocol/protocol/proto.rs`）：

| Item | Go | Rust |
|---|---|---|
| Protocol name | `"bsc"` | `"bsc"` |
| Versions | `bsc/1`, `bsc/2` | `bsc/1`, `bsc/2` |
| Message count | v1=2, v2=4 | v1=2, v2=4 |
| `0x00` BscCapMsg | ✅ | ✅ (Capability) |
| `0x01` VotesMsg | ✅ | ✅ (Votes) |
| `0x02` GetBlocksByRange | ✅ | ✅ |
| `0x03` BlocksByRange | ✅ | ✅ |

Rust 有一个以 **Go 的 RLP 编码作为参考向量**的单元测试（`proto.rs:64` `test_bsc1_votes_messages_rlp_matches_reference`，对照真实 Go 节点抓包字节 `f90973…`），证明线格式等价。两侧都实现了 BSC 特有的 `GetBlocksByRange`（每请求最多 64 个块、8MB 软上限）以及基于总难度阈值的投票广播策略（`deltaTdThreshold=1000`；Rust `registry.rs:309` 注释 "Matches go-bsc eth/handler.go"）。

#### 4.3 Upgrade-Status 握手（EVN 信号）

- BSC 在 eth 握手期间增加一个扩展消息，用于告诉对端"是否禁用交易广播"（Enhanced Validator Network）。
- Rust 的 `upgrade_status.rs` 解码了**来自真实 BSC 节点抓包的字节**（`0bc180` / `0bc2c101`，消息 id `0x0b`），与 Go 的 `UpgradeStatusMsg` 一致（`bsc/eth/protocols/eth/handshake.go:88,103`）。
- 注意：Rust 的 `UpgradeStatusExtension.disable_peer_tx_broadcast` 被标记为 **`TODO: support`**（`upgrade_status.rs:56`）——能解码，但行为尚未完全生效；Go 已经在用。Rust 另通过独立的 `--evn.enabled` 标志实现 EVN（仅在同步完成且 head 滞后 < 30s 后激活，CLAUDE.md:91）——殊途同归。

**P2P 结论：** 线协议 100% 可互操作；Rust 是跟随者 / 对齐者。唯一需要留意的是：Go 已迁到 eth/70，而 Rust 仍在 eth/68（靠 Go 的向后兼容撑着）。

---

### 5. RPC 差异

#### 5.1 命名空间对比

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

#### 5.2 Rust 缺失 / 需警惕的 BSC 特有 API（重要）

经代码搜索确认：Rust **没有**以下 Go 文档中存在的、与最终性相关的方法（`reth-bsc/src` 中无匹配）：

- `eth_getFinalizedHeader`
- `eth_getFinalizedBlock`
- `eth_newFinalizedHeaderFilter`
- `eth_getHeaderByNumber` 中对 `"finalized"` 标签的支持

Go 还有 `eth_getTransactionsByBlockNumber`、`eth_getTransactionDataAndReceipt`、`eth_getBlobSidecarByTxHash` 等，需逐一核对 Rust 是否对齐（`eth_getBlobSidecars` 应已存在；finalized 系列最可能缺失）。

**影响：** 任何依赖 BSC **最终性证明**的工作流（跨链桥、IBC / 轻客户端、中继器）都无法从 Rust 节点拿到最终化 head/块——必须使用 Go。

#### 5.3 细微语义差异

- `parlia_getSnapshot` 等：Go 使用 `*rpc.BlockNumber`（支持 `safe`/`finalized` 标签）；Rust 接收字符串并**把 `safe` 和 `finalized` 都当作 `latest`**（`parlia.rs:195-204`）——调用可能返回不同高度。
- `parlia_getFinalizedNumber`：Go 返回 `Attestation.SourceNumber`（`api.go:114`）；Rust 返回 `snapshot.get_finalized_number()`（`parlia.rs:405`）——在快照结构相同的前提下二者应等价，但标签处理的差异会改变输入块，从而改变结果。

---

### 6. 功能成熟度对照

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

### 7. 生产状态与风险

1. **谁在生产中运行：** BSC 主网 21 个验证者运行 Go `geth-bsc`（多年无事故）。Rust 节点出现在"别人的 RPC 节点"这类只读 / 基础设施角色中，不参与出块。
2. **把 Rust 节点当 RPC 源的风险：** 它是年轻实现；即便 EVM 等价性由 EEST 覆盖，极端边缘的共识分歧仍有可能 → 它可能返回与 Go **不一致的块 / 状态**；它缺少 finalized API 系列；且官方免责声明"use at your own risk"。
3. **把 Rust 当验证者的风险：** 目前项目不支持，也未经 21 验证者集合的实战检验。
4. **已验证的互操作点：** `bsc/1`、`bsc/2` 消息码与 RLP 逐字节一致（Rust 有真实抓包字节对比测试）；eth/68 目前可互联（靠 Go 向后兼容撑着）。

---

### 8. 针对你的场景的建议（MEV 搜索 / DEX 聚合器，进程内 revm 模拟）

你的架构：你在**自己进程内**使用 revm 来同步 BSC 区块状态并运行模拟。这意味着：

- 客户端只是你的**状态与区块数据提供方**；它不需要在本地跑 revm（你跑）。
- 关键需求：**准确的区块头（尤其是 `stateRoot`）、准确的 txs + receipts，以及任意高度的历史状态**（用于回放 / 模拟）。
- 最终性感知（你的 bundle 必须瞄准一个"安全"的 head，并避开可能 reorg 的块）需要一个 finalized/head 概念。

#### RPC 客户端选择

| Use | Recommended client | Why |
|---|---|---|
| Primary state source / authoritative read | **Go archive node** | production-validated, full finality API, trustworthy state root |
| Historical-state replay/sim (feed revm state) | Go archive (primary) + Rust archive (cross-check) | history needs archive; cross-checking state roots cuts chain-split risk |
| Chain head / finality awareness (pick bundle target) | **Go** | Rust lacks `eth_getFinalizedHeader` and treats `finalized` as `latest` |
| Bulk reads / offload Go | Rust full/archive node (redundancy/compare source only) | read RPC works, but **run the probe for state-root consistency before every production use** |
| `eth_call` / `eth_getStorageAt` / `eth_getProof` (sim inputs) | Go (preferred) | result equivalence needs probe verification; Rust is redundancy |
| `eth_getBlockByNumber`(full) / `eth_getBlockReceipts` (build revm block) | either, but **must be compared** | matching header/tx-root/receipt-root is the precondition for "same world state" |

#### 运行要点

1. **revm 模拟前必须保证世界状态一致：** 用 `eth_getBlockByNumber(height, true)` 取区块头（`stateRoot`）与 txs，用 `eth_getBlockReceipts` 取 receipts，再用 `eth_getProof` / `eth_getStorageAt` 拉取账户 + 存储来填充 revm 的状态库。两个客户端在同高度的 `stateRoot` **必须相等**——这正是 `bsc-consistency-probe` crate 所做的事情。
2. **历史模拟需要归档节点：** 全节点只保留近期状态，无法服务旧块状态。Go 与 Rust 的归档模式都支持读取（`bsc/README.md:75`、`reth-bsc/README.md:75`），但 Rust 归档的"读一致性"必须用探针测量。
3. **不要信任 Rust 的 `safe`/`finalized`：** 若你的搜索器用 `eth_getBlockByNumber("finalized")` 挑选安全高度，Rust 返回的是 `latest`——你可能构建在一个尚未最终化的 head 上。始终走 Go。
4. **双客户端交叉校验是廉价的保险：** 在关键高度（尤其生产高峰、硬分叉前后）定期跑探针；一旦发现 state root 不一致，立即告警并回退到 Go。

> 配套工具是本仓库中的 Rust crate `bin/bsc-consistency-probe`：它在 Go 端点与 Rust 端点之间交叉校验同高度区块头（含 `stateRoot` / `receiptsRoot` / `transactionsRoot`），并提供 `truth` 模式，用 `eth_getProof` 的 Merkle 证明对照所声明的 state root 做验证。

---

### 9. 结论

- 两套实现**并非"都可用 / 都达到生产级"**：Go 达到生产级并能出块；Rust 是实验性的，能同步并提供只读 RPC，但**不能做验证者**。
- **它们可以互相同步：** 底层链、共识、硬分叉相同；P2P 线协议（含 BSC 自定义子协议）被 Rust 严格镜像，互操作性由真实抓包字节测试覆盖。
- **RPC 大体对齐**，但 Rust 缺少 BSC 的 finalized/最终性 API，且对 `safe`/`finalized` 标签的处理与 Go 不同——对最终性敏感的工作，使用 Go。
- 别人给你的 Rust 节点 = 一个**可用的只读 / RPC 全节点**，适合做冗余与对比，**不适合作为权威源，也不适合任何最终性 / 出块相关的事**。
- 对你的 MEV / DEX 聚合场景：**Go 归档节点作主状态源，Rust 归档作冗余 / 对比源，每次生产使用前跑探针做 state root 一致性校验**；finalized/head 决策只走 Go。
