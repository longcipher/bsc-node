//! End-to-end coverage of the CLI itself.
//!
//! The unit tests drive `compare` and `truth` through their internal
//! functions, which leaves the parts a user actually touches unverified: the
//! `clap` wiring, what `main` chooses to do, the stdout report, and the exit
//! code. Those are exactly the layers a refactor is most likely to break
//! silently, so they are exercised here against the built binary.
//!
//! Each test runs its own loopback JSON-RPC server, because a probe with
//! nothing to talk to is not a probe. The `eth_getProof` fixture is the same
//! real BSC mainnet capture the unit tests use, so the proofs verified here
//! are genuinely committed under the root the mock reports.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener},
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use serde_json::{Value, json};

/// The real mainnet proof capture shared with the unit tests.
const FIXTURE: &str = include_str!("fixtures/bsc_mainnet_proof.json");

/// A loopback JSON-RPC endpoint answering canned per-method results.
///
/// Self-contained rather than shared with the unit-test mock: an integration
/// test compiles against the bin crate's test binary, not against it, so
/// `#[cfg(test)] mod mock_rpc` is not reachable from here.
struct MockRpc {
    addr: SocketAddr,
    served: Arc<AtomicUsize>,
    /// Method names in arrival order, so a test can assert *what* was asked.
    methods: Arc<Mutex<Vec<String>>>,
}

impl MockRpc {
    /// Serves `method -> result` for every request, forever.
    fn start(routes: &[(&str, Value)]) -> Self {
        Self::spawn(routes, None)
    }

    /// Serves `method -> result` until `limit` requests have been answered,
    /// then answers a JSON-RPC error.
    fn start_limited(routes: &[(&str, Value)], limit: usize) -> Self {
        Self::spawn(routes, Some(limit))
    }

    fn spawn(routes: &[(&str, Value)], limit: Option<usize>) -> Self {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .expect("mock: bind loopback listener");
        let addr = listener.local_addr().expect("mock: read local addr");
        let routes: Arc<Vec<(String, Value)>> =
            Arc::new(routes.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect());
        let served = Arc::new(AtomicUsize::new(0));
        let methods = Arc::new(Mutex::new(Vec::new()));

        let srv_routes = Arc::clone(&routes);
        let srv_served = Arc::clone(&served);
        let srv_methods = Arc::clone(&methods);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let routes = Arc::clone(&srv_routes);
                let served = Arc::clone(&srv_served);
                let methods = Arc::clone(&srv_methods);
                thread::spawn(move || {
                    let _ = serve(&mut stream, &routes, limit, &served, &methods);
                });
            }
        });

        Self { addr, served, methods }
    }

    /// The `http://host:port` URL to point the CLI at.
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// How many requests this endpoint has answered.
    fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }

    /// The methods that were requested, in arrival order.
    fn methods(&self) -> Vec<String> {
        self.methods.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Whether `method` was requested at least once.
    fn asked_for(&self, method: &str) -> bool {
        self.methods().iter().any(|m| m == method)
    }
}

/// Handles one connection: reads the request, answers the routed result.
fn serve(
    stream: &mut std::net::TcpStream,
    routes: &[(String, Value)],
    limit: Option<usize>,
    served: &AtomicUsize,
    methods: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let request = read_request(stream)?;
    let Some(req) = request else { return Ok(()) };
    let count = served.fetch_add(1, Ordering::SeqCst) + 1;

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
    if let Ok(mut log) = methods.lock() {
        log.push(method.clone());
    }

    let body = match limit {
        Some(limit) if count > limit => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": "mock: request budget exhausted" },
        })
        .to_string(),
        _ => match routes.iter().find(|(m, _)| m == &method) {
            Some((_, result)) => {
                json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
            }
            None => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("mock: no route for {method}") },
            })
            .to_string(),
        },
    };

    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: \
         close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Reads one `Content-Length` framed request body, or `None` on EOF.
fn read_request(stream: &mut std::net::TcpStream) -> std::io::Result<Option<Value>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }

    let length = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(std::io::Error::other)
}

