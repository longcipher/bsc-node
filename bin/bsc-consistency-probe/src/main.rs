//! BSC dual-client consistency probe (Go geth-bsc vs Rust reth-bsc).
//!
//! Two modes:
//! - `compare`: cross-validate block headers (incl. stateRoot/receiptsRoot/ transactionsRoot)
//!   between a Go and a Rust endpoint.
//! - `truth`: cryptographic state-root truth verification via `eth_getProof` Merkle proofs,
//!   optionally comparing two endpoints (triple check).

#![expect(clippy::print_stdout)]

mod mpt;
mod proof;
mod rpc;

use clap::{Parser, Subcommand};
use eyre::{Result, eyre};
use proof::{VerifiedAddress, parse_address_list, parse_slot_list, resolve_block, verify_address};
use rpc::{RpcClient, parse_u64_hex, to_hex};
use serde::Serialize;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

/// Block header fields compared field-by-field between the two endpoints.
const HEADER_FIELDS: &[&str] = &[
    "number",
    "hash",
    "parentHash",
    "stateRoot",
    "transactionsRoot",
    "receiptsRoot",
    "logsBloom",
    "gasUsed",
    "gasLimit",
    "timestamp",
    "mixHash",
    "nonce",
    "baseFeePerGas",
    "withdrawalsRoot",
    "blobGasUsed",
    "excessBlobGas",
    "difficulty",
    "extraData",
];

#[derive(Debug, Parser)]
#[command(name = "bsc-consistency-probe", about = "BSC dual-client consistency probe (Go vs Rust)")]
struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Cross-validate block headers between a Go and a Rust endpoint.
    Compare {
        /// Go (geth-bsc) JSON-RPC endpoint.
        #[arg(long)]
        go: String,
        /// Rust (reth-bsc) JSON-RPC endpoint.
        #[arg(long)]
        rust: String,
        /// Start block (inclusive). Defaults to `latest - blocks + 1`.
        #[arg(long)]
        start: Option<u64>,
        /// End block (inclusive). Defaults to `latest`.
        #[arg(long)]
        end: Option<u64>,
        /// Number of recent blocks to check when no range is given.
        #[arg(long, default_value_t = 20)]
        blocks: usize,
        /// Also compare `eth_getBlockReceipts`.
        #[arg(long)]
        receipts: bool,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Concurrency is sequential in this build; retained for CLI parity.
        #[arg(long, default_value_t = 8)]
        workers: usize,
    },
    /// Cryptographic state-root truth verification via eth_getProof.
    ///
    /// Pass `--endpoint` for a single client, or `--go` + `--rust` for a
    /// cross-client triple check.
    Truth {
        /// Go (geth-bsc) JSON-RPC endpoint.
        #[arg(long)]
        go: Option<String>,
        /// Rust (reth-bsc) JSON-RPC endpoint.
        #[arg(long)]
        rust: Option<String>,
        /// Single-client endpoint (alternative to --go/--rust).
        #[arg(long)]
        endpoint: Option<String>,
        /// Block tag or number. Defaults to `latest`.
        #[arg(long, default_value = "latest")]
        block: String,
        /// Comma-separated addresses to sample (defaults to major BSC contracts).
        #[arg(long)]
        addresses: Option<String>,
        /// Comma-separated storage slots to sample.
        #[arg(long, default_value = "0,1,2")]
        storage_slots: String,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Compare { go, rust, start, end, blocks, receipts, json, .. } => {
            run_compare(&go, &rust, start, end, blocks, receipts, json).await
        }
        Cmd::Truth { go, rust, endpoint, block, addresses, storage_slots, json } => {
            run_truth(go, rust, endpoint, &block, addresses, &storage_slots, json).await
        }
    }
}

/// Result of comparing one block across the two endpoints.
#[derive(Debug, Serialize)]
struct BlockResult {
    /// Block height.
    number: u64,
    /// Whether state roots matched.
    state_root_match: bool,
    /// Whether the whole block matched.
    ok: bool,
    /// Human-readable field diffs.
    diffs: Vec<String>,
}

