# 依赖

`privnet.sh` 和 builder 端到端测试所依赖的版本，以及每个修复在上游的位置。固定的分支、提交或
PR 一旦变化，就更新本文件。

## 版本

| 组件 | 用于 | 来源 |
|---|---|---|
| reth-bsc | 除 `go` 外的所有模式 | [`akfork/reth-bsc` 分支 `integration/develop`](https://github.com/akfork/reth-bsc/tree/integration/develop)，提交 `d11e769`，已端到端测试（完整矩阵，见[验证](#验证)） |
| go-bsc（`geth`） | `go`、`mixed`、`canary` | [`bnb-chain/bsc` v1.8.1](https://github.com/bnb-chain/bsc/releases/tag/v1.8.1)（`947ba1cb1ef05d4a5f71b738113a3d8b4c16f6b7`），未修改 |
| rbuilder | builder 端到端测试 | `flashbots/rbuilder` 的 BSC 分叉，分支 `integration/develop`（`bsc-only` 与之一致）：`scripts/privnet-e2e.sh`，`BSC_NODE_DIR` 指向本仓库。它的 `reth_bsc` 库固定在 `d11e769`。 |
| 系统合约 | 创世 | 固定版本的 `bnb-chain/bsc-genesis-contract`，见 [GENESIS.md](./GENESIS.md) |

构建 reth-bsc：

```bash
git clone -b integration/develop https://github.com/akfork/reth-bsc
cd reth-bsc && cargo build --release --bin reth-bsc
export RETH_BIN=$PWD/target/release/reth-bsc
```

## reth-bsc：`integration/develop`

上游 `develop`（`738307f`）加上下面的提交，按改动分组。对上游代码的每一处改动，要么是一个开着的
上游 PR，要么标为仅 akfork。

| 提交 | 改动 | 用途 | 上游 |
|---|---|---|---|
| `3509cc3` `0c0e5f0` `926eabf` | 全新私有网能启动：eth Status 带真实的启动头，自定义链有自己的缺省值，内置链规格按创世区分 | 所有 reth-bsc 模式 | [bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546)，未合并 |
| `333f998` `a5513c7` | `rbuilder_*` 状态 RPC：账户、代码、区块哈希、状态根 | rbuilder 的 `STATE=ipc` 或扩展 HTTP 状态源 | 仅 akfork |
| `185d81b` | 最优出价淘汰与查询的调试日志 | 诊断出价选择 | 仅 akfork |
| `e8b2ff6` `0f32c12` `1d8e985` | `mev_params` 报告出价模拟器真实的预留时间（`BidSimulationLeftOver`：出块前 120 ms 内不再开始模拟） | builder 无需额外配置即可掌握最后一次出价的时机 | [bnb-chain/reth-bsc#604](https://github.com/bnb-chain/reth-bsc/pull/604)，未合并 |
| `c1617c8` `6bc8721` | 验证人在提交截止等待之后再读一次 BEP-322 出价存储，并按构建出的区块去重候选 | reth-bsc 验证人用 `mev_sendBid` 的出价出块 | [bnb-chain/reth-bsc#599](https://github.com/bnb-chain/reth-bsc/pull/599)，未合并 |
| `8eb0604` | 只有块头带 `withdrawalsRoot` 时，块体才带 withdrawals | go-bsc 节点从 reth-bsc 同步（`canary`） | [bnb-chain/reth-bsc#600](https://github.com/bnb-chain/reth-bsc/pull/600)，未合并 |
| `6a04ff5` `7af1ee3` | 出价对应的区块按出块节点的 gas 上限构建（而非 reth 的 30M 缺省值），`mev_params` 报告实时上限 | reth-bsc 验证人接受装满（达到 `GasCeil`）的出价 | [bnb-chain/reth-bsc#601](https://github.com/bnb-chain/reth-bsc/pull/601)，未合并 |
| `c3e171a` | 出价模拟器重新模拟最优出价时让出 tokio 工作线程（`block_in_place`），`mev_sendBid` 请求不再被卡住 | builder 的最后一次出价在截止前到达模拟器 | [bnb-chain/reth-bsc#603](https://github.com/bnb-chain/reth-bsc/pull/603)，未合并 |
| `62b4ce1` | 出价在独立任务中接收，更优的出价可以打断对最优出价的重新模拟，与 go-bsc 的 `newBidLoop` 一致 | 最后一次出价的满块（重负载两行）在截止前得到模拟 | [bnb-chain/reth-bsc#605](https://github.com/bnb-chain/reth-bsc/pull/605)，未合并（基于 #603） |
| `8a3b53f` `d11e769` | `rbuilder_*` 读取遇到磁盘回滚时重试 150 ms，仍失败则返回 `-32052`：重组替换了节点已持久化的区块时，reth 原地截断 changeset 文件，而更早开始的读取仍按旧长度读，于是读取失败（"failed to fill whole buffer"）或 reth 的静态文件读取器 panic（"slice index starts at … but ends at 0"）；读取改在阻塞线程上运行，panic 不再断开连接 | 会重组已持久化区块的网络（`multi`）上，builder 依赖节点计算的状态根 | 仅 akfork（竞争本身在 reth 的存储层：[paradigmxyz/reth#28225](https://github.com/paradigmxyz/reth/issues/28225)） |

所有 PR 都以上游 `develop` 为目标分支。合并后，上游 `develop` 构建即可运行所有模式；builder
测试仍需要上面那些仅 akfork 的提交。

## 本仓库

`master` 包含 builder 端到端的全部工作，由分支 `builder-e2e-hardening` 快进合并而来
（2026-10-31）：带可用快速终局的固定创世配置、加固后的 `privnet.sh`、go-bsc 出块节点接收
BEP-322 出价、go-bsc 出块节点投票，以及带配置化 builder 白名单的 `canary` 模式。

## 验证

最近一次完整验证：2026-11-01，reth-bsc `d11e769`、go-bsc v1.8.1、本仓库 `master`
（`ea8c62b` 加本文件）、rbuilder `integration/develop`（`644d30e`），与生产环境一样只运行
并行 builder（16 个冲突解决线程，完整排序）。rbuilder 发布矩阵的全部 27 行均通过，排序
builder 与并行 builder 同时运行的带 BidBlock 和负载的 `multi` 行也通过；两个浸泡测试与其余
各行并行运行。每一行最多 1 个 builder 出价来不及模拟（出块节点崩溃那一行，共 142 个）。
4 小时浸泡测试有 698 236 个订单上链，即所有有效订单，32 006 个区块由 builder 的出价封装，
64 006 个出价中 0 个来不及模拟；`canary` 浸泡测试中两种客户端共用其 BidBlock 封装了
2 194 个区块。每秒 400 个订单时，16 和 32 个冲突解决线程分别使用 8.7 和 14.2 个核。

同一天的上一次验证，reth-bsc `62b4ce1`、本仓库 `master`（`3429df6`）、rbuilder `bsc-only`
（`694c7d0`，排序 builder）：rbuilder 发布矩阵（其 `docs/testing.md`）的全部 27 行均通过（4 小时浸泡测试与其余各行并行，其余各行逐行运行）：
所有模式（`fast`、`multi`、`pasteur`、`go`、`mixed`、`canary`），HTTP 与 IPC 状态源，`self`
与 `pay_account` 付款方式，BEP-322 出价与 BEP-675 BidBlock（JSON-RPC 与 gRPC），blob、breathe
与终局奖励区块，含 7M gas 订单区块的 BEP-703 付款通道，节点冻结、网络故障、丢失一个区块的
出块节点崩溃，30 分钟 `canary` 浸泡测试（两种客户端共用 builder 的 BidBlock 出了 2 075 个块），
以及每秒 50 个订单的 4 小时 `fast` 浸泡测试（698 220 个订单上链，即所有有效订单；
32 006 个区块由 builder 的出价封装，64 007 个出价中 1 个来不及模拟；builder 内存
116–136 MiB）。每一行中，reth-bsc 验证人因时间不够而没有模拟的 builder 出价最多 3 个
（IPC 重负载那一行，共 272 个）。

这次验证发现并修复了四个 bug。reth-bsc 的 `mev_params` 报告的是 go-bsc 的模拟预留时间，而不是
它自己的 120 ms 截止（`e8b2ff6`，#604）；它的出价模拟器在重新模拟时占住一个 tokio 工作线程，
卡住了 `mev_sendBid`（`c3e171a`，#603）：每一个最后出价都来得太晚。更优的出价无法打断 reth-bsc
对最优出价的重新模拟，满块的最后出价只能排在后面等待（`62b4ce1`，#605）。rbuilder 发出满块最后出价的
时间太晚，不够它 75–106 ms 的模拟（rbuilder `3117909`）。

此后（2026-11-01），从未运行过的 rbuilder 并行 builder 被修复（rbuilder `96fd974`、
`ecffc96`），在 reth-bsc `62b4ce1` 上通过了矩阵中的 21 行和 20 分钟浸泡测试。排序 builder 与
并行 builder 同时运行时，带 BidBlock 和负载的 `multi` 行失败：reth-bsc 的
`rbuilder_calculateStateRoot` 在磁盘回滚期间返回 "failed to fill whole buffer"，或在 reth 的
静态文件读取器 panic 时断开连接；使用 `d11e769` 和 rbuilder `1fe99dc` 后，该行连续四次通过。

再上一次验证（2026-10-31，reth-bsc `6a04ff5`）发现了三个：rbuilder 为一个因自己刚出过轮外块而被
Parlia 禁止出块的轮值验证人构建区块；rbuilder 丢弃了被重组（或崩溃的节点）收回的区块中的订单；
reth-bsc 按 30M 而不是自己的 gas 上限构建出价区块（即上面的 `6a04ff5`）。
