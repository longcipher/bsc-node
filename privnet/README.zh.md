# BSC 私有网络（`privnet/`）

生成的创世文件、一个脚本、六种组网模式：3 秒出块的纯 Go、纯 Rust、混合（Go + Rust），
以及按主网节奏运行的纯 Rust 网络（450 ms 出块、Pasteur、三验证人且 turn length 为 4），
供区块构建者（builder）做端到端测试。

- 链：`chainId: 1337`，系统合约取自固定版本的 `bnb-chain/bsc-genesis-contract`，
  并带 Luban 之后的 BLS 投票地址，所以快速终局（fast finality）正常运行、`finalized` 持续前进。
  详见 [GENESIS.md](./GENESIS.md)。
- 脚本：[`privnet.sh`](./privnet.sh) 负责启动、校验、停止、清理网络，并自动完成节点互联。
- **仅限开发（DEV-ONLY）的私钥。** 所有验证人、BLS、builder 和测试账户私钥都是公开的
  （[`accounts.json`](./accounts.json)），绝不能在 BSC 主网（56）或测试网（97）上充值或使用；
  脚本会拒绝这两个 chain id。

完整实验报告（搭建过程、结果、发现并修复的 bug）：[docs/privnet.zh.md](../docs/privnet.zh.md)。

## 环境要求

- `go`/`mixed` 模式：由 `bnb-chain/bsc` 编译出的 `geth`（`make geth`），或设置 `GETH_BIN`。
  缺少时这两个模式以退出码 77（跳过）结束，并给出明确提示。
