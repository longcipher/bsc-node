//! Minimal JSON-RPC client over HTTP.
//!
//! Uses the workspace-preferred [`hpx`] HTTP client instead of `reqwest`. It
//! stays dependency-light and portable so the probe can be dropped into a
//! revm-based searcher without pulling in a full node client.

use eyre::{Result, eyre};
use serde_json::{Value, json};

/// HTTP JSON-RPC client bound to a single endpoint URL.
#[derive(Clone, Debug)]
pub struct RpcClient {
    url: String,
}

impl RpcClient {
    /// Creates a client targeting `url`.
    #[must_use]
    pub fn new(url: &str) -> Self {
        Self { url: url.to_string() }
    }

    /// Issues a single JSON-RPC `call` and returns the `result` field.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
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
        let client = RpcClient::new(&server.url());

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
        let client = RpcClient::new(&server.url());

        let err = client.call("eth_broken", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("method exploded"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn call_reports_unrouted_method_as_error() {
        let server = MockRpc::start(vec![]).await;
        let client = RpcClient::new(&server.url());

        assert!(client.call("eth_unknown", vec![]).await.is_err());
    }

    #[tokio::test]
    async fn call_rejects_non_json_response() {
        let server =
            MockRpc::start(vec![("eth_bad", raw_body("<html>gateway timeout</html>"))]).await;
        let client = RpcClient::new(&server.url());

        let err = client.call("eth_bad", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("not valid json"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn call_reports_unreachable_endpoint() {
        // Bind then immediately release a port so nothing is listening on it.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = RpcClient::new(&format!("http://{addr}"));
        let err = client.call("eth_chainId", vec![]).await.unwrap_err();
        assert!(err.to_string().contains("failed"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn null_result_is_returned_as_null() {
        // `eth_getBlockByNumber` for an unknown height returns JSON null, and
        // the caller relies on that rather than on an error.
        let server = MockRpc::start(vec![("eth_getBlockByNumber", Value::Null)]).await;
        let client = RpcClient::new(&server.url());

        let got = client.call("eth_getBlockByNumber", vec![]).await.unwrap();
        assert!(got.is_null());
    }

    // --- block_number ---

    #[tokio::test]
    async fn block_number_parses_hex_string() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x79141de"))]).await;
        let client = RpcClient::new(&server.url());

        assert_eq!(client.block_number().await.unwrap(), 0x79141de);
        assert_eq!(server.only_request()["method"], "eth_blockNumber");
        assert_eq!(server.only_request()["params"], json!([]));
    }

    #[tokio::test]
    async fn block_number_rejects_non_string_result() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!(1234))]).await;
        let client = RpcClient::new(&server.url());

        let err = client.block_number().await.unwrap_err();
        assert!(err.to_string().contains("non-string"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn block_number_rejects_malformed_hex() {
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0xnope"))]).await;
        let client = RpcClient::new(&server.url());

        assert!(client.block_number().await.is_err());
    }

    #[tokio::test]
    async fn block_number_serves_concurrent_callers() {
        // The compare driver fans out four requests per block.
        let server = MockRpc::start(vec![("eth_blockNumber", json!("0x2a"))]).await;
        let client = RpcClient::new(&server.url());

        let results =
            tokio::join!(client.block_number(), client.block_number(), client.block_number());
        assert_eq!(results.0.unwrap(), 42);
        assert_eq!(results.1.unwrap(), 42);
        assert_eq!(results.2.unwrap(), 42);
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn client_is_cloneable_for_parallel_probes() {
        let client = RpcClient::new("http://localhost:8545");
        let clone = client.clone();
        assert_eq!(clone.url, client.url);
    }
}