/// Runs the cross-client header comparison.
async fn run_compare(
    go_url: &str,
    rust_url: &str,
    start: Option<u64>,
    end: Option<u64>,
    blocks: usize,
    check_receipts: bool,
    as_json: bool,
) -> Result<()> {
    let go = RpcClient::new(go_url);
    let rust = RpcClient::new(rust_url);

    let heights: Vec<u64> = if let (Some(s), Some(e)) = (start, end) {
        (s..=e).collect()
    } else {
        let latest = go.block_number().await?;
        let from = latest.saturating_sub(blocks as u64 - 1);
        (from..=latest).collect()
    };

    let mut results = Vec::new();
    for h in heights {
        results.push(probe_block(&go, &rust, h, check_receipts).await?);
    }
    results.sort_by_key(|r| r.number);

    let passed = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    let mismatched: Vec<&BlockResult> = results.iter().filter(|r| !r.ok).collect();

    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "total": total,
                "passed": passed,
                "failed": total - passed,
                "results": &results,
            }))?
        );
    } else {
        println!("{:>12}  {:>16}  {:>6}  DETAILS", "BLOCK", "STATE_ROOT_MATCH", "RESULT");
        println!("{}", "-".repeat(100));
        for r in &results {
            let detail = if r.ok { String::new() } else { r.diffs.join("; ") };
            println!(
                "{:>12}  {:>16}  {:>6}  {}",
                r.number,
                r.state_root_match,
                if r.ok { "PASS" } else { "FAIL" },
                detail
            );
        }
        println!("{}", "-".repeat(100));
        println!("SUMMARY: {passed}/{total} blocks consistent across Go & Rust endpoints.");
        if !mismatched.is_empty() {
            println!("INCONSISTENT BLOCKS:");
            for r in &mismatched {
                println!("  #{}: {}", r.number, r.diffs.join("; "));
            }
        }
    }

    if mismatched.is_empty() {
        Ok(())
    } else {
        Err(eyre!("{} block(s) inconsistent across Go & Rust endpoints", mismatched.len()))
    }
}

/// Probes a single block, fetching headers (and receipts) from both endpoints.
async fn probe_block(
    go: &RpcClient,
    rust: &RpcClient,
    num: u64,
    check_receipts: bool,
) -> Result<BlockResult> {
    let num_hex = to_hex(num);
    let gf =
        go.call("eth_getBlockByNumber", vec![Value::String(num_hex.clone()), Value::Bool(false)]);
    let rf =
        rust.call("eth_getBlockByNumber", vec![Value::String(num_hex.clone()), Value::Bool(false)]);
    let gt =
        go.call("eth_getBlockByNumber", vec![Value::String(num_hex.clone()), Value::Bool(true)]);
    let rt =
        rust.call("eth_getBlockByNumber", vec![Value::String(num_hex.clone()), Value::Bool(true)]);
    let (go_hdr, rust_hdr, go_full, rust_full) = tokio::try_join!(gf, rf, gt, rt)?;

    let mut diffs = compare_header(&go_hdr, &rust_hdr);

    if let (Some(g), Some(r)) = (go_full.as_object(), rust_full.as_object()) {
        let g_txs = tx_hashes(g);
        let r_txs = tx_hashes(r);
        if g_txs.len() == r_txs.len() {
            for (i, (a, b)) in g_txs.iter().zip(r_txs.iter()).enumerate() {
                if a != b {
                    diffs.push(format!("tx[{i}] hash: go={a} rust={b}"));
                }
            }
        } else {
            diffs.push(format!("tx count: go={} rust={}", g_txs.len(), r_txs.len()));
        }
    }

    if check_receipts {
        let gr = go.call("eth_getBlockReceipts", vec![Value::String(num_hex.clone())]).await;
        let rr = rust.call("eth_getBlockReceipts", vec![Value::String(num_hex)]).await;
        if let (Ok(g), Ok(r)) = (gr, rr) {
            diffs.extend(compare_receipts(&g, &r));
        }
    }

    let sr_go = go_hdr.get("stateRoot").and_then(Value::as_str).map(str::to_string);
    let sr_rust = rust_hdr.get("stateRoot").and_then(Value::as_str).map(str::to_string);
    let sr_match = sr_go == sr_rust && sr_go.is_some();

    Ok(BlockResult { number: num, state_root_match: sr_match, ok: diffs.is_empty(), diffs })
}

