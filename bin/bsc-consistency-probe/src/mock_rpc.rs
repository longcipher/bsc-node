//! In-test loopback JSON-RPC server.
//!
//! [`crate::rpc::RpcClient`], [`crate::proof::verify_address`] and the compare
//! driver are all async network code that would otherwise go untested. This
//! module binds an ephemeral `127.0.0.1:0` listener that replays canned
//! per-method responses, so those paths can be driven deterministically and
//! offline.
//!
//! Routes map a method name to the JSON-RPC `result` it should answer with.
//! Use [`rpc_error`] or [`raw_body`] as the routed value to exercise the
//! client-side failure paths.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

/// Sentinel key selecting a response body that is not valid JSON.
const RAW_BODY_KEY: &str = "$rawBody";

/// Sentinel key selecting a JSON-RPC `error` reply.
const ERROR_KEY: &str = "$error";

/// Builds a route value that makes the server answer with a JSON-RPC error.
#[must_use]
pub fn rpc_error(message: &str) -> Value {
    json!({ ERROR_KEY: message })
}

/// Builds a route value that makes the server answer with a non-JSON body.
#[must_use]
pub fn raw_body(body: &str) -> Value {
    json!({ RAW_BODY_KEY: body })
}

/// A running loopback JSON-RPC endpoint.
pub struct MockRpc {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
    in_flight: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

impl MockRpc {
    /// Binds a loopback listener serving `routes` and spawns its accept loop.
    ///
    /// Each connection is handled on its own task, so concurrent `try_join!`
    /// calls in the code under test are served concurrently too.
    pub async fn start(routes: Vec<(&str, Value)>) -> Self {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).await.expect("mock rpc: bind loopback listener");
        let addr = listener.local_addr().expect("mock rpc: read local addr");

        let routes: Arc<HashMap<String, Value>> =
            Arc::new(routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak_in_flight = Arc::new(AtomicUsize::new(0));

        let srv_routes = Arc::clone(&routes);
        let srv_requests = Arc::clone(&requests);
        let srv_flight = Arc::clone(&in_flight);
        let srv_peak = Arc::clone(&peak_in_flight);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let routes = Arc::clone(&srv_routes);
                let requests = Arc::clone(&srv_requests);
                let in_flight = Arc::clone(&srv_flight);
                let peak_in_flight = Arc::clone(&srv_peak);
                tokio::spawn(async move {
                    let _ = serve(stream, &routes, &requests, &in_flight, &peak_in_flight).await;
                });
            }
        });

        Self { addr, requests, in_flight, peak_in_flight }
    }

    /// The `http://host:port` URL to point an [`crate::rpc::RpcClient`] at.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every JSON-RPC request body the server has received, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().map_or_else(|e| e.into_inner().clone(), |g| g.clone())
    }

    /// The largest number of requests this endpoint was serving at once.
    ///
    /// This is what lets a test *require* concurrency: a caller that never
    /// overlaps its requests leaves this at 1 no matter how it is configured,
    /// so an assertion on it cannot be satisfied by an unrelated speed-up.
    #[must_use]
    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    /// The number of requests still being served.
    ///
    /// A probe run must always drain to zero: anything still in flight here
    /// after the run means a connection was left open, which the results would
    /// never reveal.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// The single request the server received.
    ///
    /// # Panics
    ///
    /// Panics unless exactly one request arrived, which keeps test call sites
    /// free of index arithmetic.
    #[must_use]
    pub fn only_request(&self) -> Value {
        let reqs = self.requests();
        assert_eq!(reqs.len(), 1, "expected exactly one request, got {reqs:?}");
        reqs.into_iter().next().unwrap_or(Value::Null)
    }
}

async fn serve(
    mut stream: TcpStream,
    routes: &HashMap<String, Value>,
    requests: &Mutex<Vec<Value>>,
    in_flight: &AtomicUsize,
    peak_in_flight: &AtomicUsize,
) -> Result<(), String> {
    let open = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    // A plain store is safe here: the count only ever rises while this request
    // is being served, so the last writer in always holds the maximum.
    peak_in_flight.fetch_max(open, Ordering::SeqCst);

    let served = serve_inner(&mut stream, routes, requests).await;

    in_flight.fetch_sub(1, Ordering::SeqCst);
    served
}

/// Reads one request and answers it, with the in-flight accounting done by the
/// caller so every exit path decrements.
async fn serve_inner(
    stream: &mut TcpStream,
    routes: &HashMap<String, Value>,
    requests: &Mutex<Vec<Value>>,
) -> Result<(), String> {
    let Some(req) = read_request(stream).await? else {
        return Ok(());
    };
    if let Ok(mut log) = requests.lock() {
        log.push(req.clone());
    }

    let id = req.get("id").cloned().unwrap_or_else(|| json!(1));
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default().to_string();

    let body = match routes.get(&method) {
        Some(v) if v.get(ERROR_KEY).is_some() => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": v[ERROR_KEY].as_str().unwrap_or("mock error") },
        })
        .to_string(),
        Some(v) if v.get(RAW_BODY_KEY).is_some() => {
            v[RAW_BODY_KEY].as_str().unwrap_or("not json").to_string()
        }
        Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string(),
        None => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("mock rpc: no route for {method}") },
        })
        .to_string(),
    };

    write_response(stream, &body).await
}
async fn read_request(stream: &mut TcpStream) -> Result<Option<Value>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];

    let header_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = head
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .next_back()
        .unwrap_or(0);

    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let end = buf.len().min(body_start + content_length);
    serde_json::from_slice(&buf[body_start..end]).map(Some).map_err(|e| e.to_string())
}

async fn write_response(stream: &mut TcpStream, body: &str) -> Result<(), String> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(body.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())
}
