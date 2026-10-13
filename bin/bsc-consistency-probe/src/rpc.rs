//! Minimal JSON-RPC client over HTTP.
//!
//! Uses the workspace-preferred [`hpx`] HTTP client instead of `reqwest`. It
//! stays dependency-light and portable so the probe can be dropped into a
//! revm-based searcher without pulling in a full node client.

use std::time::Duration;

use eyre::{Result, eyre};
use serde_json::{Value, json};

/// Default per-request timeout, in milliseconds.
///
/// A probe is meant to be run as a gate, so an endpoint that wedges has to
/// produce a failure and a non-zero exit rather than an unbounded wait. Ten
/// seconds is far above a healthy BSC node's response time, including the
/// proof for a large account, and short enough that a stuck run is noticed.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// HTTP JSON-RPC client bound to a single endpoint URL.
#[derive(Clone, Debug)]
pub struct RpcClient {
    url: String,
    timeout: Duration,
}

impl RpcClient {
    /// Creates a client targeting `url` with an explicit request timeout.
    ///
    /// The timeout covers the whole exchange rather than only the send, so an
    /// endpoint that accepts a connection and then goes quiet fails a request
    /// instead of leaving the caller waiting forever. This is the only
    /// constructor: every caller has a timeout policy, even when it is the
    /// default, and naming it at each call site keeps that policy visible.
    ///
    /// A zero timeout is clamped to 1ms rather than rejected: it makes every
    /// request fail, which is a configuration mistake the caller notices on the
    /// first call, and is not worth an error path of its own.
    #[must_use]
    pub fn with_timeout_ms(url: &str, timeout_ms: u64) -> Self {
        Self { url: url.to_string(), timeout: Duration::from_millis(timeout_ms.max(1)) }
    }

    /// Issues a single JSON-RPC `call` and returns the `result` field.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        tokio::time::timeout(self.timeout, self.call_unbounded(method, params)).await.map_err(
            |_| eyre!("rpc request to {} timed out after {}ms", self.url, self.timeout.as_millis()),
        )?
    }

    /// The exchange without the timeout wrapper, which a `.await` inside would
    /// otherwise reset.
    async fn call_unbounded(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let resp = hpx::post(self.url.as_str())
            .json(&body)
            .send()
            .await
            .map_err(|e| eyre!("rpc http request to {} failed: {e}", self.url))?;

        let v: Value = resp
            .json()
            .await
            .map_err(|e| eyre!("rpc response from {} was not valid json: {e}", self.url))?;

        let Some(error) = v.get("error") else {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        };
        if error.is_null() {
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
        Err(eyre!("rpc error: {error}"))
    }

    /// Fetches the current chain head block number.
    pub async fn block_number(&self) -> Result<u64> {
        let v = self.call("eth_blockNumber", vec![]).await?;
        let s =
            v.as_str().ok_or_else(|| eyre!("eth_blockNumber returned non-string value: {v}"))?;
        parse_u64_hex(s)
    }
}

/// Parses a `0x`-prefixed hex string as a `u64`.
pub fn parse_u64_hex(s: &str) -> Result<u64> {
    let s = s.trim_start_matches("0x");
    u64::from_str_radix(s, 16).map_err(|e| eyre!("invalid hex number {s:?}: {e}"))
}

