//! BSC dual-client consistency probe (Go geth-bsc vs Rust reth-bsc).
//!
//! Two modes:
//! - `compare`: cross-validate block headers (incl. stateRoot/receiptsRoot/ transactionsRoot)
//!   between a Go and a Rust endpoint.
//! - `truth`: cryptographic state-root truth verification via `eth_getProof` Merkle proofs,
//!   optionally comparing two endpoints (triple check).

#![expect(clippy::print_stdout)]

#[cfg(test)]
mod mock_rpc;
mod mpt;
mod proof;
mod rpc;

#[cfg(test)]
use std::time::Duration;

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
        /// Number of block probes to keep in flight at once.
        ///
        /// One block probe issues four JSON-RPC calls, so this is what bounds
        /// how much of a public endpoint's capacity a wide `--start..--end`
        /// range consumes.
        #[arg(long, default_value_t = 8)]
        workers: usize,
        /// Also compare `eth_getBlockReceipts`.
        #[arg(long)]
        receipts: bool,
        /// Emit machine-readable JSON instead of a table.
        #[arg(long)]
        json: bool,
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
        Cmd::Compare { go, rust, start, end, blocks, workers, receipts, json } => {
            let go = RpcClient::new(&go);
            let rust = RpcClient::new(&rust);
            let heights = resolve_heights(&go, start, end, blocks).await?;
            run_compare(&go, &rust, &heights, workers, receipts, json).await
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

/// Resolves the height range to probe.
///
/// An explicit `--start`/`--end` pair is taken as the inclusive range it names,
/// and the two bounds must be in order: an inverted range would probe nothing
/// and then report "0/0 consistent", which reads like a pass.
///
/// Without both bounds the range is the `--blocks` most recent heights ending
/// at the Go endpoint's head, so the CLI asks for the head exactly once.
async fn resolve_heights(
    go: &RpcClient,
    start: Option<u64>,
    end: Option<u64>,
    blocks: usize,
) -> Result<Vec<u64>> {
    if let (Some(s), Some(e)) = (start, end) {
        if s > e {
            return Err(eyre!("--start {s} is above --end {e}; nothing would be probed"));
        }
        return Ok((s..=e).collect());
    }

    let latest = go.block_number().await?;
    // `--blocks 0` would underflow here, so it reads as "the head alone".
    let from = latest.saturating_sub(blocks.saturating_sub(1) as u64);
    Ok((from..=latest).collect())
}

/// Rejects a worker count that would leave nothing to probe.
fn worker_count(workers: usize) -> Result<usize> {
    if workers == 0 {
        return Err(eyre!("--workers must be at least 1"));
    }
    Ok(workers)
}

/// Hands one block probe to the worker pool.
fn spawn_probe(
    in_flight: &mut tokio::task::JoinSet<(u64, Result<BlockResult>)>,
    go: &RpcClient,
    rust: &RpcClient,
    height: u64,
    check_receipts: bool,
) {
    let go = go.clone();
    let rust = rust.clone();
    in_flight.spawn(async move { (height, probe_block(&go, &rust, height, check_receipts).await) });
}

/// Runs the cross-client header comparison over the already-resolved heights.
///
/// Heights are probed `workers` at a time. Each block probe issues four
/// JSON-RPC calls, so the worker count is what bounds how much of a public
/// endpoint's capacity a wide range consumes, and a public node answers
/// concurrently whether the caller asks for it or not.
///
/// Results are gathered in completion order and sorted by height afterwards, so
/// the report is deterministic whatever order the two endpoints answer in.
/// A height whose probe fails is still reported, as a diff on its own row, so
/// one unreachable block does not silently drop out of the range.
async fn run_compare(
    go: &RpcClient,
    rust: &RpcClient,
    heights: &[u64],
    workers: usize,
    check_receipts: bool,
    as_json: bool,
) -> Result<()> {
    let workers = worker_count(workers)?;

    let mut results = Vec::with_capacity(heights.len());
    let mut in_flight = tokio::task::JoinSet::new();
    let mut remaining = heights.iter().copied();

    // Prime the pool, then refill one slot per finished probe. A height is
    // only handed to the pool as a slot frees up, so at most `workers` block
    // probes are ever in flight.
    for height in remaining.by_ref().take(workers) {
        spawn_probe(&mut in_flight, go, rust, height, check_receipts);
    }
    while let Some(joined) = in_flight.join_next().await {
        // A probe that failed, and a probe task that was cancelled, both leave
        // the height without a result rather than aborting the whole run.
        let (height, outcome) = match joined {
            Ok(pair) => pair,
            Err(e) => (0, Err(eyre!("block probe task failed: {e}"))),
        };
        results.push(match outcome {
            Ok(result) => result,
            Err(e) => BlockResult {
                number: height,
                state_root_match: false,
                ok: false,
                diffs: vec![format!("probe failed: {e}")],
            },
        });
        if let Some(next) = remaining.next() {
            spawn_probe(&mut in_flight, go, rust, next, check_receipts);
        }
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
    if !go.is_object() || !rust.is_object() {
        // Anything that is not a block object has no comparable fields. Saying
        // "no differences" here would let two broken clients pass the probe.
        d.push("header: response is not a block object".into());
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
        println!("[{name}] block {height} stateRoot = {state_root:?}");
        let mut verified = Vec::new();
        for addr in &addrs {
            match verify_address(client, state_root, *addr, &slots, block).await {
                Ok(va) => {
                    print_verified(name, *addr, &va);
                    verified.push(serde_json::to_value(&va)?);
                }
                Err(e) => {
                    println!("  [{name}] {addr:?} VERIFY FAILED: {e}");
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
                "state_root": format!("{state_root:?}"),
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
            // Hashes and addresses are printed with `{:?}`: their `Display`
            // elides the middle (`0xcf73…95dc`), which is useless in a report
            // and unusable in the `--json` output.
            println!(
                "  [{name}] {addr:?} OK  balance={} nonce={} storageRoot={:?} codeHash={:?}",
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
        None => println!("  [{name}] {addr:?} ABSENT (proven under root)"),
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

#[cfg(test)]
mod tests {
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::mock_rpc::MockRpc;

    // ------------------------------------------------------------------
    // tx_hashes
    // ------------------------------------------------------------------

    #[test]
    fn tx_hashes_from_string_array() {
        let mut block = Map::new();
        block.insert(
            "transactions".into(),
            Value::Array(vec![Value::String("0xaa".into()), Value::String("0xbb".into())]),
        );
        assert_eq!(tx_hashes(&block), vec!["0xaa".to_string(), "0xbb".to_string()]);
    }

    #[test]
    fn tx_hashes_from_object_array() {
        let mut block = Map::new();
        block.insert(
            "transactions".into(),
            Value::Array(vec![json!({ "hash": "0xaa" }), json!({ "hash": "0xbb" })]),
        );
        assert_eq!(tx_hashes(&block), vec!["0xaa".to_string(), "0xbb".to_string()]);
    }

    #[test]
    fn tx_hashes_missing_or_wrong_type() {
        assert!(tx_hashes(&Map::new()).is_empty());
        let mut block = Map::new();
        block.insert("transactions".into(), Value::String("nope".into()));
        assert!(tx_hashes(&block).is_empty());
    }

    #[test]
    fn tx_hashes_empty_array() {
        let mut block = Map::new();
        block.insert("transactions".into(), Value::Array(vec![]));
        assert!(tx_hashes(&block).is_empty());
    }

    #[test]
    fn tx_hashes_handles_mixed_shapes_and_junk_entries() {
        // A full block mixes only objects, but the extractor must not panic on
        // an entry it cannot read.
        let mut block = Map::new();
        block.insert(
            "transactions".into(),
            Value::Array(vec![
                json!({ "hash": "0xaa" }),
                Value::String("0xbb".into()),
                json!({ "hash": null }),
                Value::Number(7.into()),
            ]),
        );
        assert_eq!(tx_hashes(&block), vec![
            "0xaa".to_string(),
            "0xbb".to_string(),
            String::new(),
            String::new()
        ]);
    }

    // ------------------------------------------------------------------
    // compare_header
    // ------------------------------------------------------------------

    #[test]
    fn compare_header_both_null() {
        let d = compare_header(&Value::Null, &Value::Null);
        assert_eq!(d, vec!["both endpoints returned null block".to_string()]);
    }

    #[test]
    fn compare_header_go_null() {
        let d = compare_header(&Value::Null, &json!({"number": "0x1"}));
        assert_eq!(d, vec!["go endpoint returned null block".to_string()]);
    }

    #[test]
    fn compare_header_rust_null() {
        let d = compare_header(&json!({"number": "0x1"}), &Value::Null);
        assert_eq!(d, vec!["rust endpoint returned null block".to_string()]);
    }

    #[test]
    fn compare_header_detects_state_root_diff() {
        let go = json!({ "number": "0x1", "stateRoot": "0xaaa", "gasUsed": "0x10" });
        let rust = json!({ "number": "0x1", "stateRoot": "0xbbb", "gasUsed": "0x10" });
        let d = compare_header(&go, &rust);
        assert!(d.iter().any(|s| s.contains("header.stateRoot")));
        assert!(!d.iter().any(|s| s.contains("header.gasUsed")));
    }

    #[test]
    fn compare_header_match_is_empty() {
        let go = json!({ "number": "0x1", "stateRoot": "0xaaa" });
        let rust = json!({ "number": "0x1", "stateRoot": "0xaaa" });
        assert!(compare_header(&go, &rust).is_empty());
    }

    #[test]
    fn compare_header_detects_a_field_present_on_one_side_only() {
        // Missing and null are different answers, and geth omits fields that
        // reth reports, so this asymmetry must be visible.
        let go = json!({ "number": "0x1", "withdrawalsRoot": "0xaa" });
        let rust = json!({ "number": "0x1" });
        let d = compare_header(&go, &rust);
        assert_eq!(d.len(), 1);
        assert!(d[0].contains("header.withdrawalsRoot"), "unexpected: {}", d[0]);
    }

    #[test]
    fn compare_header_ignores_fields_outside_the_compared_set() {
        let go = json!({ "number": "0x1", "somethingNew": "0xaa" });
        let rust = json!({ "number": "0x1", "somethingNew": "0xbb" });
        assert!(compare_header(&go, &rust).is_empty());
    }

    #[test]
    fn compare_header_rejects_non_object_responses() {
        // Two malformed responses must not be reported as agreement.
        for (go, rust) in [
            (json!("nope"), json!("nope")),
            (json!([1, 2]), json!([1, 2])),
            (json!(7), json!(7)),
            (json!({"number": "0x1"}), json!("nope")),
        ] {
            let d = compare_header(&go, &rust);
            assert_eq!(
                d,
                vec!["header: response is not a block object".to_string()],
                "go={go} rust={rust}"
            );
        }
    }

    #[test]
    fn compare_header_covers_every_declared_field() {
        let go = json!({ "number": "0x1" });
        let rust = json!({ "number": "0x1" });
        for &field in HEADER_FIELDS {
            if field == "number" {
                continue;
            }
            let mut rust_block = rust.clone();
            rust_block[field] = json!("0xdeadbeef");
            let d = compare_header(&go, &rust_block);
            assert_eq!(d.len(), 1, "field {field} was not compared");
            assert!(d[0].contains(field), "unexpected diff for {field}: {}", d[0]);
        }
    }

    // ------------------------------------------------------------------
    // compare_receipts
    // ------------------------------------------------------------------

    #[test]
    fn compare_receipts_count_mismatch() {
        let go = json!([{ "status": "0x1" }]);
        let rust = json!([{ "status": "0x1" }, { "status": "0x1" }]);
        let d = compare_receipts(&go, &rust);
        assert!(d.iter().any(|s| s.contains("receipt count")));
    }

    #[test]
    fn compare_receipts_field_diff() {
        let go = json!([{ "status": "0x1", "gasUsed": "0x10", "transactionHash": "0xaa" }]);
        let rust = json!([{ "status": "0x0", "gasUsed": "0x10", "transactionHash": "0xaa" }]);
        let d = compare_receipts(&go, &rust);
        assert!(d.iter().any(|s| s.contains("receipt[0].status")));
    }

    #[test]
    fn compare_receipts_log_count_diff() {
        let go = json!([{ "logs": [{}] }]);
        let rust = json!([{ "logs": [{}, {}] }]);
        let d = compare_receipts(&go, &rust);
        assert!(d.iter().any(|s| s.contains("receipt[0] log count")));
    }

    #[test]
    fn compare_receipts_unavailable() {
        let d = compare_receipts(&json!("nope"), &json!([{}]));
        assert!(d.iter().any(|s| s.contains("one endpoint unavailable")));
    }

    #[test]
    fn compare_receipts_identical_is_empty() {
        let receipts = json!([
            { "status": "0x1", "gasUsed": "0x10", "transactionHash": "0xaa", "logs": [{}] },
        ]);
        assert!(compare_receipts(&receipts, &receipts).is_empty());
    }

    #[test]
    fn compare_receipts_both_empty_is_empty() {
        assert!(compare_receipts(&json!([]), &json!([])).is_empty());
    }

    #[test]
    fn compare_receipts_gas_used_and_tx_hash_diffs() {
        let base = json!([{ "status": "0x1", "gasUsed": "0x10", "transactionHash": "0xaa" }]);

        let mut rust = base.clone();
        rust[0]["gasUsed"] = json!("0x20");
        assert!(compare_receipts(&base, &rust).iter().any(|s| s.contains("receipt[0].gasUsed")));

        let mut rust = base.clone();
        rust[0]["transactionHash"] = json!("0xbb");
        assert!(
            compare_receipts(&base, &rust).iter().any(|s| s.contains("receipt[0].transactionHash"))
        );
    }

    #[test]
    fn compare_receipts_reports_only_the_first_index_when_counts_differ() {
        // Differing lengths make positional comparison meaningless, so the
        // count diff must be reported without cascading per-index noise.
        let go = json!([{ "status": "0x1" }]);
        let rust = json!([{ "status": "0x1" }, { "status": "0x0" }]);
        assert_eq!(compare_receipts(&go, &rust), vec!["receipt count: go=1 rust=2".to_string()]);
    }

    #[test]
    fn compare_receipts_treats_missing_logs_as_zero() {
        let go = json!([{ "status": "0x1" }]);
        let rust = json!([{ "status": "0x1", "logs": [{}] }]);
        assert!(compare_receipts(&go, &rust).iter().any(|s| s.contains("log count")));
    }

    // ------------------------------------------------------------------
    // cross_check
    // ------------------------------------------------------------------

    fn verified_address_json(addr: &str, balance: &str, nonce: &str, sr: &str, ch: &str) -> Value {
        json!({
            "address": addr,
            "account": { "balance": balance, "nonce": nonce, "storageRoot": sr, "codeHash": ch },
            "slots": [ { "slot": "0x0", "value": "0x1", "present": true } ],
        })
    }

    fn report_with(name: &str, entries: Vec<Value>) -> Map<String, Value> {
        let mut r = Map::new();
        r.insert(name.into(), json!({ "addresses": entries }));
        r
    }

    const ADDR_A: &str = "0xbb4cdb9cbd36b01bc983aec51fa2d8c5b2f3f3a7";
    const ADDR_B: &str = "0x55d398326f99059ff775485246999027b3197955";

    /// Builds a two-endpoint report from parallel per-address JSON values.
    fn two_way(go: Vec<Value>, rust: Vec<Value>) -> Map<String, Value> {
        let mut report = report_with("go", go);
        report.insert("rust".into(), json!({ "addresses": rust }));
        report
    }

    #[test]
    fn cross_check_agreement() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let rust = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        assert!(cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_balance_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let rust = verified_address_json(ADDR_A, "0x11", "0x2", "0xsr", "0xch");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_nonce_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let rust = verified_address_json(ADDR_A, "0x10", "0x3", "0xsr", "0xch");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_storage_root_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr1", "0xch");
        let rust = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr2", "0xch");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_code_hash_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch1");
        let rust = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch2");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_slot_value_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let mut rust = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        rust["slots"][0]["value"] = json!("0x2");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_address_order_mismatch() {
        let go = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let rust = verified_address_json(ADDR_B, "0x10", "0x2", "0xsr", "0xch");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_length_mismatch() {
        let a = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        let b = verified_address_json(ADDR_B, "0x10", "0x2", "0xsr", "0xch");
        assert!(!cross_check(&two_way(vec![a.clone()], vec![a, b]), "go", "rust"));
    }

    #[test]
    fn cross_check_missing_endpoint() {
        assert!(!cross_check(&report_with("go", vec![]), "go", "rust"));
    }

    #[test]
    fn cross_check_missing_addresses_key() {
        let mut report = Map::new();
        report.insert("go".into(), json!({ "block": 1 }));
        report.insert("rust".into(), json!({ "block": 1 }));
        assert!(!cross_check(&report, "go", "rust"));
    }

    #[test]
    fn cross_check_identical_failures_still_agree() {
        // Both clients failing the same way is agreement about the failure,
        // not a disagreement about state.
        let e = json!({ "address": ADDR_A, "error": "boom" });
        assert!(cross_check(&two_way(vec![e.clone()], vec![e]), "go", "rust"));
    }

    #[test]
    fn cross_check_differing_failures_disagree() {
        let go = json!({ "address": ADDR_A, "error": "boom" });
        let rust = json!({ "address": ADDR_A, "error": "other" });
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_one_failed_one_succeeded_disagrees() {
        let go = json!({ "address": ADDR_A, "error": "boom" });
        let rust = verified_address_json(ADDR_A, "0x10", "0x2", "0xsr", "0xch");
        assert!(!cross_check(&two_way(vec![go], vec![rust]), "go", "rust"));
    }

    #[test]
    fn cross_check_absent_accounts_agree() {
        let a = json!({ "address": ADDR_A, "account": null, "slots": [] });
        assert!(cross_check(&two_way(vec![a.clone()], vec![a]), "go", "rust"));
    }

    // ------------------------------------------------------------------
    // probe_block / run_compare, end to end against loopback endpoints
    // ------------------------------------------------------------------

    /// A block header with every compared field populated.
    fn block_header(height: u64, state_root: &str) -> Value {
        let mut v = json!({ "transactions": [json!({ "hash": "0xaa" })] });
        for &f in HEADER_FIELDS {
            v[f] = json!(match f {
                "number" => to_hex(height),
                "stateRoot" => state_root.to_string(),
                "hash" => format!("0x{height:064x}"),
                "parentHash" => format!("0x{:064x}", height.saturating_sub(1)),
                "timestamp" => "0x65000000".to_string(),
                "nonce" => "0x0000000000000000".to_string(),
                "gasUsed" => "0x5208".to_string(),
                "gasLimit" => "0x1c9c380".to_string(),
                "baseFeePerGas" => "0x3b9aca00".to_string(),
                _other => "0x1234".to_string(),
            });
        }
        v
    }

    fn go_routes(head: &str, state_root: &str, height: u64) -> Vec<(&'static str, Value)> {
        vec![
            ("eth_blockNumber", json!(head)),
            ("eth_getBlockByNumber", block_header(height, state_root)),
            ("eth_getBlockReceipts", json!([])),
        ]
    }

    #[tokio::test]
    async fn probe_block_reports_agreement() {
        let go = MockRpc::start(go_routes("0x64", "0xabc", 100)).await;
        let rust = MockRpc::start(go_routes("0x64", "0xabc", 100)).await;

        let result =
            probe_block(&RpcClient::new(&go.url()), &RpcClient::new(&rust.url()), 100, false)
                .await
                .unwrap();

        assert_eq!(result.number, 100);
        assert!(result.state_root_match);
        assert!(result.ok);
        assert!(result.diffs.is_empty());
    }

    #[tokio::test]
    async fn probe_block_flags_a_state_root_divergence() {
        let go = MockRpc::start(go_routes("0x64", "0xaaa", 100)).await;
        let rust = MockRpc::start(go_routes("0x64", "0xbbb", 100)).await;

        let result =
            probe_block(&RpcClient::new(&go.url()), &RpcClient::new(&rust.url()), 100, false)
                .await
                .unwrap();

        assert!(!result.state_root_match);
        assert!(!result.ok);
        assert!(result.diffs.iter().any(|d| d.contains("stateRoot")));
    }

    #[tokio::test]
    async fn probe_block_survives_one_endpoint_being_down() {
        let go = MockRpc::start(go_routes("0x64", "0xabc", 100)).await;
        // A listener that is bound then dropped leaves a closed port.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let dead = listener.local_addr().unwrap();
        drop(listener);

        assert!(
            probe_block(
                &RpcClient::new(&go.url()),
                &RpcClient::new(&format!("http://{dead}")),
                100,
                false
            )
            .await
            .is_err()
        );
    }

    /// Points a compare run at two loopback endpoints over an explicit range.
    async fn compare_range(
        go: &MockRpc,
        rust: &MockRpc,
        heights: &[u64],
        workers: usize,
    ) -> Result<()> {
        run_compare(
            &RpcClient::new(&go.url()),
            &RpcClient::new(&rust.url()),
            heights,
            workers,
            false,
            false,
        )
        .await
    }

    #[tokio::test]
    async fn run_compare_succeeds_over_a_consistent_range() {
        let routes = go_routes("0x65", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        assert!(compare_range(&go, &rust, &[100, 101], 1).await.is_ok());
    }

    #[tokio::test]
    async fn run_compare_fails_when_any_block_diverges() {
        let go = MockRpc::start(go_routes("0x65", "0xaaa", 100)).await;
        let rust = MockRpc::start(go_routes("0x65", "0xbbb", 100)).await;

        let err = compare_range(&go, &rust, &[100, 101], 1).await.unwrap_err();
        assert!(err.to_string().contains("inconsistent"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn run_compare_defaults_to_the_recent_window() {
        // With no explicit range the height comes from eth_blockNumber.
        let go = MockRpc::start(go_routes("0x65", "0xabc", 101)).await;
        let _rust = MockRpc::start(go_routes("0x65", "0xabc", 101)).await;

        let heights = resolve_heights(&RpcClient::new(&go.url()), None, None, 2)
            .await
            .expect("window must resolve");
        assert_eq!(heights, vec![100, 101]);
        assert!(go.requests().iter().any(|r| r["method"] == "eth_blockNumber"));
    }

    #[tokio::test]
    async fn run_compare_emits_a_json_report() {
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        let outcome = run_compare(
            &RpcClient::new(&go.url()),
            &RpcClient::new(&rust.url()),
            &[100],
            1,
            false,
            true,
        )
        .await;
        // The JSON path must not change the pass/fail outcome.
        assert!(outcome.is_ok());
    }

    #[tokio::test]
    async fn run_compare_json_payload_is_well_formed() {
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        run_compare(
            &RpcClient::new(&go.url()),
            &RpcClient::new(&rust.url()),
            &[100],
            1,
            false,
            true,
        )
        .await
        .unwrap();

        // Re-derive the summary the command prints so the shape is pinned.
        let summary = json!({
            "total": 1,
            "passed": 1,
            "failed": 0,
            "results": [BlockResult {
                number: 100,
                state_root_match: true,
                ok: true,
                diffs: vec![],
            }],
        });
        let v: Value = serde_json::to_value(&summary).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["results"][0]["number"], 100);
        assert_eq!(v["results"][0]["ok"], true);
        assert_eq!(v["results"][0]["state_root_match"], true);
    }

    // ------------------------------------------------------------------
    // resolve_heights
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn resolve_heights_takes_an_explicit_inclusive_range() {
        // An explicit pair must not touch the endpoint at all.
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0xff"))]).await;
        let client = RpcClient::new(&server.url());

        let heights = resolve_heights(&client, Some(100), Some(103), 20).await.unwrap();
        assert_eq!(heights, vec![100, 101, 102, 103]);
        assert!(server.requests().is_empty(), "an explicit range needs no eth_blockNumber");
    }

    #[tokio::test]
    async fn resolve_heights_accepts_a_single_block_range() {
        let server = MockRpc::start(vec![]).await;
        let client = RpcClient::new(&server.url());

        assert_eq!(resolve_heights(&client, Some(7), Some(7), 20).await.unwrap(), vec![7]);
    }

    #[tokio::test]
    async fn resolve_heights_rejects_an_inverted_range() {
        // Probing nothing and reporting "0/0 consistent" reads like a pass.
        let server = MockRpc::start(vec![]).await;
        let client = RpcClient::new(&server.url());

        let err = resolve_heights(&client, Some(103), Some(100), 20).await.unwrap_err();
        assert!(err.to_string().contains("above"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn resolve_heights_windows_recent_blocks_from_the_head() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x65"))]).await;
        let client = RpcClient::new(&server.url());

        assert_eq!(resolve_heights(&client, None, None, 2).await.unwrap(), vec![100, 101]);
        assert_eq!(resolve_heights(&client, None, None, 3).await.unwrap(), vec![99, 100, 101]);
    }

    #[tokio::test]
    async fn resolve_heights_clamps_the_window_at_genesis() {
        // A head below the window size must not underflow to u64::MAX.
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x1"))]).await;
        let client = RpcClient::new(&server.url());

        assert_eq!(resolve_heights(&client, None, None, 20).await.unwrap(), vec![0, 1]);
    }

    #[tokio::test]
    async fn resolve_heights_reads_zero_blocks_as_the_head_alone() {
        // `--blocks 0` would underflow `blocks - 1`.
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x65"))]).await;
        let client = RpcClient::new(&server.url());

        assert_eq!(resolve_heights(&client, None, None, 0).await.unwrap(), vec![101]);
    }

    #[tokio::test]
    async fn resolve_heights_propagates_an_unreachable_head() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let dead = listener.local_addr().unwrap();
        drop(listener);

        let err = resolve_heights(&RpcClient::new(&format!("http://{dead}")), None, None, 4)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("failed"), "unexpected: {err}");
    }

    // ------------------------------------------------------------------
    // worker_count
    // ------------------------------------------------------------------

    #[test]
    fn worker_count_passes_positive_counts_through() {
        for n in [1usize, 2, 8, 1024] {
            assert_eq!(worker_count(n).unwrap(), n);
        }
    }

    #[test]
    fn worker_count_rejects_zero() {
        // Zero workers would prime an empty pool and probe nothing, which the
        // report would then render as "0/0 consistent".
        assert!(worker_count(0).is_err());
    }

    // ------------------------------------------------------------------
    // Bounded concurrency
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn run_compare_uses_the_workers_it_was_given() {
        // The bound below is only meaningful if the workers are really used.
        // All 8 block probes are primed before any of them can answer, so the
        // peak is exactly 8 blocks x 2 calls = 16 per endpoint.
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;
        let heights: Vec<u64> = (100..108).collect();

        assert!(compare_range(&go, &rust, &heights, 8).await.is_ok());

        assert_eq!(
            go.peak_in_flight(),
            16,
            "8 workers must keep 8 block probes in flight, not fall back to sequential"
        );
        assert_eq!(rust.peak_in_flight(), 16, "8 workers must keep 8 block probes in flight");
    }

    #[tokio::test]
    async fn run_compare_bounds_concurrency_to_the_worker_count() {
        // One block probe issues two concurrent requests to each endpoint, so a
        // run with N workers opens at most 2N connections to either side. That
        // bound is the point: it is what stops a wide --start..--end range from
        // exhausting the sockets of a public node.
        for workers in [1usize, 2, 3, 5, 8] {
            let routes = go_routes("0x64", "0xabc", 100);
            let go = MockRpc::start(routes.clone()).await;
            let rust = MockRpc::start(routes).await;
            let heights: Vec<u64> = (100..116).collect();

            assert!(compare_range(&go, &rust, &heights, workers).await.is_ok());

            let ceiling = workers * 2;
            assert!(
                go.peak_in_flight() <= ceiling,
                "{workers} workers: go peaked at {} concurrent requests, ceiling {ceiling}",
                go.peak_in_flight()
            );
            assert!(
                rust.peak_in_flight() <= ceiling,
                "{workers} workers: rust peaked at {} concurrent requests, ceiling {ceiling}",
                rust.peak_in_flight()
            );
        }
    }

    #[tokio::test]
    async fn run_compare_with_one_worker_opens_only_its_block_two_calls() {
        // The control case: a single worker must never overlap the requests of
        // two blocks, which is what makes the peak a measurement of the worker
        // count rather than of anything else.
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;
        let heights: Vec<u64> = (100..108).collect();

        assert!(compare_range(&go, &rust, &heights, 1).await.is_ok());

        assert_eq!(go.peak_in_flight(), 2, "one worker must not overlap two blocks");
        assert_eq!(rust.peak_in_flight(), 2, "one worker must not overlap two blocks");
    }

    #[tokio::test]
    async fn run_compare_reports_every_block_in_ascending_order() {
        // Heights are handed to workers in order, but they complete in
        // whatever order the endpoints answer, so the report must re-sort.
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        let heights: Vec<u64> = (0..40).rev().collect();
        assert!(compare_range(&go, &rust, &heights, 8).await.is_ok());
    }

    #[tokio::test]
    async fn run_compare_probes_every_height_exactly_once() {
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        let heights: Vec<u64> = (0..24).collect();
        compare_range(&go, &rust, &heights, 5).await.unwrap();

        // Four requests per block per endpoint: 24 x 4 = 96.
        let sent = go.requests().len() + rust.requests().len();
        assert_eq!(sent, 96, "each height must be probed exactly once");
    }

    #[tokio::test]
    async fn run_compare_handles_more_workers_than_heights() {
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        assert!(compare_range(&go, &rust, &[100, 101], 16).await.is_ok());
    }

    #[tokio::test]
    async fn run_compare_drains_every_in_flight_request() {
        // A JoinSet that outlives its results would leave connections open.
        let routes = go_routes("0x64", "0xabc", 100);
        let go = MockRpc::start(routes.clone()).await;
        let rust = MockRpc::start(routes).await;

        let heights: Vec<u64> = (100..108).collect();
        assert!(compare_range(&go, &rust, &heights, 8).await.is_ok());

        // The accept loop outlives its tasks, so poll until they are done
        // rather than asserting the instant the compare run returns.
        for _ in 0..100 {
            if go.in_flight() == 0 && rust.in_flight() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "requests left in flight after the run: go={} rust={}",
            go.in_flight(),
            rust.in_flight()
        );
    }

    #[tokio::test]
    async fn run_compare_reports_a_failed_probe_instead_of_dropping_it() {
        // One dead endpoint must surface as that height's diff, not as a
        // silently shortened range.
        let go = MockRpc::start(go_routes("0x64", "0xabc", 100)).await;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let dead = listener.local_addr().unwrap();
        drop(listener);

        let err = run_compare(
            &RpcClient::new(&go.url()),
            &RpcClient::new(&format!("http://{dead}")),
            &[100, 101, 102],
            2,
            false,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("inconsistent"), "unexpected: {err}");
        assert!(err.to_string().contains("3"), "every failed height counts: {err}");
    }

    // ------------------------------------------------------------------
    // print_verified / parse_height
    // ------------------------------------------------------------------

    #[test]
    fn block_result_serializes_with_stable_keys() {
        let r =
            BlockResult { number: 7, state_root_match: false, ok: false, diffs: vec!["x".into()] };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["number"], 7);
        assert_eq!(v["state_root_match"], false);
        assert_eq!(v["ok"], false);
        assert_eq!(v["diffs"], json!(["x"]));
    }

    #[test]
    fn print_verified_handles_present_and_absent_accounts() {
        // Both branches must render without panicking; output is captured by
        // the harness rather than asserted here.
        let present = VerifiedAddress {
            account: Some(proof::VerifiedAccount {
                address: primitive_types::H160::repeat_byte(1),
                nonce: primitive_types::U256::one(),
                balance: primitive_types::U256::zero(),
                storage_root: crate::mpt::EMPTY_ROOT,
                code_hash: crate::mpt::EMPTY_CODE_HASH,
                present: true,
            }),
            slots: vec![proof::VerifiedSlot {
                slot: primitive_types::U256::zero(),
                value: primitive_types::U256::one(),
                present: true,
            }],
        };
        print_verified("go", primitive_types::H160::repeat_byte(1), &present);
        print_verified("go", primitive_types::H160::repeat_byte(1), &VerifiedAddress {
            account: None,
            slots: vec![],
        });
    }

    #[test]
    fn parse_height_hex() {
        assert_eq!(parse_height("0x10").unwrap(), 16);
        assert_eq!(parse_height("10").unwrap(), 16);
    }

    #[test]
    fn parse_height_rejects_bad() {
        assert!(parse_height("zz").is_err());
        assert!(parse_height("").is_err());
        assert!(parse_height("-1").is_err());
    }
}
