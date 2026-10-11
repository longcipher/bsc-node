# BSC 私有网实验：Go 与 Rust 互操作

English: [privnet.md](./privnet.md)

可复现环境：[`privnet/`](../privnet/)（共享 `genesis.json` + `privnet.sh`）。
关联分析：[bsc-diff.zh.md](./bsc-diff.zh.md)。

> 实验对象（2026-10-11 基于本地 checkout 构建）：
>
> - 第一轮：Go `bnb-chain/bsc` @ `c5533ab5b`（二进制自报 `v1.8.0-alpha`），
>   Rust `bnb-chain/reth-bsc` @ `98ef6c5`（二进制自报 `0.1.2-1-g98ef6c5`）。
> - 第二轮（§10）：Go @ `8cc60b246`（**develop**），Rust @ `349f55c`
>   （**develop**，二进制自报 `v0.0.8-alpha-198-…`，因 `v0.1.x` tag 只在
>   `main` 上）。4 个修复中 3 个在 develop 重打（解码 panic 上游已修）；
>   `privnet.sh` 原样跑通全部模式。
>
> 以下结论立足三点：源码阅读、**真实私有网联调实验**、**2026-10-11 当天联网核验**
> （release、硬分叉时间表、验证人集合）。此前"未做联网调研"的保留意见不再适用。

## 1. 环境

共享创世（`privnet/genesis.json`）：`chainId: 1337`，Parlia `period: 3s`、`epoch: 200`，
单个开发验证人 `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A`（私钥 `0x1111…1111`）。
`alloc` 未部署系统合约（这在 epoch 边界很关键，见 §5）。
两端吃同一文件：Go 用 `geth init`，Rust 用 `--chain <文件>`。

| 节点 | 角色 | P2P | HTTP RPC |
|---|---|---|---|
| go1 | 出块（go/mixed 模式） | 30311 | 8545 |
| go2 | 跟随 | 30312 | 8547 |
| rust1 | 出块（rust）/ 跟随（mixed） | 30313 | 8549 |
| rust2 | 跟随 | 30314 | 8553 |

Go↔Go 用 `admin.addPeer` 互联（`--nodiscover` 下 `--bootnodes` 不会被拨号）；
Rust 用 `-d`（关闭发现）加 `--trusted-peers` 静态 enode。Rust 出块必须启动时开启
（`--mining.enabled --mining.private-key`）；Go 出块可在运行时开关
（`miner_stop`/`miner_start`）。

## 2. 测试一 —— 纯 Go 网络

`./privnet.sh go`：go1 出块，go2 跟随。

- go2 几秒内追上 tip 并保持同步（检查时 `0x1c`/`0x1c`）。
- Legacy 交易（10 gwei，本创世无 London、无 base fee）在 `0x20` 块落盘，两端回执
  `status: 0x1`。
- 结论：**通过**。作为后两组测试的基线。

## 3. 测试二 —— 纯 Rust 网络

`./privnet.sh rust`：rust1 从创世出块，rust2 经 `eth/69` 跟随。

- **修复前：彻底死锁。** 两节点永远停在 0 块，`BlockhashZero` 握手拒绝 + ban 循环
  （§6.1）。无 peer → 矿工 peer 门打不开 → 永不出块。
- **修复后：通过。** rust1 从 1 块开始密封（7 笔系统交易，与 Go 建块结构一致），
  rust2 跟随（`0x11`/`0x11`，各 1 peer，零 `blockhash is zero`）。哈希与 Go 历史
  运行不同仅因墙钟时间戳，符合预期。

## 4. 测试三 —— 混合网络（Go + Rust）

`./privnet.sh mixed`：go1 出块；go2、rust1、rust2 跟随。

- **握手矩阵（`admin_peers` 与节点日志实测）：**
  Rust↔Go 协商 **`eth/68`**（Go 提供 `{70, 68}`，Rust 侧 reth fork 提供
  `{69, 68, 67, 66}`）；Rust↔Rust 协商 **`eth/69`**。两种组合都完成 BSC
  `UpgradeStatus`（`0x0b`）往返；`bsc/1` + `bsc/2` 同样协商成功。四节点
  `net_version` 均为 1337。
- **状态收敛：** 1~198 块四节点哈希逐块一致；经 Go 提交和经 Rust 提交的用户交易
  都在四端执行一致（回执、余额、`stateRoot` 全同，如 `0x5a` 高度四端同一 stateRoot）。
- **出块权双向交接**（`handover-to-rust`、`handover-to-go`）：go1 `miner_stop` +
  rust1 带出块重启后，Rust 连出 13+ 块，Go 全盘接受（哈希/stateRoot 四端一致）；
  交回 Go 同样成功。Rust 建块携带正确的系统交易（`deposit 0xf340fa01` →
  `0x1000`、slash 指示调用 → `0x1002`）。