/// Runs the built binary with `args` and captures its exit status and stdout.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bsc-consistency-probe"))
        .args(args)
        .env_remove("RUST_LOG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("probe binary must run")
}

/// The stdout of a successful run.
fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The block fields `compare` reads, for a header that agrees on everything.
fn block_header(height: u64, state_root: &str) -> Value {
    let mut v = json!({
        "transactions": [json!({ "hash": "0xaa" })],
        "number": format!("0x{height:x}"),
        "hash": format!("0x{height:064x}"),
        "parentHash": format!("0x{:064x}", height.saturating_sub(1)),
        "stateRoot": state_root,
        "transactionsRoot": "0x1234",
        "receiptsRoot": "0x1234",
        "logsBloom": "0x1234",
        "gasUsed": "0x5208",
        "gasLimit": "0x1c9c380",
        "timestamp": "0x65000000",
        "mixHash": "0x1234",
        "nonce": "0x0000000000000000",
        "baseFeePerGas": "0x3b9aca00",
        "withdrawalsRoot": "0x1234",
        "blobGasUsed": "0x1234",
        "excessBlobGas": "0x1234",
        "difficulty": "0x1234",
        "extraData": "0x1234",
    });
    // geth omits null fields, so a header may legitimately skip extras.
    v.as_object_mut().expect("block is an object");
    v
}

/// Routes serving an agreeing pair of endpoints for `compare`.
fn compare_routes(state_root: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("eth_blockNumber", json!("0x65")),
        ("eth_getBlockByNumber", block_header(101, state_root)),
        ("eth_getBlockReceipts", json!([])),
    ]
}

// ----------------------------------------------------------------------
// compare
// ----------------------------------------------------------------------

#[test]
fn compare_reports_a_summary_and_exits_zero_when_the_endpoints_agree() {
    let routes = compare_routes("0xabc");
    let go = MockRpc::start(&routes);
    let rust = MockRpc::start(&routes);

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "102",
    ]);

    // `main` choosing to do the work at all is what this pins: replacing its
    // body with `Ok(())` produced a silent exit 0.
    assert!(out.status.code() == Some(0), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("SUMMARY"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("3/3 blocks consistent"), "unexpected stdout: {stdout}");
    // Both endpoints must have been consulted for every block.
    assert_eq!(go.served(), 6, "go served {} requests", go.served());
    assert_eq!(rust.served(), 6, "rust served {} requests", rust.served());
}

#[test]
fn compare_reports_every_inconsistent_block_and_exits_nonzero() {
    // The divergent detail listing is written only when a block disagrees, so
    // a diff on this path is the only thing that observes it.
    let go = MockRpc::start(&compare_routes("0xabc"));
    let mut lying = compare_routes("0xdeadbeef");
    lying.push(("eth_blockNumber", json!("0x65")));
    let rust = MockRpc::start(lying.as_slice());

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "102",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("INCONSISTENT BLOCKS"), "unexpected stdout: {stdout}");
    for height in ["#100:", "#101:", "#102:"] {
        assert!(stdout.contains(height), "missing {height} in: {stdout}");
    }
    assert!(stdout.contains("header.stateRoot"), "unexpected stdout: {stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.lines().any(|l| l.contains("3 block(s) inconsistent across Go & Rust endpoints")),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn compare_json_report_is_machine_readable_and_still_fails() {
    let go = MockRpc::start(&compare_routes("0xabc"));
    let mut lying = compare_routes("0xdeadbeef");
    lying.push(("eth_blockNumber", json!("0x65")));
    let rust = MockRpc::start(lying.as_slice());

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "102",
        "--json",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    let report: Value = serde_json::from_str(&stdout).expect("--json must emit valid JSON");
    assert_eq!(report["total"], 3);
    assert_eq!(report["failed"], 3);
    assert_eq!(report["passed"], 0);
    // The per-block rows keep the contract the summary counts.
    assert_eq!(report["results"][0]["number"], 100);
    assert_eq!(report["results"][0]["state_root_match"], false);
    assert_eq!(report["results"][0]["ok"], false);
    assert!(report["results"][0]["diffs"][0].as_str().is_some_and(|d| d.contains("stateRoot")));
}