/// Formats a `u64` as a `0x`-prefixed hex string.
#[must_use]
pub fn to_hex(num: u64) -> String {
    format!("0x{num:x}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    /// A client for a test endpoint, on the default timeout.
    ///
    /// Tests almost always want the default, so they state it once here rather
    /// than at each of the dozens of call sites.
    fn client(url: &str) -> RpcClient {
        RpcClient::with_timeout_ms(url, DEFAULT_TIMEOUT_MS)
    }

    use serde_json::json;

    use super::*;
    use crate::mock_rpc::{MockRpc, raw_body, rpc_error};

    #[test]
    fn hex_roundtrip_zero() {
        assert_eq!(to_hex(0), "0x0");
        assert_eq!(parse_u64_hex("0x0").unwrap(), 0);
    }

    #[test]
    fn hex_roundtrip_known_values() {
        assert_eq!(to_hex(0x1f), "0x1f");
        assert_eq!(to_hex(255), "0xff");
        assert_eq!(parse_u64_hex("0xff").unwrap(), 255);
    }

    #[test]
    fn parse_u64_hex_without_prefix() {
        assert_eq!(parse_u64_hex("deadbeef").unwrap(), 0xdead_beef);
    }

    #[test]
    fn parse_u64_hex_large() {
        // Largest u64.
        assert_eq!(parse_u64_hex("0xffffffffffffffff").unwrap(), u64::MAX);
    }

    #[test]
    fn parse_u64_hex_rejects_non_hex() {
        assert!(parse_u64_hex("0xzz").is_err());
        assert!(parse_u64_hex("not-hex").is_err());
        assert!(parse_u64_hex("").is_err());
    }

    #[test]
    fn parse_u64_hex_rejects_out_of_range() {
        // One past u64::MAX must be rejected rather than silently truncated.
        assert!(parse_u64_hex("0x1ffffffffffffffff").is_err());
    }

    #[test]
    fn to_hex_then_parse_is_identity() {
        for v in [0u64, 1, 42, 1_000_000, u64::MAX] {
            assert_eq!(parse_u64_hex(&to_hex(v)).unwrap(), v);
        }
    }

    // --- transport ---

    #[tokio::test]
    async fn call_returns_result_and_sends_a_well_formed_request() {
        let server = MockRpc::start(vec![("eth_chainId", json!("0x38"))]).await;
        let client = client(&server.url());

        let got = client.call("eth_chainId", vec![Value::String("0x1".into())]).await.unwrap();
        assert_eq!(got, json!("0x38"));

        let req = server.only_request();
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "eth_chainId");
        assert_eq!(req["params"], json!(["0x1"]));
        assert!(req.get("id").is_some(), "request must carry an id");
    }

    #[tokio::test]
    async fn call_propagates_json_rpc_error() {
        let server = MockRpc::start(vec![("eth_broken", rpc_error("method exploded"))]).await;
        let client = client(&server.url());

        let err = client.call("eth_broken", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("method exploded"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn call_reports_unrouted_method_as_error() {
        let server = MockRpc::start(vec![]).await;
        let client = client(&server.url());

        assert!(client.call("eth_unknown", vec![]).await.is_err());
    }

    #[tokio::test]
    async fn call_rejects_non_json_response() {
        let server =
            MockRpc::start(vec![("eth_bad", raw_body("<html>gateway timeout</html>"))]).await;
        let client = client(&server.url());

        let err = client.call("eth_bad", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("not valid json"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn call_reports_unreachable_endpoint() {
        // Bind then immediately release a port so nothing is listening on it.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = client(&format!("http://{addr}"));
        let err = client.call("eth_chainId", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("failed"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn null_result_is_returned_as_null() {
        // `eth_getBlockByNumber` for an unknown height returns JSON null, and
        // the caller relies on that rather than on an error.
        let server = MockRpc::start(vec![("eth_getBlockByNumber", Value::Null)]).await;
        let client = client(&server.url());

        let got = client.call("eth_getBlockByNumber", vec![]).await.unwrap();
        assert!(got.is_null());
    }

    // --- block_number ---

    #[tokio::test]
    async fn block_number_parses_hex_string() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x79141de"))]).await;
        let client = client(&server.url());

        assert_eq!(client.block_number().await.unwrap(), 0x79141de);
        assert_eq!(server.only_request()["method"], "eth_blockNumber");
        assert_eq!(server.only_request()["params"], json!([]));
    }

    #[tokio::test]
    async fn block_number_rejects_non_string_result() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!(1234))]).await;
        let client = client(&server.url());

        let err = client.block_number().await.unwrap_err();
        assert!(err.to_string().contains("non-string"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn block_number_rejects_malformed_hex() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0xnope"))]).await;
        let client = client(&server.url());

        assert!(client.block_number().await.is_err());
    }

    #[tokio::test]
    async fn block_number_serves_concurrent_callers() {
        // The compare driver fans out four requests per block.
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x2a"))]).await;
        let client = client(&server.url());

        let results =
            tokio::join!(client.block_number(), client.block_number(), client.block_number());
        assert_eq!(results.0.unwrap(), 42);
        assert_eq!(results.1.unwrap(), 42);
        assert_eq!(results.2.unwrap(), 42);
        assert_eq!(server.requests().len(), 3);
    }

    // --- timeouts ---

    /// An endpoint that accepts connections and never answers.
    ///
    /// This is the failure a probe must not hang on: a node wedged mid-request,
    /// or a path that black-holes rather than resetting, leaves the caller with
    /// no exit and no verdict.
    async fn start_silent_endpoint() -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Hold every connection open without reading or writing, so the
            // client is stuck waiting for bytes that never come.
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn call_times_out_against_an_endpoint_that_never_answers() {
        let dead = start_silent_endpoint().await;
        let slow = RpcClient::with_timeout_ms(&dead, 150);

        // The whole point: this returns a failure instead of hanging the run.
        let within = tokio::time::timeout(Duration::from_secs(5), async {
            slow.call("eth_chainId", vec![]).await
        })
        .await;

        let err = within.expect("a silent endpoint must not hang the client").unwrap_err();
        assert!(err.to_string().contains("timed out"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn a_silent_endpoint_fails_every_rpc_layer() {
        // Both the compare and truth paths ask for the head before probing, so
        // neither may ride out a silent endpoint.
        let dead = start_silent_endpoint().await;
        let slow = RpcClient::with_timeout_ms(&dead, 150);

        let err = slow.block_number().await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "unexpected: {err}");
    }

    #[test]
    fn a_zero_timeout_is_clamped_rather_than_instant() {
        // Zero milliseconds would make every request fail immediately, which
        // is a configuration mistake, not a state worth a special error.
        let c = RpcClient::with_timeout_ms("http://localhost:8545", 0);
        assert!(c.timeout >= Duration::from_millis(1), "zero must be clamped");
    }

    #[test]
    fn client_is_cloneable_for_parallel_probes() {
        let client = client("http://localhost:8545");
        let clone = client.clone();
        assert_eq!(clone.url, client.url);
    }
}
