# BSC 私有网络（`privnet/`）

一份共享创世文件、一个脚本，三种组网模式：纯 Go、纯 Rust、混合（Go + Rust）。

- 共享链：`genesis.json` —— `chainId: 1337`，Parlia `period: 3s`、`epoch: 200`，单个开发验证人 `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A`（私钥 `0x1111…1111`，**绝不能用于主网/测试网**）。
- 脚本：[`privnet.sh`](./privnet.sh) —— 启动/停止/检查网络，自动完成节点互联。

完整实验报告（搭建过程、结果、发现并修复的 bug）：[docs/privnet.zh.md](../docs/privnet.zh.md)。

## 环境要求

- 由 `bnb-chain/bsc` 编译出的 `geth`（`make geth`），或设置 `GETH_BIN`。
- `rust`/`mixed` 模式还需要含私有网修复的 `reth-bsc`（Status 头修复、自定义链缺省值；见 [docs/privnet.zh.md](../docs/privnet.zh.md)），或设置 `RETH_BIN`。在上游 PR [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546) 合并前，请从我们的分支构建（`https://github.com/akfork/reth-bsc`，分支 `fix/fix-privnet`）；合并后上游 `develop` 构建即可。
- `curl`、`python3` 等常规 Unix 工具。`cast`（Foundry）可选，用于发送测试交易。

## 快速开始

```bash
# 纯 Go：go1（出块）+ go2（跟随）
GETH_BIN=/path/to/geth ./privnet.sh go

# 纯 Rust：rust1（出块）+ rust2（跟随）
RETH_BIN=/path/to/reth-bsc ./privnet.sh rust

# 混合：go1（出块）+ go2 + rust1 + rust2（跟随）
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc ./privnet.sh mixed

# 查看 / 停止 / 清理
./privnet.sh status
./privnet.sh handover-to-rust   # 仅混合：出块权交给 rust1
./privnet.sh handover-to-go     # 仅混合：出块权交回 go1
./privnet.sh stop
./privnet.sh clean              # 停止并删除链数据
```

`GETH_BIN`/`RETH_BIN` 默认取 `PATH` 中的 `geth`/`reth-bsc`。运行时数据在 `./data`（可用 `PRIVNET_DIR` 覆盖）。

## 拓扑与端口

| 节点 | 角色 | P2P | HTTP RPC | 说明 |
|---|---|---|---|---|
| go1 | 出块（go/mixed） | 30311 | 8545 | `--miner.recommit 1s` |
| go2 | 跟随 | 30312 | 8547 | `--syncmode full`（见下） |
| rust1 | 出块（rust）/ 跟随（mixed） | 30313 | 8549 | authrpc 8555，`-d`（关闭发现） |
| rust2 | 跟随 | 30314 | 8553 | authrpc 8556，`-d`，trusted 指向 rust1 |

节点互联全自动：Go 节点间用 `admin.addPeer`；Rust 节点用 `--trusted-peers`（启动时从日志抓取静态 enode）。

## 运行注意事项（均为实测结论）

- **Go 跟随节点必须 `--syncmode full`。** 默认 snap 模式在极小私链上失败（`missing trie node ... layer stale`）。
- **Rust 节点必须 `-d`（关闭发现）。** 自定义链没有内置 bootnode，上游 reth 会回退到以太坊主网 bootnode，导致节点拨号公网并被 ban。修复版二进制已让自定义链默认空 bootnode。
- **Rust 出块必须在启动时开启**（`--mining.enabled --mining.private-key ...`）。`miner_start` RPC 虽然返回成功，但不会启动密封循环；Go 的 `miner_stop`/`miner_start` 运行时有效，`handover-to-go` 依赖这一点。
- **Go 的 `--nodiscover` 下 `--bootnodes` 不会被拨号。** 本脚本 Go↔Go 用 `admin.addPeer` 直连。
- **链会在 199 块附近停滞。** 共享创世文件没有部署系统合约，两端都组装不出 epoch-200 的验证人集合：Go 报 `Failed to prepare header for sealing` 停在 199；Rust 报 `failed to decode getValidators output` 停在 198。两端都存活并继续服务 RPC——这是同级的优雅降级。如需更长的运行，把数据目录清掉重跑即可（同一创世文件仍可用：chainId/创世哈希不变，新块用墙钟时间戳）。
- **不要用同一验证人私钥同时跑两个出块节点。** 两端都按 ~3s 出块，tip 会在两分叉间抖动（最终会收敛，但日志全是重组噪音）。切换出块权请用 handover 动作，保持单一生产者。

## 「每日呼吸块」问题

**呼吸块（breathe block）**指父块处于上一个 UTC 自然日的块，它是
`updateValidatorSetV2` 唯一会执行的地方（`consensus/parlia/feynmanfork.go`），
而它恰好也是私网无法完成的唯一一笔系统交易，原因有两条且互相叠加：

1. `bsc-genesis-contract` 的 `genesis-dev.json` 里部署在 `0x1000` 的是
   **Feynman 之前**的 `BSCValidatorSet` —— 其运行时代码里根本没有
   `updateValidatorSetV2` 的选择器（`0x6550d581`），只有 `deposit` 之类。
2. 正常情况下 go-bsc 本会替换它，但内置的升级表按创世哈希索引，只有
   `mainnet`/`chapel` 两个条目（`feynmanUpgrade[defaultNet]` 为 `nil`，
   `applySystemContractUpgrade` 对 nil 表直接 no-op）。自定义链因此永远不会被升级。

于是该调用落到合约分发器的 default 分支，以 `invalid opcode: INVALID`
**在组装区块的过程中**停机。这不是一笔可以跳过的失败交易——区块根本组装不出来，
矿工会永远重试，链彻底死亡，只能清掉数据目录重来。注意这**不只是"停机后重启"的问题**：
只要跨过 UTC 自然日边界就会触发，不管链是否在运行。所以普通的 7×24 私网会在
1 号块之后 24 小时内必然死掉。

修复方式是把这条规则关掉。`isBreatheBlock` 属于共识规则，**每个节点**必须得出
相同判定，而两个客户端都为此提供了覆盖开关：

| 客户端 | 覆盖方式 |
|---|---|
| go-bsc | `--override.breatheblockinterval <秒>` |
| reth-bsc | `BREATHE_BLOCK_INTERVAL=<秒>`（环境变量） |

`privnet.sh` 给全部 4 个节点传同一个 `BREATHE_BLOCK_INTERVAL`，并默认设为 `1e15`
——大于任何真实时间戳，因此任意两块都不可能落入不同的时间桶。具体用法：

```bash
# 默认：关闭呼吸块，可无限期运行
./privnet.sh mixed

# 若确实想按主网节奏（86400）做每日刷新；则每个 UTC 午夜都需要清数据目录重启
BREATHE_BLOCK_INTERVAL=86400 ./privnet.sh mixed
```

已端到端验证：`--override.breatheblockinterval=120` 时，链在 1 号块后两分钟内即停滞；
`1e15` 时链可以持续跨过同样边界，4 个节点的 hash 与 `stateRoot` 逐字节一致，
且 `just e2e-evmdb-bsc` 与 `just e2e-fork-db-bsc` 全部通过。切勿在已运行的链上
修改该间隔——区块布局会分叉。

用 `just privnet-verify-breathe` 可复跑该验证（约 8 分钟）：它会把网络启动两次，
一次用 120s 间隔（必须停滞），一次用当前默认值（必须持续出块），最后检查 4 个
节点的 hash 与 `stateRoot` 是否仍然一致。`--case-b` 只跑后半段。脚本退出时会
停机并清空数据目录，所以请在不依赖在线私网时运行。