- **同钥双出块（误配置探针）：** 两端同时出块时 tip 在两分叉间抖动（`need_reorg`
  churn），但一两块内收敛。实际使用保持单一生产者。

## 5. 无合约链的 epoch-200 边界

没有部署系统合约时，两端都组装不出 epoch-200 的验证人集合：

- Go 停在 199 块，每次重试打 `Failed to prepare header for sealing: abi:
  attempting to unmarshal an empty string`——存活、RPC 正常。
- Rust **修复前崩溃**：构建 199 块时 `abi_decode_output(&[]).unwrap()` 以
  `SolTypes(Overrun)` panic，出块任务死 → 引擎任务死（`Fatal error in consensus
  engine`）→ 进程退出。
- Rust **修复后与 Go 同级降级**：`failed to decode getValidators output:
  buffer overrun`，零 panic，双进程存活、RPC 正常（停在 198，Go 停 199——Rust
  建块路径的验证人集合缓存刷新早一块触发，同样优雅）。

## 6. 在 `reth-bsc` 中发现并修复的 bug

所有修复都在 Rust 仓库；Go 作为参照实现无需改动。
它们位于 [`akfork:fix/fix-privnet`](https://github.com/akfork/reth-bsc/tree/fix/fix-privnet)
分支，已向上游提 [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546)；
合并后上游 `develop` 构建即包含。

### 6.1 零哈希初始 Status 锁死新网络（致命）

`src/node/network/mod.rs`：`BscNetworkBuilder` 用写死的
`BscChainSpec::head()`（未来块号、**零 blockhash**）填充初始 Status，
弃用了数据库真实头（`BuilderContext::head()`，新库即创世——与上游 reth 缺省、
Go 真实头一致）。任何 `eth/69+` 对端以 `BlockhashZero` 拒绝，导致全新
Rust-only 网络永远连不上 peer（§3）。已改为 `.set_head(ctx.head())`；fork
filter 与 fork id 同源，保持一致。配套：`BscChainSpec::head()` 自定义链分支锚定
创世（`src/chainspec/mod.rs`），首块前 Status 即正确。

### 6.2 系统合约解码 panic 干掉节点（致命）

`src/system_contracts/mod.rs` 6 个 `unpack_*` 对合约返回做 `.unwrap()`；
空/畸形返回（缺合约）直接 panic 出块任务，导入路径上还会拖死引擎任务（§5）。
6 个函数全部改为返回 `Result<_, SystemContractError>`（新增
`DecodeOutput`/`MalformedOutput` 变体）；
`src/node/evm/pre_execution.rs`、`post_execution.rs` 向上传播，仅 EVN NodeID
富化保持 best-effort。出块/导入失败只打日志——与 Go 同级。

> develop 状态：上游已独立修复（`Option` 方案＋显式 `Empty eth call output` /
> `new_payload returned Invalid` 路径）——§10 重跑验证优雅，此修复未再重打。

### 6.3 `--chain local` 通告错误的链 ID

`src/chainspec/local.rs` 把 `chain` 写死成 BSC 主网（56），而其创世 `chainId`
是 12345，Status `networkID` 与同创世的任何 peer 都对不上。已改为读创世文件；
删除多余 `head()`；单测断言 id 为 12345 且 fork id（`e4d5334c`）不变。

### 6.4 自定义链拨号公网

`BscChainSpec::bootnodes()` 对未知链返回 `None`，触发上游回退到**以太坊主网**
bootnode（实测：大量外拨 + 被 ban）。现返回显式空集合；
`--bootnodes`/`--trusted-peers` 覆盖逻辑不变。

细化（PR review）：仅凭链 kind 无法证明来源——复用注册链 ID（如 56）的自定义
创世经 `Chain::from_id` 仍判为 `ChainKind::Named`。因此 `bootnodes()` 与
`head()` 都改用 `BscChainSpec::is_bundled()` 把关，以创世哈希比对打包好的
主网/测试网/qanet 哈希；其余一律按自定义处理。覆盖单测
`test_custom_genesis_with_registered_chain_id_stays_custom`。

### 修复验证

- `cargo test`：第一轮 449/449 单测通过；develop 重跑 755 个 lib 单测
  （0 失败，1 忽略），含 4 个新增回归单测（创世锚定头及空自定义 bootnode、
  打包链 bootnode 不变、本地链 id、chain-id-56 自定义创世）及 fork-id 扩展覆盖；
  第一轮的空解码返回 Err 上游已有覆盖。
- `cargo clippy -D warnings`：改动文件零新增（35 个剩余告警均早于本次改动）。
- 端到端：§3（修复后）、§4、§5（修复后）全部用修复版 release 二进制经
  `privnet.sh` 重跑（`go`、`rust`、`mixed`、双向交接、`status`/`stop`/`clean`），
  后在 `develop` 上重复（§10）。

## 7. Go 侧观察（无需改动）

- `authrpc.*` flag 被编译剔除（`cmd/geth/main.go` 注释掉）——BSC 内嵌共识，
  没有 beacon Engine API。by design。
- 极小私链上默认 snap 同步失败（`missing trie node ... layer stale`）；
  `--syncmode full` 正常。已编码进 `privnet.sh` 的运维说明。
- `--nodiscover` 下 `--bootnodes` 不会被拨号；静态互联需 `admin.addPeer`。
- Go 同样忽略创世 `parlia.period/epoch`（`ParliaConfig` 为空结构体；
  200 块 / 3000ms 写死，随分叉切换）——一度怀疑是 Rust 缺口，经源码比对确认为
  完全一致，相关改动已在测试前回滚。

## 8. 已知剩余缺口（未修，超出私网范围）

- **导入不验 seal 授权**。Rust 从不校验收块的恢复签名人是否为授权验证人
  （Go `verifySeal`：签名人∈集合、difficulty、coinbase 一致、double-sign 缓存）。
  诚实网无感；主网对抗 robustness 相关。
- **静默 peer 不回填**。新 Rust 节点不会从停滞 peer 的 Status 头做 staged 同步，
  需要活块公告。已在 peer 到停滞 Go 节点时观察到（停在 0，直到生产者恢复）。
- `miner_start` RPC 不启动密封循环（需启动 flag）；
  `eth_getTransactionsByBlockNumber` / `eth_getTransactionDataAndReceipt` 缺失
  （已核验；旧文档写"待逐一核对"——现已核对）。
- 两端都不支持 LES/轻同步；Rust 无 snap 协议、graphql、ethstats、clef、devp2p 工具。

## 9. 联网核验（2026-10-11 复核）

- Go：最新 tag `v1.8.0-alpha`（2026-08-13）是明确的**预览版，不可用于生产**；
  生产序列最新为 `v1.7.8`。`v1.8.0-alpha` 跟踪 go-ethereum `v1.17.3`，
  `eth/70` 为主协议同时保留 `eth/68`，未宣布移除。
  （<https://github.com/bnb-chain/bsc/releases>）
- Rust：README 仍写 `Run as validator ❌ (soon)`，但出块能力早已发布
  （`v0.0.6-beta`，2025-12-10，"Validator-Enabling"；最新 `v0.1.2`，
  2026-08-20，BEP-675 `mev_sendBidBlock` + Pasteur 修复）。无主网验证人
  运行它的公开证据；免责声明仍为 "experimental"。
  （<https://github.com/bnb-chain/reth-bsc/releases>）
- 硬分叉均按时激活：Fermi 主网 2026-01-14，Osaka/Mendel 2026-04-28，
  Pasteur 2026-08-25（测试网各早约一两个月）。
  （<https://docs.bnbchain.org/announce/fermi-bsc/>，
  .../mendel-bsc/，.../pasteur-bsc/）
- 验证人集合为 **45 个活跃**（21 Cabinets + 24 Candidates），并非"仅 21"；
  每 epoch 共识集为 21。
  （<https://docs.bnbchain.org/bnb-smart-chain/validator/overview/>）
- `bsc-snapshots` 仍按月发布 reth 快照（v2 布局，TiB 级）。
  （<https://github.com/bnb-chain/bsc-snapshots/>）

## 10. develop 分支重跑（2026-10-11）

§2~§5 已在 Go @ `8cc60b246`、Rust @ `349f55c`（均为 **develop**）上重复，
其余 3 个修复重打（§6.1、§6.3、§6.4）：

- 混合 2+2：结果一致——同样握手矩阵（对 Go `eth/68`、Rust 间 `eth/69`）、
  哈希/stateRoot 收敛、交易双向、出块权经 `privnet.sh` 双向交接。
- 纯 Rust 从创世：互联出块，零 `blockhash is zero`。
- epoch-200 边界：两侧优雅停滞、零 panic。解码 panic（§6.2）**上游已修**
  （`Option` 方案＋显式日志），效果与第一轮验证的修复等价，故未重打。
- develop 新增、已实测：`parlia_getValidators`/`getFinalizedNumber` 正常；
  `admin_nodeInfo` 有响应；`traceBlock*`（OpenEthereum 风格，`debug` 模块）、
  `eth_createAccessList` 已接入（本无 London 链上后者拒旧类型，符合预期）；
  parlia 的 `safe`/`finalized` 标签现从真实 canonical 标记解析，不再替换
  为 `latest`（见 bsc-diff §5.3）。
- Jenner（BEP-706/714）只在 develop 存在且**仅 qanet**（2026-10-05 已激活）；
  主网/测试网未排期，两端 BEP-714 backoff 镜像一致——本次测试（无时间分叉）
  无互操作影响。
