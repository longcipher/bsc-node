# BSC Node Rust 工作区

## 简介

本仓库是围绕 BNB Chain 的 Rust 工作区（workspace），用于对比、验证与实验 BSC 的两套客户端实现：官方 Go 实现 `bsc`（geth-bsc）与官方 Rust 实现 `reth-bsc`。工作区遵循严格的 Rust 编码规范（edition 2024、`eyre`、`tracing`、强制 clippy lint），并内置一致性探针 `bsc-consistency-probe`，用于在同高度交叉校验两个客户端返回的区块头（尤其是 `stateRoot`），以及在 `truth` 模式下用 `eth_getProof` 的 Merkle 证明验证账户 / 存储叶子确实承诺于所声明的 `stateRoot`。

English: [README.md](./README.md)

## 工作区结构

工作区采用 Cargo workspace 布局：可执行程序放在 `bin/*`，根 `Cargo.toml` 通过 `members = ["bin/*"]` 聚合并统一管理共享依赖（根 `[workspace.dependencies]`）。可复用库应放在 `crates/*`，`members` 已为其预留位置。

## 成员

- `bin/bsc-consistency-probe` —— Go vs Rust 一致性探针（详见下节）。

## 一致性探针

### 背景

Go `bsc` 是 BSC 主网 45 个活跃验证者实际运行的权威客户端；Rust `reth-bsc` 目前定位为实验性、可同步与提供只读 RPC、私有网上可出块（已验证，见下），但主网验证未经证明，且缺少部分最终性相关 API（如 `eth_getFinalizedHeader`），并把 `safe`/`finalized` 标签当作 `latest` 处理。因此，任何把 Rust 节点当作状态源的关键业务（例如 MEV 搜索 / DEX 聚合器用 revm 内联模拟），在信任其数据前都应先与 Go 交叉校验 `stateRoot`。

### 子命令

- `compare` —— 在高度区间 `[start, end]` 内，对 Go 与 Rust 同高度区块头逐字段比较（含 `stateRoot` / `receiptsRoot` / `transactionsRoot` 等），输出逐区块差异与汇总。
- `truth` —— 用 `eth_getProof` 拉取账户与存储证明，按 MPT 规则验证叶子确实承诺于所声明的 `stateRoot`。可只给 `--go`（以 Go 为可信源），也可同时给 `--go` 与 `--rust` 做三方可信交叉校验。

### 使用示例

```bash
# 交叉校验高度 38_000_000..=38_000_100 的区块头（Go vs Rust）
cargo run -p bsc-consistency-probe -- compare \
  --go   https://bsc-mainnet.bnbchain.org \
  --rust https://your-reth-bsc.example.com \
  --start 38000000 --end 38000100 --workers 8

# 以 Go 为可信源，验证某账户及其 storage slot 的 Merkle 证明
cargo run -p bsc-consistency-probe -- truth \
  --go https://bsc-mainnet.bnbchain.org \
  --address 0xbb4C0b1500D34CC1b8d8e5f0b1f26a0aE0c1E2d3 \
  --slots 0x0,0x1 --block 38000100

# 三方可信校验：Go 与 Rust 都作为证明源，交叉验证
cargo run -p bsc-consistency-probe -- truth \
  --go   https://bsc-mainnet.bnbchain.org \
  --rust https://your-reth-bsc.example.com \
  --address 0xbb4C0b1500D34CC1b8d8e5f0b1f26a0aE0c1E2d3 \
  --slots 0x0 --block 38000100
```

## 开发工作流

本工作区使用 `just` 管理常用任务。

```bash
just setup     # 安装 Rust 工具链与 git hooks
just check     # 类型检查
just test      # 运行测试（含 proptest 属性测试）
just lint      # 格式化 + 排序 + clippy -D warnings + typos + rumdl
just build     # 构建整个工作区
just mutation  # cargo-mutants 变异测试
```

## 代码规范

工作区启用 clippy `pedantic`（warn）并 deny 一系列易 panic 的写法：`unwrap` / `expect` / `panic`、`todo`、`dbg!`、库代码中的 `println!`/`eprintln!`、`#[allow(...)]` 属性等；`unused_must_use` 为 deny。库代码应使用 `tracing` 而非 `println`。CLI 程序可在 `main.rs` 顶部用 `#![expect(clippy::print_stdout)]` 放行 CLI 输出。HTTP 客户端统一使用 `hpx`（rustls-tls），禁止使用 `reqwest`。

## 文档

BSC 双客户端（Go vs Rust）深度对比分析：

- English: [docs/bsc-diff.md](./docs/bsc-diff.md)
- 中文: [docs/bsc-diff.zh.md](./docs/bsc-diff.zh.md)

私有网实验（纯 Go / 纯 Rust / 混合）与一键搭建：

- English: [docs/privnet.md](./docs/privnet.md)、[privnet/README.md](./privnet/README.md)
- 中文: [docs/privnet.zh.md](./docs/privnet.zh.md)、[privnet/](./privnet/README.zh.md)

## 许可证

Apache-2.0
