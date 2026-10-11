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
