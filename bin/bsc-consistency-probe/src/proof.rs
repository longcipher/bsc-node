//! Fetches `eth_getProof` from a client and cryptographically verifies it
//! against the reported state root. This is the "truth" layer on top of the
//! raw RPC.

use eyre::{Result, eyre};
use primitive_types::{H160, H256, U256};
use serde::Serialize;
use serde_json::Value;

use crate::{
    mpt::{decode_account, verify_account_proof, verify_storage_proof},
    rpc::RpcClient,
};

/// A successfully verified account under a state root.
#[derive(Debug, Serialize)]
pub struct VerifiedAccount {
    /// Account address.
    pub address: H160,
    /// Transaction count.
    pub nonce: U256,
    /// Native balance in wei.
    pub balance: U256,
    /// Account storage root.
    pub storage_root: H256,
    /// Contract code hash.
    pub code_hash: H256,
    /// Whether the account exists in the trie.
    pub present: bool,
}

/// A successfully verified storage slot under an account storage root.
#[derive(Debug, Serialize)]
pub struct VerifiedSlot {
    /// Storage slot index.
    pub slot: U256,
    /// Slot value.
    pub value: U256,
    /// Whether the slot exists in the trie.
    pub present: bool,
}

/// Full verification result for one address.
#[derive(Debug, Serialize)]
pub struct VerifiedAddress {
    /// Account verification, if present.
    pub account: Option<VerifiedAccount>,
    /// Per-slot verifications.
    pub slots: Vec<VerifiedSlot>,
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    hex::decode(s.trim_start_matches("0x")).unwrap_or_default()
}

fn parse_address(s: &str) -> Result<H160> {
    let b = hex_to_bytes(s);
    if b.len() != 20 {
        return Err(eyre!("invalid address length (got {} bytes) for {s}", b.len()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&b);
    Ok(H160::from(a))
}

fn parse_slot(s: &str) -> Result<U256> {
    let s = s.trim_start_matches("0x");
    U256::from_str_radix(s, 16).map_err(|e| eyre!("invalid slot {s:?}: {e}"))
}

async fn block_state_root(client: &RpcClient, block: &str) -> Result<H256> {
    let blk = client
        .call("eth_getBlockByNumber", vec![Value::String(block.to_string()), Value::Bool(false)])
        .await?;
    let root_hex = blk
        .get("stateRoot")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("block {block} missing stateRoot"))?;
    let b = hex_to_bytes(root_hex);
    if b.len() != 32 {
        return Err(eyre!("bad stateRoot length (got {} bytes) for block {block}", b.len()));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&b);
    Ok(H256::from(a))
}

/// Verifies one address (account + supplied storage slots) under `state_root`.
pub async fn verify_address(
    client: &RpcClient,
    state_root: H256,
    address: H160,
    slots: &[U256],
    block: &str,
) -> Result<VerifiedAddress> {
    let params = vec![
        Value::String(format!("0x{address:x}")),
        Value::Array(slots.iter().map(|s| Value::String(format!("0x{s:x}"))).collect()),
        Value::String(block.to_string()),
    ];
    let resp = client.call("eth_getProof", params).await?;

    let proof: Vec<Vec<u8>> = resp
        .get("accountProof")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
        .iter()
        .map(|s: &String| hex_to_bytes(s))
        .collect();

    let account_leaf = verify_account_proof(state_root, address, &proof)?;

    let mut verified_slots = Vec::new();
    match account_leaf {
        Some(leaf) => {
            let (nonce, balance, storage_root, code_hash) = decode_account(&leaf)?;
            if let Some(claimed) = resp.get("storageHash").and_then(Value::as_str) {
                let claimed_b = hex_to_bytes(claimed);
                if claimed_b.len() == 32 && claimed_b != storage_root.as_bytes() {
                    return Err(eyre!(
                        "storageHash mismatch for {address}: proof={storage_root} \
                         claimed={claimed}"
                    ));
                }
            }

            let storage_proofs: Vec<(U256, Vec<Vec<u8>>)> = {
                let sp = resp.get("storageProof").cloned().unwrap_or(Value::Null);
                let arr = sp.as_array().cloned().unwrap_or_default();
                arr.iter()
                    .map(|e| {
                        let key = e.get("key").and_then(Value::as_str).unwrap_or("0x0");
                        let slot = parse_slot(key).unwrap_or(U256::zero());
                        let pf: Vec<String> = e
                            .get("proof")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                        let pf = pf.iter().map(|s| hex_to_bytes(s)).collect();
                        (slot, pf)
                    })
                    .collect()
            };

            for (slot, pf) in storage_proofs {
                let val = verify_storage_proof(storage_root, slot, &pf)?;
                verified_slots.push(VerifiedSlot {
                    slot,
                    value: val.unwrap_or(U256::zero()),
                    present: val.is_some(),
                });
            }

            Ok(VerifiedAddress {
                account: Some(VerifiedAccount {
                    address,
                    nonce,
                    balance,
                    storage_root,
                    code_hash,
                    present: true,
                }),
                slots: verified_slots,
            })
        }
        None => Ok(VerifiedAddress { account: None, slots: Vec::new() }),
    }
}

/// Resolves a block tag/number to `(height, state_root)` for an endpoint.
pub async fn resolve_block(client: &RpcClient, block: &str) -> Result<(u64, H256)> {
    let height = if block == "latest" {
        client.block_number().await?
    } else if let Ok(h) = crate::rpc::parse_u64_hex(block) {
        h
    } else {
        client.block_number().await?
    };
    let root = block_state_root(client, block).await?;
    Ok((height, root))
}

/// Default sample of major BSC mainnet contracts.
#[must_use]
pub const fn default_mainnet_addresses() -> &'static [&'static str] {
    &[
        "0xbb4CdB9CBd36B01bC983AEC51fA2d8c5b2f3f3a7", // WBNB
        "0x55d398326f99059fF775485246999027B3197955", // USDT
        "0x8AC76a51cc950d9822D68b83fE43Ad10364AE25",  // USDC
        "0x0E09FaBB73Bd3Ade0a17ECC321fD13a19e81cE82", // CAKE
        "0x10ED43C718714eb63d5aA57B78B54704E256024E", // PancakeSwap V2 Router
    ]
}

/// Parses a comma-separated list of addresses.
pub fn parse_address_list(s: &str) -> Result<Vec<H160>> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(parse_address).collect()
}

/// Parses a comma-separated list of storage slots.
pub fn parse_slot_list(s: &str) -> Result<Vec<U256>> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(parse_slot).collect()
}
