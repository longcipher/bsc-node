# BSC Node Rust 工作区 / BSC Node Rust Workspace

## 简介 / Overview

本仓库是围绕 BNB Chain 的 Rust 工作区（workspace），用于对比、验证与实验 BSC 的两套客户端实现：官方 Go 实现 `bsc`（geth-bsc）与官方 Rust 实现 `reth-bsc`。工作区遵循严格的 Rust 编码规范（edition 2024、`eyre`、`tracing`、强制 clippy lint），并内置一致性探针 `bsc-consistency-probe`，用于在同高度交叉校验两个客户端返回的区块头（尤其是 `stateRoot`），以及在 `truth` 模式下用 `eth_getProof` 的 Merkle 证明验证账户 / 存储叶子确实承诺于所声明的 `stateRoot`。

This repository is a Rust workspace around BNB Chain, used to compare, verify, and experiment with BSC's two client implementations: the official Go implementation `bsc` (geth-bsc) and the official Rust implementation `reth-bsc`.

The workspace follows strict Rust conventions (edition 2024, `eyre`, `tracing`, mandatory clippy lints) and ships a consistency probe, `bsc-consistency-probe`, which cross-checks block headers (notably the `stateRoot`) returned by both clients at the same height, and—in `truth` mode—verifies that account/storage leaves from `eth_getProof` are genuinely committed under the reported `stateRoot` via Merkle proof.

## 工作区结构 / Workspace Layout

工作区采用 Cargo workspace 布局：可执行程序放在 `bin/*`，可复用库放在 `crates/*`，根 `Cargo.toml` 通过 `members = ["bin/*", "crates/*"]` 聚合并统一管理共享依赖（根 `[workspace.dependencies]`）。

The workspace uses a Cargo workspace layout: executable crates live under `bin/*`, reusable libraries under `crates/*`, and the root `Cargo.toml` aggregates them via `members = ["bin/*", "crates/*"]` while centralizing shared dependencies in the root `[workspace.dependencies]`.

## 成员 / Crates

- `bin/cli-app` —— 示例 CLI（`clap`），演示工作区约定。/ Example CLI (`clap`) demonstrating workspace conventions.
- `crates/common` —— 跨 crate 共享工具库。/ Shared utility library used across crates.
- `bin/bsc-consistency-probe` —— Go vs Rust 一致性探针（详见下节）。/ Go vs Rust consistency probe (see next section).

## 一致性探针 / Consistency Probe

### 背景 / Background

中文：Go `bsc` 是 BSC 主网 21 个验证者实际运行的权威客户端；Rust `reth-bsc` 目前定位为实验性、可同步与提供只读 RPC，但尚不能作为出块验证者，且缺少部分最终性相关 API（如 `eth_getFinalizedHeader`），并把 `safe`/`finalized` 标签当作 `latest` 处理。因此，任何把 Rust 节点当作状态源的关键业务（例如 MEV 搜索 / DEX 聚合器用 revm 内联模拟），在信任其数据前都应先与 Go 交叉校验 `stateRoot`。

English: Go `bsc` is the authoritative client actually run by BSC mainnet's 21 validators; Rust `reth-bsc` is currently experimental—it can sync and serve read RPC, but cannot act as a block-producing validator, lacks some finality APIs (e.g. `eth_getFinalizedHeader`), and treats the `safe`/`finalized` tags as `latest`.

Therefore, any critical workload that trusts a Rust node as a state source (e.g. a MEV searcher / DEX aggregator running in-process revm simulation) should cross-validate the `stateRoot` against Go before trusting its data.

### 子命令 / Subcommands

- `compare` —— 在高度区间 `[start, end]` 内，对 Go 与 Rust 同高度区块头逐字段比较（含 `stateRoot` / `receiptsRoot` / `transactionsRoot` 等），输出逐区块差异与汇总。/ In the height range `[start, end]`, compares Go vs Rust block headers field-by-field at the same height (including `stateRoot` / `receiptsRoot` / `transactionsRoot`), printing per-block diffs and a summary.
- `truth` —— 用 `eth_getProof` 拉取账户与存储证明，按 MPT 规则验证叶子确实承诺于所声明的 `stateRoot`。可只给 `--go`（以 Go 为可信源），也可同时给 `--go` 与 `--rust` 做三方可信交叉校验。/ Pulls account/storage proofs via `eth_getProof` and verifies, per MPT rules, that the leaves are genuinely committed under the reported `stateRoot`. Pass only `--go` (Go as the trusted source) or both `--go` and `--rust` for a three-way trusted cross-check.

### 使用示例 / Examples

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

## 开发工作流 / Development Workflow

中文：本工作区使用 `just` 管理常用任务。

English: This workspace uses `just` for common tasks.

```bash
just setup     # 安装 Rust 工具链与 git hooks / install toolchain and git hooks
just check     # 类型检查 / type-check
just test      # 运行测试（含 proptest 属性测试）/ run tests (incl. proptest)
just lint      # 格式化 + 排序 + clippy -D warnings + typos + rumdl / fmt + sort + clippy + typos + rumdl
just build     # 构建整个工作区 / build the workspace
just mutation  # cargo-mutants 变异测试 / mutation testing
```

## 代码规范 / Conventions

中文：工作区启用 clippy `pedantic`（warn）并 deny 一系列易 panic 的写法：`unwrap` / `expect` / `panic`、`todo`、`dbg!`、库代码中的 `println!`/`eprintln!`、`#[allow(...)]` 属性等；`unused_must_use` 为 deny。库代码应使用 `tracing` 而非 `println`。CLI 程序可在 `main.rs` 顶部用 `#![expect(clippy::print_stdout)]` 放行 CLI 输出。HTTP 客户端统一使用 `hpx`（rustls-tls），禁止使用 `reqwest`。

English: The workspace enables clippy `pedantic` (warn) and denies several panic-prone patterns: `unwrap` / `expect` / `panic`, `todo`, `dbg!`, `println!`/`eprintln!` in library code, and `#[allow(...)]` attributes; `unused_must_use` is denied. Library code must use `tracing` instead of `println`. A CLI binary may lift the print lint via `#![expect(clippy::print_stdout)]` at the top of `main.rs`. The HTTP client is uniformly `hpx` (rustls-tls); `reqwest` is forbidden.

## 文档 / Documentation

中文：BSC 双客户端（Go vs Rust）深度对比分析见 `docs/bsc-diff.md`（中文 + 英文）。

English: In-depth BSC dual-client (Go vs Rust) analysis: `docs/bsc-diff.md` (Chinese + English).

## 许可证 / License

Apache-2.0