#[test]
fn compare_defaults_to_the_recent_window_from_the_go_endpoint() {
    let mut routes = compare_routes("0xabc");
    routes.push(("eth_blockNumber", json!("0x65")));
    let go = MockRpc::start(&routes);
    let rust = MockRpc::start(&routes);

    let out = run(&["compare", "--go", &go.url(), "--rust", &rust.url(), "--blocks", "3"]);

    assert!(out.status.code() == Some(0), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("3/3 blocks consistent"), "unexpected stdout: {stdout}");
    for height in ["99", "100", "101"] {
        assert!(stdout.contains(height), "window is missing block {height}: {stdout}");
    }
}

#[test]
fn compare_rejects_an_inverted_range_before_any_rpc_call() {
    // An empty range would otherwise print "0/0 consistent" and exit zero,
    // which is indistinguishable from a pass.
    let go = MockRpc::start(&compare_routes("0xabc"));
    let rust = MockRpc::start(&compare_routes("0xabc"));

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "102",
        "--end",
        "100",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    assert_eq!(go.served(), 0, "an inverted range must not probe anything");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("above --end"), "unexpected stderr: {stderr}");
}

#[test]
fn compare_rejects_zero_workers() {
    let go = MockRpc::start(&compare_routes("0xabc"));
    let rust = MockRpc::start(&compare_routes("0xabc"));

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "100",
        "--workers",
        "0",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    assert_eq!(go.served(), 0, "zero workers must not probe anything");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--workers"), "unexpected stderr: {stderr}");
}

#[test]
fn compare_reports_a_failed_probe_for_every_block_it_was_asked_to_check() {
    // One dead endpoint must not silently shorten the range.
    let go = MockRpc::start(&compare_routes("0xabc"));
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dead = listener.local_addr().unwrap();
    drop(listener);

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &format!("http://{dead}"),
        "--start",
        "100",
        "--end",
        "104",
        "--workers",
        "4",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("5 block(s) inconsistent"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for height in ["#100:", "#101:", "#102:", "#103:", "#104:"] {
        assert!(stdout.contains(height), "missing {height} in: {stdout}");
    }
}

#[test]
fn compare_fails_fast_when_an_endpoint_never_answers() {
    // A probe run as a gate cannot wait forever: a wedged endpoint or a
    // black-holing path must produce a failure and a non-zero exit.
    let hanging = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dead = hanging.local_addr().unwrap();
    // Accept every connection and never answer it.
    thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((stream, _)) = hanging.accept() {
            held.push(stream);
        }
    });

    let start = std::time::Instant::now();
    let out = run(&[
        "compare",
        "--go",
        &format!("http://{dead}"),
        "--rust",
        &format!("http://{dead}"),
        "--start",
        "100",
        "--end",
        "103",
        "--workers",
        "4",
        "--timeout-ms",
        "300",
    ]);
    let elapsed = start.elapsed();

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    // Bounded by the timeout, not by patience. The generous ceiling leaves
    // room for a slow CI machine while still catching a run that hangs.
    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "took {elapsed:?}, which means the timeout is not bounding the run"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("4 block(s) inconsistent"), "unexpected stderr: {stderr}");
    // Every height must still be reported, not silently dropped.
    let stdout = stdout_of(&out);
    assert!(stdout.contains("timed out"), "unexpected stdout: {stdout}");
    for height in ["#100:", "#101:", "#102:", "#103:"] {
        assert!(stdout.contains(height), "missing {height} in: {stdout}");
    }
}