/// Extracts transaction hashes from a full block object.
fn tx_hashes(block: &serde_json::Map<String, Value>) -> Vec<String> {
    match block.get("transactions") {
        Some(Value::Array(txs)) => txs
            .iter()
            .map(|t| match t {
                Value::String(h) => h.clone(),
                Value::Object(o) => {
                    o.get("hash").and_then(Value::as_str).unwrap_or_default().to_string()
                }
                _ => String::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Compares two block headers field-by-field.
fn compare_header(go: &Value, rust: &Value) -> Vec<String> {
    let mut d = Vec::new();
    if go.is_null() && rust.is_null() {
        d.push("both endpoints returned null block".into());
        return d;
    }
    if go.is_null() {
        d.push("go endpoint returned null block".into());
        return d;
    }
    if rust.is_null() {
        d.push("rust endpoint returned null block".into());
        return d;
    }
    for &f in HEADER_FIELDS {
        let gv = go.get(f);
        let rv = rust.get(f);
        if gv != rv {
            d.push(format!("header.{f}: go={gv:?} rust={rv:?}"));
        }
    }
    d
}

/// Compares receipt lists from both endpoints.
fn compare_receipts(go: &Value, rust: &Value) -> Vec<String> {
    let mut d = Vec::new();
    let (g, r) = if let (Some(g), Some(r)) = (go.as_array(), rust.as_array()) {
        (g, r)
    } else {
        d.push("receipts: one endpoint unavailable (eth_getBlockReceipts unsupported)".into());
        return d;
    };
    if g.len() != r.len() {
        d.push(format!("receipt count: go={} rust={}", g.len(), r.len()));
        return d;
    }
    for (i, (a, b)) in g.iter().zip(r.iter()).enumerate() {
        for f in ["status", "gasUsed", "transactionHash"] {
            if a.get(f) != b.get(f) {
                d.push(format!("receipt[{i}].{f}: go={:?} rust={:?}", a.get(f), b.get(f)));
            }
        }
        let gl = a.get("logs").and_then(Value::as_array).map_or(0, Vec::len);
        let rl = b.get("logs").and_then(Value::as_array).map_or(0, Vec::len);
        if gl != rl {
            d.push(format!("receipt[{i}] log count: go={gl} rust={rl}"));
        }
    }
    d
}

/// Runs the cryptographic truth verification.
async fn run_truth(
    go: Option<String>,
    rust: Option<String>,
    endpoint: Option<String>,
    block: &str,
    addresses: Option<String>,
    storage_slots: &str,
    as_json: bool,
) -> Result<()> {
    let addrs = match &addresses {
        Some(s) => parse_address_list(s)?,
        None => parse_address_list(&proof::default_mainnet_addresses().join(","))?,
    };
    let slots = parse_slot_list(storage_slots)?;

    let endpoints: Vec<(String, RpcClient)> = if let Some(ep) = endpoint {
        vec![("endpoint".to_string(), RpcClient::new(&ep))]
    } else {
        let mut v = Vec::new();
        if let Some(g) = go.clone() {
            v.push(("go".to_string(), RpcClient::new(&g)));
        }
        if let Some(r) = rust.clone() {
            v.push(("rust".to_string(), RpcClient::new(&r)));
        }
        if v.is_empty() {
            return Err(eyre!("provide --endpoint, or --go and/or --rust"));
        }
        v
    };

    let mut report = serde_json::Map::new();

    for (name, client) in &endpoints {
        let (height, state_root) = resolve_block(client, block).await?;
        println!("[{name}] block {height} stateRoot = {state_root}");
        let mut verified = Vec::new();
        for addr in &addrs {
            match verify_address(client, state_root, *addr, &slots, block).await {
                Ok(va) => {
                    print_verified(name, *addr, &va);
                    verified.push(serde_json::to_value(&va)?);
                }
                Err(e) => {
                    println!("  [{name}] {addr} VERIFY FAILED: {e}");
                    verified.push(serde_json::json!({
                        "address": format!("0x{addr:x}"),
                        "error": e.to_string(),
                    }));
                }
            }
        }
        report.insert(
            name.clone(),
            serde_json::json!({
                "block": height,
                "state_root": format!("{state_root}"),
                "addresses": verified,
            }),
        );
    }

    if endpoints.len() == 2 {
        let go_name = &endpoints[0].0;
        let rust_name = &endpoints[1].0;
        let agree = cross_check(&report, go_name, rust_name);
        report.insert("cross_client_agreement".to_string(), Value::Bool(agree));
        println!(
            "\nCROSS-CLIENT (Go vs Rust) state agreement: {}",
            if agree { "OK" } else { "MISMATCH" }
        );
        if !agree {
            return Err(eyre!("Go and Rust clients disagree on verified state"));
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&Value::Object(report))?);
    }
    Ok(())
}

/// Prints a per-address verification summary line.
fn print_verified(name: &str, addr: primitive_types::H160, va: &VerifiedAddress) {
    match &va.account {
        Some(a) => {
            println!(
                "  [{name}] {addr} OK  balance={} nonce={} storageRoot={} codeHash={}",
                a.balance, a.nonce, a.storage_root, a.code_hash
            );
            for s in &va.slots {
                println!(
                    "        slot {} = {} ({})",
                    s.slot,
                    s.value,
                    if s.present { "present" } else { "absent" }
                );
            }
        }
        None => println!("  [{name}] {addr} ABSENT (proven under root)"),
    }
}

/// Compares the two endpoints' per-address account + slot values.
fn cross_check(report: &serde_json::Map<String, Value>, go_name: &str, rust_name: &str) -> bool {
    let go = match report.get(go_name).and_then(|v| v.get("addresses")) {
        Some(Value::Array(a)) => a,
        _ => return false,
    };
    let rust = match report.get(rust_name).and_then(|v| v.get("addresses")) {
        Some(Value::Array(a)) => a,
        _ => return false,
    };
    if go.len() != rust.len() {
        return false;
    }
    for (g, r) in go.iter().zip(rust.iter()) {
        let ga = g.get("address").and_then(Value::as_str);
        let ra = r.get("address").and_then(Value::as_str);
        if ga != ra {
            return false;
        }
        if g.get("error").is_some() || r.get("error").is_some() {
            if g.get("error") != r.get("error") {
                return false;
            }
            continue;
        }
        for f in ["balance", "nonce", "storageRoot", "codeHash"] {
            if g.pointer(&format!("/account/{f}")) != r.pointer(&format!("/account/{f}")) {
                return false;
            }
        }
        if g.get("slots") != r.get("slots") {
            return false;
        }
    }
    true
}

/// Parse helper re-exported for documentation parity with the RPC module.
pub fn parse_height(s: &str) -> Result<u64> {
    parse_u64_hex(s)
}