- 其余模式：包含我们修复的 `reth-bsc`，或设置 `RETH_BIN`。请从
  [`akfork/reth-bsc` 分支 `integration/develop`](https://github.com/akfork/reth-bsc/tree/integration/develop)
  构建：上游 `develop` 加上私有网修复
  （[bnb-chain/reth-bsc#546](https://github.com/bnb-chain/reth-bsc/pull/546)）、builder 修复
  （[#599](https://github.com/bnb-chain/reth-bsc/pull/599)、
  [#600](https://github.com/bnb-chain/reth-bsc/pull/600)、
  [#601](https://github.com/bnb-chain/reth-bsc/pull/601)、
  [#603](https://github.com/bnb-chain/reth-bsc/pull/603)、
  [#604](https://github.com/bnb-chain/reth-bsc/pull/604)）以及仅 akfork 的提交（builder API、
  模拟器运行时接收出价）。
  [DEPENDENCIES.zh.md](./DEPENDENCIES.zh.md) 列出所有固定的分支、提交和 PR。
- `bash` 4.4+、`curl`、`python3` 等常规 Unix 工具。`cast`（Foundry）可选，用于发送测试交易。

## 快速开始

```bash
# 纯 Rust，3 秒出块：rust1（出块）+ rust2（跟随）
RETH_BIN=/path/to/reth-bsc ./privnet.sh rust

# 纯 Rust，主网节奏（450 ms）；带 Pasteur（BEP-675）；或三个验证人
RETH_BIN=/path/to/reth-bsc ./privnet.sh fast
RETH_BIN=/path/to/reth-bsc ./privnet.sh pasteur
RETH_BIN=/path/to/reth-bsc ./privnet.sh multi

# 纯 Go 与混合（需要 go-bsc）
GETH_BIN=/path/to/geth ./privnet.sh go
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc ./privnet.sh mixed

# 两种客户端同时做验证人（同 BSC 测试网），并把 builder 加入白名单
GETH_BIN=/path/to/geth RETH_BIN=/path/to/reth-bsc \
  BUILDERS=0x7564105E977516C53bE337314c7E53838967bDaC ./privnet.sh canary

# 查看 / 校验 / 停止 / 清理
./privnet.sh status
./privnet.sh health             # 网络不健康时以非零码退出
./privnet.sh env                # 供 builder e2e 脚本使用的 export
./privnet.sh accounts           # 仅限开发的测试账户
./privnet.sh handover-to-rust   # 仅混合：出块权交给 rust1
./privnet.sh handover-to-go     # 仅混合：出块权交回 go1
./privnet.sh stop
./privnet.sh clean              # 停止并删除链数据、投票日志、IPC 套接字
```

`GETH_BIN`/`RETH_BIN` 默认取 `PATH` 中的 `geth`/`reth-bsc`。运行时数据在 `./data`
（可用 `PRIVNET_DIR` 覆盖）。`start` 只有在下文的健康检查通过后才返回；检查不通过时
以非零码退出，并停掉本次启动的节点。全部动作与变量见 `./privnet.sh --help`。

## 模式与创世配置

| 模式 | 节点 | 创世文件（profile） | 出块 | 说明 |
|---|---|---|---|---|
| `go` | go1（出块）、go2 | `genesis.json`（classic） | 3 s | 需要 go-bsc |
| `rust` | rust1（出块）、rust2 | `genesis.json`（classic） | 3 s | |
| `mixed` | go1（出块）、go2、rust1、rust2 | `genesis.json`（classic） | 3 s | 需要 go-bsc；支持 handover |
| `fast` | rust1（出块）、rust2 | `genesis-fast.json`（fast） | 450 ms | 创世即激活 Lorentz、Maxwell、Fermi |
| `pasteur` | rust1（出块）、rust2 | `genesis-pasteur.json`（pasteur） | 450 ms | 再加 Osaka、Mendel、Pasteur：`BidBlockEnabled: true` |
| `multi` | rust1、rust2、rust3（都出块） | `genesis-multi.json`（multi） | 450 ms | 3 个验证人，turn length 4，Pasteur 硬分叉集 |
| `canary` | rust1、rust2、go1（都出块） | `genesis-multi.json`（multi） | 450 ms | 需要 go-bsc；即 `multi`，但 3 号验证人跑 go-bsc |

`PROFILE=fast`（或 `pasteur`）可让 `go`、`rust`、`mixed` 改用该创世文件运行；与 go-bsc
的组合未经测试。fast 系列复现主网的节奏：创世起依次为 1.5 s、0.75 s，然后 0.45 s 出块间隔；
从 500 块起 epoch 为 500，从 1000 块起为 1000。BidBlock 准入要求链头已激活 Pasteur，
而创世把它门控在 London（8 号块）之后，所以前 8 个块里 `mev_params` 报告
`BidBlockEnabled: false`。

`multi` 模式下，1-200 块每个验证人轮流出一个块（与 BSC 一致，创世快照的 turn length 为 1）。
从第一个 epoch 边界起，验证人集合与 turn length 都取自 `BSCValidatorSet`：
此后每个验证人连续出 4 个块，builder 每 1.8 s 就会遇到一次轮次切换。

`canary` 运行 `multi` 的链，但由 go1 担任 3 号验证人，于是 builder 在一轮轮换中会向两种客户端
出价，与 BSC 测试网相同。rust1、rust2 先启动并出块（三个验证人中的两个就能维持出块），go1 以
full 模式同步完成后才 `miner_start`：若 go1 一启动就出块，它会在连上 peer 之前出自己的 1 号块，
并一直停在那条分叉上。go-bsc 只能从这样的 reth-bsc 同步：London 之前的块体不带 withdrawals
（akfork/reth-bsc `integration/develop` 上的 `8eb0604`；上游
[bnb-chain/reth-bsc#600](https://github.com/bnb-chain/reth-bsc/pull/600)）。更早的版本给这些块体带上
`withdrawals: []`，go-bsc 会以 `retrieved block body is invalid` 断开该 peer。

### Builder 白名单

`BUILDERS=0xA,0xB` 按验证人运营方的方式把 builder 加入白名单，所有模式都适用：go-bsc 出块节点
在配置文件里写入 `[[Eth.Miner.Mev.Builders]]`，reth-bsc 出块节点在环境变量里设置
`BSC_ALLOWED_BUILDERS`。名单随网络记录下来，handover 重启节点时会沿用。`miner_addBuilder`
仍然可用，但重启后通过它添加的 builder 会丢失。

## 拓扑、端口与 IPC

| 节点 | P2P | HTTP RPC | authrpc | mev gRPC | IPC |
|---|---|---|---|---|---|
| go1 | 30311 | 8545 | - | - | `$IPC_DIR/go1.ipc` |
| go2 | 30312 | 8547 | - | - | `$IPC_DIR/go2.ipc` |
| rust1 | 30313 | 8549 | 8555 | 8552 | `$IPC_DIR/rust1.ipc` |
| rust2 | 30314 | 8553 | 8556 | 8554 | `$IPC_DIR/rust2.ipc` |
| rust3 | 30315 | 8557 | 8558 | 8559 | `$IPC_DIR/rust3.ipc` |

- `PORT_OFFSET` 会加到每个端口上，因此可以在第一个网络旁再跑一个：
  `PRIVNET_DIR=/tmp/pn2 PORT_OFFSET=1000 ./privnet.sh fast` 让 rust1 监听 9549。
  `start` 遇到已被占用的端口会拒绝启动；若该端口上应答的是 BSC 主网或测试网节点，
  会明确指出并同样拒绝。
- `IPC_DIR` 默认是 `$PRIVNET_DIR/ipc`。Unix 套接字路径最长 107 字节，
  路径过长时 `start` 会给出明确报错；此时把 `IPC_DIR` 设为更短的目录即可。
- 所有监听都绑定 127.0.0.1（包括 P2P），节点发现关闭。Go 节点用 `admin_addPeer` 互联；
  Rust 节点用 `--trusted-peers` 加 `admin_addTrustedPeer`，enode 一律从 `admin_nodeInfo`
  读取（取不到即报错退出）。
- mev gRPC 监听（BEP-675 `SendBidBlock`）只在出块节点上运行。

## 健康检查

`./privnet.sh health`（每次 `start` 结束时都会运行）依次检查，并打印每一项失败：

1. 每个节点进程存活（pidfile，并对照 `/proc` 校验）、RPC 有响应、chain id 与创世一致（绝不是 56/97）。
2. 链头在 `HEALTH_TIMEOUT`（3 s 出块为 90 s，450 ms 为 45 s）内于每个节点上前进 `HEALTH_BLOCKS`（5）个块。
3. 每个节点都有应有的 peer：1 个；`mixed` 中的 go1 以及 `multi`、`canary` 中每个节点为 2 个。
4. 所有节点在同一近期高度上的哈希相同。
5. `multi`/`canary`：最近 48 个块里三个验证人都出过块。
6. `eth_getBlockByNumber("finalized")` 大于 0，且在 `FINALITY_TIMEOUT`（120 s / 60 s）内于每个节点上前进。
   所有模式下都是必选项：go-bsc 出块节点也用导入 BLS 钱包的、所属验证人的 BLS 私钥投票
   （`REQUIRE_FINALITY=0` 改为警告）。
7. `pasteur`/`multi`/`canary`：每个出块节点的 `mev_params` 都报告 `BidBlockEnabled: true`。

所有脚本的退出码：`0` 正常、`1` 失败、`2` 用法错误、`77` 跳过（缺少 go-bsc）。其他开关：
`START_HEALTH=0`（`start` 时跳过检查）、`KEEP_ON_FAILURE=1`（`start` 失败时保留节点以便排查）、
`VOTE_WARMUP_BLOCKS`（出块节点开始投票前等待的块数，默认 3）、`ETH_PROOF_WINDOW`
（reth `--rpc.eth-proof-window`，默认 100000，供通过 `eth_getProof` 读状态的 builder 使用）、
`RETH_EXTRA_ARGS` 与 `GETH_EXTRA_ARGS`（例如 `--verbosity 4`）。

## Builder 端到端测试

`./privnet.sh env` 先确认目标不是 56/97 链，再打印 builder e2e 脚本所需的 export：

| 变量 | 含义（默认端口） |
|---|---|
| `PRIVNET_CHAIN_ID` | `1337` |
| `PRIVNET_RPC_URL` | 第一个出块节点的 HTTP RPC（rust1：`http://127.0.0.1:8549`；`go`/`mixed` 中为 go1） |
| `PRIVNET_IPC_PATH` | 第一个出块节点的 IPC 套接字（`$PRIVNET_DIR/ipc/rust1.ipc`） |
| `PRIVNET_MEV_GRPC` | 第一个出块节点的 BEP-675 gRPC（`127.0.0.1:8552`） |
| `PRIVNET_VALIDATOR_RPC_URLS`、`PRIVNET_VALIDATOR_IPC_PATHS` | 全部出块节点，1 号验证人在前（`multi`：8549、8553、8557；`canary`：8549、8553、8545） |
| `PRIVNET_FOLLOWER_RPC_URL` | 一个不出块的节点（rust2：8553） |
| `PRIVNET_BLOCK_INTERVAL_MS` | `3000` 或 `450` |
| `PRIVNET_GENESIS` | 节点所用创世文件（builder 的 `chain` 也应指向它） |
| `PRIVNET_BUILDER_ADDRESS`、`PRIVNET_BUILDER_KEY` | 已充值的 builder 账户 |
| `PRIVNET_USER{1,2,3}_ADDRESS`、`PRIVNET_USER{1,2,3}_KEY` | 已充值的用户账户 |

用户交易请从测试账户发出，不要用验证人私钥。验证人签的每笔系统交易也会推进它的 nonce，
用它签名的测试会和自己的节点抢 nonce。

### 仅限开发的账户

| 角色 | 地址 | 私钥 |
|---|---|---|
| validator1（go1、rust1） | `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A` | `0x1111…1111`，BLS `0x0707…0707` |
| validator2（`multi` 中的 rust2） | `0x1563915e194D8CfBA1943570603F7606A3115508` | `0x2222…2222`，BLS `0x0808…0808` |
| validator3（`multi` 中的 rust3、`canary` 中的 go1） | `0x5CbDd86a2FA8Dc4bDdd8a8f69dBa48572EeC07FB` | `0x3333…3333`，BLS `0x0909…0909` |
| builder | `0x7564105E977516C53bE337314c7E53838967bDaC` | `0x4444…4444` |
| user1 | `0xe1fAE9b4fAB2F5726677ECfA912d96b0B683e6a9` | `0x5555…5555` |
| user2 | `0xdb2430B4e9AC14be6554d3942822BE74811A1AF9` | `0x6666…6666` |
| user3 | `0xAe72A48c1a36bd18Af168541c53037965d26e4A8` | `0x7777…7777` |

每个私钥都是表中字节重复 32 次。在所有创世配置里，每个验证人持有 1e9 BNB，builder 与用户各持有 1e7 BNB。

## 进程管理

- 每个节点以自己的数据目录为工作目录运行：投票日志在 `<datadir>/voteJournal`
  （`BSC_VOTE_JOURNAL_DIR`），日志在 `node.out`、`node.log` 和 `logs/`。不会有任何文件
  写到调用者的当前目录下。旧版本会让 reth-bsc 在脚本运行时所在的目录下创建 `voteJournal/`；
  残留的旧日志会拒绝为其中列出的每个高度投票，现在 `clean` 会删除遗留的 `privnet/voteJournal`。
- 脚本为每个节点记录 `pid` 和二进制的规范路径（`pid.exe`）。发送任何信号前，
  都会确认 `/proc/<pid>` 运行的正是该二进制，且带有 `--datadir <该节点目录>`；
  从不按模式匹配进程列表。`stop` 先发 SIGTERM，最多等待 `STOP_TIMEOUT`（60 s）让进程退出，
  SIGKILL 只作最后手段。`clean` 拒绝删除仍被任何进程使用的数据目录。
- handover 会先停掉 rust1 并等待它真正退出（释放数据库锁），再在同一数据目录上重启，
  并确认链继续前进。

## 运行注意事项（均为实测结论）

- **Go 跟随节点必须 `--syncmode full`。** 默认 snap 模式在极小私链上失败（`missing trie node ... layer stale`）。
- **Rust 节点必须 `-d`（关闭发现）。** 自定义链没有内置 bootnode，上游 reth 会回退到以太坊主网 bootnode，
  导致节点拨号公网并被 ban。修复版二进制已让自定义链默认空 bootnode。
- **Rust 出块必须在启动时开启**（`--mining.enabled --mining.private-key ...`）。`miner_start` RPC 虽然返回成功，
  但不会启动密封循环；Go 的 `miner_stop`/`miner_start` 运行时有效，`handover-to-go` 依赖这一点。
- **Go 的 `--nodiscover` 下 `--bootnodes` 不会被拨号。** Go 节点互联用 `admin_addPeer`。
- **呼吸块会让链停摆，所以脚本把它关掉。** 见下文[「每日呼吸块」问题](#每日呼吸块问题)。
  `BREATHE_BLOCK_INTERVAL`（默认 `1e15`）在每个节点上关闭该规则。
- **epoch 边界与终局性都正常。** 创世部署了系统合约，链能跨过每个 epoch（450 ms 下测到 1248 块）。
  旧创世有两个问题：不带合约的会停在 199 块；在 `BSCValidatorSet` 里登记了上游 BLS 私钥的，
  会在 200 块失去快速终局（`cannot collect enough votes`，`finalized` 停在 0）。见 [GENESIS.md](./GENESIS.md)。
- **不要用同一验证人私钥同时跑两个出块节点。** 两端都按时出块，tip 会在两分叉间抖动
  （最终会收敛，但日志全是重组噪音）。需要单一生产者请用 handover 动作，需要多个不同验证人请用 `multi`。

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

用 `just privnet-verify-breathe` 可复跑该验证（约 8 分钟）：它会把混合网络启动两次，
一次用 120s 间隔（必须停滞），一次用当前默认值（必须持续出块），最后检查 4 个
节点的 hash 与 `stateRoot` 是否仍然一致。`--case-b` 只跑后半段。节点不再应答
一律算失败，绝不会被当成停滞。脚本在自己的临时目录里、以 `PORT_OFFSET` 2000 运行
（可用 `AB_PRIVNET_DIR` / `AB_PORT_OFFSET` 覆盖），因此绝不会碰到已从 `./data`
运行的网络；无论脚本以何种方式退出，trap 都会拆掉测试网络。它需要 go-bsc，
缺少时以 77（跳过）退出。