#[test]
fn compare_with_receipts_agrees_and_exits_zero() {
    // `--receipts` opts into a second comparison per block, so the endpoint has
    // to be asked for them at all before anything can agree or disagree.
    let go = MockRpc::start(&compare_routes("0xabc"));
    let rust = MockRpc::start(&compare_routes("0xabc"));

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "102",
        "--receipts",
    ]);

    assert!(
        out.status.code() == Some(0),
        "exit: {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    // eth_getBlockReceipts must actually have been requested.
    assert!(go.asked_for("eth_getBlockReceipts"), "go was never asked for receipts");
    assert!(rust.asked_for("eth_getBlockReceipts"), "rust was never asked for receipts");
}

#[test]
fn compare_without_receipts_never_asks_for_them() {
    // The comparison is opt-in, so an unflagged run must not spend the calls.
    let go = MockRpc::start(&compare_routes("0xabc"));
    let rust = MockRpc::start(&compare_routes("0xabc"));

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "100",
    ]);

    assert!(out.status.code() == Some(0), "exit: {:?}", out.status);
    assert!(!go.asked_for("eth_getBlockReceipts"), "an unflagged run must not fetch receipts");
}

#[test]
fn compare_with_receipts_detects_a_receipt_divergence() {
    // A receipts difference must fail the run even when the headers match,
    // which is the whole reason the flag exists.
    let go = MockRpc::start(&compare_routes("0xabc"));
    // The router answers the first match for a method, so the receipts route
    // has to be replaced rather than appended, or the honest one still wins.
    let mut lying: Vec<(&str, Value)> =
        compare_routes("0xabc").into_iter().filter(|(m, _)| *m != "eth_getBlockReceipts").collect();
    lying.push((
        "eth_getBlockReceipts",
        json!([{ "status": "0x1", "gasUsed": "0x10",
                                                 "transactionHash": "0xaa", "logs": [{}] }]),
    ));
    let rust = MockRpc::start(lying.as_slice());

    let out = run(&[
        "compare",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--start",
        "100",
        "--end",
        "100",
        "--receipts",
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    assert!(stdout.contains("receipt count"), "unexpected stdout: {stdout}");
}

// ----------------------------------------------------------------------
// truth
// ----------------------------------------------------------------------

/// Routes serving a block and its real mainnet `eth_getProof` response.
///
/// Returns the block tag and the routes that answer it, so a test can hand
/// the same setup to two endpoints. The route names are `'static` literals
/// and the results are owned, so nothing has to outlive the test.
fn truth_routes(key: &str) -> (String, Vec<(&'static str, Value)>) {
    let f: Value = serde_json::from_str(FIXTURE).expect("mainnet fixture must be valid JSON");
    let block = f["block"].as_str().expect("block").to_string();
    let root = f["state_root"].as_str().expect("state_root").to_string();
    let proof = f[key]["response"].clone();

    (block, vec![
        ("eth_blockNumber", json!("0x79141de")),
        ("eth_getBlockByNumber", json!({ "stateRoot": root })),
        ("eth_getProof", proof),
    ])
}

#[test]
fn truth_verifies_a_real_proof_and_exits_zero() {
    // The per-address verification line and the values in it are only ever
    // printed by `print_verified`, so a run that reports them is the only
    // thing that distinguishes it from a stub.
    let (block, routes) = truth_routes("present");
    let endpoint = MockRpc::start(&routes);

    let out = run(&[
        "truth",
        "--go",
        &endpoint.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197955",
        "--storage-slots",
        "0x0,0x1",
        "--block",
        &block,
    ]);

    assert!(
        out.status.code() == Some(0),
        "exit: {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("OK"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("storageRoot"), "unexpected stdout: {stdout}");
    // Both requested slots must be reported, not just the first.
    assert!(stdout.contains("slot 0 ="), "unexpected stdout: {stdout}");
    assert!(stdout.contains("slot 1 ="), "unexpected stdout: {stdout}");
    assert!(stdout.contains("present"), "unexpected stdout: {stdout}");
}

#[test]
fn truth_makes_no_json_rpc_call_before_parsing_addresses() {
    // A bad address must fail fast: probing an endpoint about an address we
    // cannot even spell would be noise.
    let (block, routes) = truth_routes("present");
    let endpoint = MockRpc::start_limited(&routes, 0);

    let out = run(&[
        "truth",
        "--go",
        &endpoint.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197956",
        "--block",
        &block,
    ]);

    assert!(out.status.code() == Some(1), "exit: {:?}", out.status);
    assert_eq!(endpoint.served(), 0, "a bad checksum must not reach the endpoint");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("EIP-55"), "unexpected stderr: {stderr}");
}

#[test]
fn truth_accepts_a_single_endpoint() {
    // The one-client path is the documented "trust Go" mode; it has no second
    // endpoint, so there is nothing to agree or disagree with.
    let (block, routes) = truth_routes("present");
    let endpoint = MockRpc::start(&routes);

    let out = run(&[
        "truth",
        "--endpoint",
        &endpoint.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197955",
        "--storage-slots",
        "0x0",
        "--block",
        &block,
    ]);

    assert!(
        out.status.code() == Some(0),
        "exit: {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[endpoint]"), "the endpoint must be labelled: {stdout}");
    assert!(stdout.contains("OK"), "unexpected stdout: {stdout}");
    assert!(!stdout.contains("CROSS-CLIENT"), "unexpected stdout: {stdout}");
}

#[test]
fn truth_cross_checks_two_agreeing_endpoints() {
    // The two-endpoint path is the only one that runs the cross-client
    // comparison, so it is the only place its agreement line can be observed.
    let (block, routes) = truth_routes("present");
    let go = MockRpc::start(&routes);
    let rust = MockRpc::start(&routes);

    let out = run(&[
        "truth",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197955",
        "--storage-slots",
        "0x0",
        "--block",
        &block,
    ]);

    assert!(
        out.status.code() == Some(0),
        "exit: {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("CROSS-CLIENT (Go vs Rust) state agreement: OK"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn truth_fails_when_the_two_endpoints_disagree() {
    // The verified account is the same, but the two endpoints are handed
    // different roots, so their proofs cannot both be honest.
    let (block, routes) = truth_routes("present");
    let go = MockRpc::start(&routes);

    // A root that does not match the fixture's proof: the proof cannot be
    // verified under it, so this endpoint reports a failure while the other
    // verifies cleanly.
    let wrong_root = "0xcf738a4646d55827f2ffffffffffffffffffffffffffffffffffffffffffffff";
    let rust = MockRpc::start(&[
        ("eth_blockNumber", json!("0x79141de")),
        ("eth_getBlockByNumber", json!({ "stateRoot": wrong_root })),
    ]);

    let out = run(&[
        "truth",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197955",
        "--storage-slots",
        "0x0",
        "--block",
        &block,
    ]);

    assert!(
        out.status.code() == Some(1),
        "exit: {:?}, stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("disagree"), "unexpected stderr: {stderr}");
}

#[test]
fn truth_json_report_has_a_cross_client_agreement_field() {
    let (block, routes) = truth_routes("present");
    let go = MockRpc::start(&routes);
    let rust = MockRpc::start(&routes);

    let out = run(&[
        "truth",
        "--go",
        &go.url(),
        "--rust",
        &rust.url(),
        "--addresses",
        "0x55d398326f99059fF775485246999027B3197955",
        "--block",
        &block,
        "--json",
    ]);

    assert!(out.status.code() == Some(0), "exit: {:?}", out.status);
    let stdout = stdout_of(&out);
    // The JSON report follows the same progress lines the non-JSON run prints.
    let payload = stdout.lines().skip_while(|l| *l != "{").collect::<Vec<_>>().join("\n");
    let report: Value = serde_json::from_str(&payload)
        .unwrap_or_else(|e| panic!("--json must emit JSON: {e}\n{payload}"));
    assert_eq!(report["cross_client_agreement"], true);
    assert!(report["go"]["addresses"][0]["account"].is_object(), "unexpected: {report}");
    assert!(report["rust"]["addresses"][0]["account"].is_object(), "unexpected: {report}");
}
