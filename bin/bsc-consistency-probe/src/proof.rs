//! Fetches `eth_getProof` from a client and cryptographically verifies it
//! against the reported state root. This is the "truth" layer on top of the
//! raw RPC.

use eyre::{Result, eyre};
use primitive_types::{H160, H256, U256};
use serde::Serialize;
use serde_json::Value;

use crate::{
    mpt::{decode_account, keccak, verify_account_proof, verify_storage_proof},
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

/// Strips a leading `0x`/`0X` prefix, if present.
fn strip_0x(s: &str) -> &str {
    s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s)
}

fn hex_to_bytes(s: &str) -> Vec<u8> {
    hex::decode(strip_0x(s)).unwrap_or_default()
}

/// The EIP-55 mixed-case checksum form of an address, without the `0x`.
///
/// The digest is taken over the lowercase hex text, so the result depends only
/// on the 20 address bytes.
fn to_checksum(addr: &H160) -> String {
    let lower = hex::encode(addr.as_bytes());
    let hash = keccak(lower.as_bytes());
    lower
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let nibble = if i % 2 == 0 { hash[i / 2] >> 4 } else { hash[i / 2] & 0x0f };
            if c.is_ascii_alphabetic() && nibble >= 8 { c.to_ascii_uppercase() } else { c }
        })
        .collect()
}

/// Whether the spelling of `s` is an acceptable way to write `addr`.
///
/// An all-lowercase or all-uppercase address carries no checksum, so both are
/// accepted verbatim. Mixed-case input is validated against EIP-55: that is
/// the whole point of the encoding, and it is what turns a transcription slip
/// into an error instead of a probe run against the wrong address.
fn has_valid_checksum(addr: &H160, s: &str) -> bool {
    let body = strip_0x(s);
    body == to_checksum(addr) ||
        body == body.to_ascii_lowercase() ||
        body == body.to_ascii_uppercase()
}

fn parse_address(s: &str) -> Result<H160> {
    let b = hex_to_bytes(s);
    if b.len() != 20 {
        return Err(eyre!("invalid address length (got {} bytes) for {s}", b.len()));
    }
    let mut a = [0u8; 20];
    a.copy_from_slice(&b);
    let addr = H160::from(a);
    if !has_valid_checksum(&addr, s) {
        return Err(eyre!(
            "address {s} fails its EIP-55 checksum; the correct spelling is 0x{}",
            to_checksum(&addr)
        ));
    }
    Ok(addr)
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
///
/// Addresses are EIP-55 checksummed so a transcription error is
/// detectable instead of silently yielding an unverifiable address.
#[must_use]
pub const fn default_mainnet_addresses() -> &'static [&'static str] {
    &[
        "0xBb4cDB9CBD36b01bc983AEC51fA2D8C5B2F3f3A7", // WBNB
        "0x55d398326f99059fF775485246999027B3197955", // USDT
        "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d", // USDC
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

#[cfg(test)]
mod tests {
    use primitive_types::H160;
    use proptest::prelude::*;
    use serde_json::{Value, json};

    use super::*;
    use crate::mock_rpc::MockRpc;

    /// A self-consistent `(block, stateRoot, eth_getProof)` capture from BNB
    /// Smart Chain mainnet, see `tests/fixtures/bsc_mainnet_proof.json`.
    const FIXTURE: &str = include_str!("../tests/fixtures/bsc_mainnet_proof.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("mainnet fixture must be valid JSON")
    }

    /// `(block, state_root, case)` for one fixture entry.
    fn fixture_case(key: &str) -> (String, H256, Value) {
        let f = fixture();
        let block = f["block"].as_str().expect("block").to_string();
        let root = parse_hash(f["state_root"].as_str().expect("state_root"));
        (block, root, f[key].clone())
    }

    fn parse_hash(s: &str) -> H256 {
        let b = hex_to_bytes(s);
        assert_eq!(b.len(), 32, "expected a 32-byte hash, got {s}");
        H256::from_slice(&b)
    }

    fn address_of(s: &str) -> H160 {
        H160::from_slice(&hex_to_bytes(s))
    }

    fn case_slots(case: &Value) -> Vec<U256> {
        let joined = case["slots"]
            .as_array()
            .expect("slots")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
        parse_slot_list(&joined).expect("fixture slots must parse")
    }

    // ------------------------------------------------------------------
    // Parsing helpers
    // ------------------------------------------------------------------

    #[test]
    fn hex_to_bytes_strips_prefix() {
        assert_eq!(hex_to_bytes("0xabcdef"), vec![0xab, 0xcd, 0xef]);
        assert_eq!(hex_to_bytes("abcdef"), vec![0xab, 0xcd, 0xef]);
    }

    #[test]
    fn hex_to_bytes_empty_and_invalid() {
        // Invalid hex decodes to empty (the helper swallows errors).
        assert_eq!(hex_to_bytes(""), Vec::<u8>::new());
        assert_eq!(hex_to_bytes("zz"), Vec::<u8>::new());
    }

    #[test]
    fn hex_to_bytes_odd_length_is_empty() {
        // An odd-length string is not valid hex and must not decode partially.
        assert_eq!(hex_to_bytes("0xabc"), Vec::<u8>::new());
    }

    #[test]
    fn parse_address_20_bytes() {
        let a = parse_address("0xBb4cDB9CBD36b01bc983AEC51fA2D8C5B2F3f3A7").unwrap();
        assert_eq!(
            a,
            H160::from([
                0xbb, 0x4c, 0xdb, 0x9c, 0xbd, 0x36, 0xb0, 0x1b, 0xc9, 0x83, 0xae, 0xc5, 0x1f, 0xa2,
                0xd8, 0xc5, 0xb2, 0xf3, 0xf3, 0xa7
            ])
        );
        assert_eq!(a.as_bytes().len(), 20);
    }

    #[test]
    fn parse_address_rejects_bad_length() {
        assert!(parse_address("0x1234").is_err());
        assert!(parse_address("").is_err());
        // 19 bytes.
        assert!(parse_address("0xbb4CdB9CBd36B01bC983AEC51fA2d8c5b2f3f3").is_err());
        // 21 bytes.
        assert!(parse_address("0xBb4cDB9CBD36b01bc983AEC51fA2D8C5B2F3f3A700").is_err());
    }

    #[test]
    fn parse_address_is_case_insensitive() {
        let lower = parse_address("0x55d398326f99059ff775485246999027b3197955").unwrap();
        let mixed = parse_address("0x55d398326f99059fF775485246999027B3197955").unwrap();
        assert_eq!(lower, mixed);
    }

    #[test]
    fn parse_address_is_the_inverse_of_hex_to_bytes() {
        let raw = [
            0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00, 0x10, 0x20, 0x30, 0x40,
        ];
        assert_eq!(parse_address(&format!("0x{}", hex::encode(raw))).unwrap(), H160(raw));
    }

    #[test]
    fn parse_slot_hex_and_decimal_like() {
        assert_eq!(parse_slot("0x0").unwrap(), U256::zero());
        assert_eq!(parse_slot("0x1").unwrap(), U256::one());
        assert_eq!(parse_slot("ff").unwrap(), U256::from(255u64));
        assert!(parse_slot("zz").is_err());
    }

    #[test]
    fn parse_slot_rejects_overflow() {
        // 33 bytes of `f` exceeds U256.
        assert!(parse_slot(&"f".repeat(66)).is_err());
    }

    #[test]
    fn parse_slot_accepts_max_u256() {
        assert_eq!(parse_slot(&format!("0x{}", "f".repeat(64))).unwrap(), U256::MAX);
    }

    #[test]
    fn parse_address_list_splits_and_trims() {
        let list = parse_address_list(
            "0xBb4cDB9CBD36b01bc983AEC51fA2D8C5B2F3f3A7, \
             0x55d398326f99059fF775485246999027B3197955",
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list[0],
            H160::from([
                0xbb, 0x4c, 0xdb, 0x9c, 0xbd, 0x36, 0xb0, 0x1b, 0xc9, 0x83, 0xae, 0xc5, 0x1f, 0xa2,
                0xd8, 0xc5, 0xb2, 0xf3, 0xf3, 0xa7
            ])
        );
    }

    #[test]
    fn parse_address_list_skips_empty_entries() {
        let list = parse_address_list(
            "0xBb4cDB9CBD36b01bc983AEC51fA2D8C5B2F3f3A7,, \
             ,0x55d398326f99059fF775485246999027B3197955",
        )
        .unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn parse_address_list_rejects_invalid() {
        assert!(parse_address_list("not-an-address").is_err());
    }

    #[test]
    fn parse_address_list_of_only_separators_is_empty() {
        assert!(parse_address_list("").unwrap().is_empty());
        assert!(parse_address_list(" , ,").unwrap().is_empty());
    }

    #[test]
    fn parse_slot_list_splits() {
        let slots = parse_slot_list("0,1,2").unwrap();
        assert_eq!(slots, vec![U256::zero(), U256::one(), U256::from(2u64)]);
    }

    #[test]
    fn parse_slot_list_of_only_separators_is_empty() {
        assert!(parse_slot_list("").unwrap().is_empty());
        assert!(parse_slot_list(" ,,").unwrap().is_empty());
    }

    // ------------------------------------------------------------------
    // Default mainnet addresses
    // ------------------------------------------------------------------

    #[test]
    fn default_addresses_all_parse() {
        let addrs = default_mainnet_addresses();
        assert_eq!(addrs.len(), 5);
        for a in addrs {
            let parsed = parse_address(a).unwrap();
            assert_eq!(parsed.as_bytes().len(), 20);
        }
    }

    /// Guards the failure mode a wrong constant hides best: a mistyped or
    /// truncated address still looks like a plausible string, it just never
    /// verifies anything. EIP-55 makes any transcription error detectable.
    #[test]
    fn default_addresses_carry_valid_eip55_checksums() {
        for a in default_mainnet_addresses() {
            let addr = parse_address(a).unwrap();
            assert_eq!(format!("0x{}", to_checksum(&addr)), *a, "bad EIP-55 checksum for {a}");
        }
    }

    /// Pins the checksum routine to the reference vectors from EIP-55 itself,
    /// so a subtly wrong implementation cannot silently bless a typo.
    #[test]
    fn to_checksum_matches_the_reference_vectors() {
        for expected in [
            "0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed",
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
            "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB",
            "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb",
        ] {
            let body = expected.strip_prefix("0x").expect("vector is 0x-prefixed");
            let addr = H160::from_slice(&hex_to_bytes(expected));
            assert_eq!(to_checksum(&addr), body, "EIP-55 vector mismatch");
        }
    }

    // ------------------------------------------------------------------
    // EIP-55 validation in the parser
    // ------------------------------------------------------------------

    const CHECKSUMMED: &str = "0x55d398326f99059fF775485246999027B3197955";

    #[test]
    fn parse_address_accepts_a_valid_checksum() {
        assert!(parse_address(CHECKSUMMED).is_ok());
    }

    #[test]
    fn parse_address_rejects_a_broken_checksum() {
        // Same 20 bytes, one hex digit changed but the casing pattern left in
        // place. Without validation this parses to a different address, and the
        // probe then reports a perfectly plausible "absent".
        let typo = "0x55d398326f99059fF775485246999027B3197956";
        let err = parse_address(typo).unwrap_err();
        assert!(err.to_string().contains("EIP-55"), "unexpected: {err}");

        // The error must name the correct spelling of what was typed, so the
        // fix is copy-pasteable, and the bytes must really differ from USDT.
        let suggested = err.to_string().rsplit("0x").next().unwrap_or_default().to_string();
        let suggested = parse_address(&format!("0x{suggested}")).unwrap();
        assert_ne!(suggested, parse_address(CHECKSUMMED).unwrap());
    }

    #[test]
    fn parse_address_rejects_a_miscased_but_intact_address() {
        // The same 20 bytes with one letter's case flipped. The address is
        // unchanged, but the checksum no longer matches and that must be an
        // error rather than a silently different reading of the address.
        assert!(parse_address(&CHECKSUMMED.to_ascii_lowercase()).is_ok(), "all-lowercase is fine");
        assert!(parse_address(&CHECKSUMMED.to_ascii_uppercase()).is_ok(), "all-uppercase is fine");
        assert!(
            parse_address("0x55d398326f99059ff775485246999027B3197955").is_err(),
            "one flipped case bit must fail"
        );
    }

    #[test]
    fn parse_address_accepts_single_case_addresses() {
        // All-lowercase and all-uppercase spellings carry no checksum at all,
        // so they must not be rejected.
        let lower = "0x55d398326f99059ff775485246999027b3197955";
        let upper = "0x55D398326F99059FF775485246999027B3197955";
        assert_eq!(parse_address(lower).unwrap(), parse_address(upper).unwrap());
        assert_eq!(parse_address(CHECKSUMMED).unwrap(), parse_address(lower).unwrap());
    }

    #[test]
    fn parse_address_accepts_addresses_without_letters() {
        // All digits: lowercase == uppercase, so there is nothing to checksum.
        let digits = "0x0000000000000000000000000000000000000123";
        assert!(parse_address(digits).is_ok());
    }

    #[test]
    fn parse_address_ignores_a_0x_prefix_case() {
        assert_eq!(
            parse_address(&format!("0x{}", &CHECKSUMMED[2..])).unwrap(),
            parse_address(CHECKSUMMED).unwrap()
        );
        assert_eq!(parse_address(&CHECKSUMMED[2..]).unwrap(), parse_address(CHECKSUMMED).unwrap());
    }

    #[test]
    fn parse_address_list_reports_a_bad_checksum() {
        let list = format!("{CHECKSUMMED},0x55d398326f99059fF775485246999027B3197956");
        let err = parse_address_list(&list).unwrap_err();
        assert!(err.to_string().contains("EIP-55"), "unexpected: {err}");
    }

    // ------------------------------------------------------------------
    // Real mainnet proofs, end to end through the RPC layer
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn verify_address_verifies_a_real_mainnet_contract() {
        let (block, root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());
        let slots = case_slots(&case);

        let server = MockRpc::start(vec![("eth_getProof", case["response"].clone())]).await;
        let client = RpcClient::new(&server.url());

        let got = verify_address(&client, root, address, &slots, &block).await.unwrap();
        let account = got.account.expect("USDT must be present under its state root");
        assert_eq!(account.address, address);
        assert!(account.present);

        let response = &case["response"];
        // Everything the client claimed must match what the proof commits to.
        assert_eq!(account.nonce, parse_slot(response["nonce"].as_str().unwrap()).unwrap());
        assert_eq!(account.balance, parse_slot(response["balance"].as_str().unwrap()).unwrap());
        assert_eq!(account.code_hash, parse_hash(response["codeHash"].as_str().unwrap()));
        assert_eq!(account.storage_root, parse_hash(response["storageHash"].as_str().unwrap()));

        // Every slot the client reported must be verified, not merely echoed.
        let claimed = response["storageProof"].as_array().expect("storageProof");
        assert_eq!(got.slots.len(), claimed.len());
        for verified in &got.slots {
            let entry = claimed
                .iter()
                .find(|e| e["key"].as_str() == Some(&format!("0x{:x}", verified.slot)))
                .expect("verified slot must be one the client reported");
            assert_eq!(
                verified.value,
                parse_slot(entry["value"].as_str().unwrap()).unwrap(),
                "slot {} value mismatch",
                verified.slot
            );
        }
    }

    #[tokio::test]
    async fn verify_address_sends_the_requested_scope() {
        let (block, root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());

        let server = MockRpc::start(vec![("eth_getProof", case["response"].clone())]).await;
        let client = RpcClient::new(&server.url());
        verify_address(&client, root, address, &[U256::zero(), U256::one()], &block).await.unwrap();

        let req = server.only_request();
        assert_eq!(req["method"], "eth_getProof");
        assert_eq!(req["params"][0], format!("0x{address:x}"));
        assert_eq!(req["params"][1], json!(["0x0", "0x1"]));
        assert_eq!(req["params"][2], block);
    }

    #[tokio::test]
    async fn verify_address_handles_a_real_empty_storage_trie() {
        // The case a naive verifier rejects outright: the storage root is
        // EMPTY_ROOT and the only proof node is the empty node 0x80.
        let (block, root, case) = fixture_case("empty_storage");
        let address = address_of(case["address"].as_str().unwrap());

        let server = MockRpc::start(vec![("eth_getProof", case["response"].clone())]).await;
        let client = RpcClient::new(&server.url());

        let got = verify_address(&client, root, address, &[U256::zero()], &block).await.unwrap();
        let account = got.account.expect("account must be present");
        assert_eq!(account.storage_root, crate::mpt::EMPTY_ROOT);
        assert_eq!(account.code_hash, crate::mpt::EMPTY_CODE_HASH);

        assert_eq!(got.slots.len(), 1);
        assert!(!got.slots[0].present, "an empty trie cannot hold slot 0");
        assert_eq!(got.slots[0].value, U256::zero());
    }

    #[tokio::test]
    async fn verify_address_fails_under_a_foreign_state_root() {
        let (block, _root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());

        let server = MockRpc::start(vec![("eth_getProof", case["response"].clone())]).await;
        let client = RpcClient::new(&server.url());

        // The same proof under a different claimed root: nothing may verify.
        let err = verify_address(&client, H256::repeat_byte(0x7f), address, &[], &block)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("root node not present"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn verify_address_rejects_a_lying_storage_hash() {
        let (block, root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());

        // The proof still commits to the real storage root, but the client
        // claims a different one. The proof must win.
        let mut tampered = case["response"].clone();
        tampered["storageHash"] = Value::String(format!("0x{}", "ab".repeat(32)));
        let server = MockRpc::start(vec![("eth_getProof", tampered)]).await;
        let client = RpcClient::new(&server.url());

        let err = verify_address(&client, root, address, &[], &block).await.unwrap_err();
        assert!(err.to_string().contains("storageHash mismatch"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn verify_address_tolerates_a_short_storage_hash() {
        let (block, root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());

        // A malformed `storageHash` is skipped rather than treated as a lie.
        let mut response = case["response"].clone();
        response["storageHash"] = Value::String("0xdeadbeef".into());
        let server = MockRpc::start(vec![("eth_getProof", response)]).await;
        let client = RpcClient::new(&server.url());

        assert!(verify_address(&client, root, address, &[], &block).await.is_ok());
    }

    #[tokio::test]
    async fn verify_address_reports_an_unverifiable_account_not_a_false_pass() {
        let (block, root, case) = fixture_case("present");
        let mut response = case["response"].clone();
        // Drop a proof node so the account leaf can no longer be reached.
        response["accountProof"] = json!(["0xdeadbeef"]);
        let server = MockRpc::start(vec![("eth_getProof", response)]).await;
        let client = RpcClient::new(&server.url());

        let address = address_of(case["address"].as_str().unwrap());
        assert!(verify_address(&client, root, address, &[], &block).await.is_err());
    }

    // ------------------------------------------------------------------
    // resolve_block / block_state_root
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn resolve_block_reads_head_and_state_root() {
        let f = fixture();
        let server = MockRpc::start(vec![
            ("eth_blockNumber", Value::String(f["block"].as_str().unwrap().into())),
            ("eth_getBlockByNumber", json!({ "stateRoot": f["state_root"] })),
        ])
        .await;
        let client = RpcClient::new(&server.url());

        let (height, root) = resolve_block(&client, "latest").await.unwrap();
        assert_eq!(format!("0x{height:x}"), f["block"].as_str().unwrap());
        assert_eq!(root.as_bytes().to_vec(), hex_to_bytes(f["state_root"].as_str().unwrap()));
    }

    #[tokio::test]
    async fn resolve_block_accepts_an_explicit_height_without_querying_the_head() {
        let f = fixture();
        let block = f["block"].as_str().unwrap();
        let server =
            MockRpc::start(vec![("eth_getBlockByNumber", json!({ "stateRoot": f["state_root"] }))])
                .await;
        let client = RpcClient::new(&server.url());

        let (height, _) = resolve_block(&client, block).await.unwrap();
        assert_eq!(format!("0x{height:x}"), block);
        assert!(
            server.requests().iter().all(|r| r["method"] == "eth_getBlockByNumber"),
            "an explicit height must not need eth_blockNumber"
        );
    }

    #[tokio::test]
    async fn resolve_block_falls_back_to_head_for_non_hex_tags() {
        let f = fixture();
        let server = MockRpc::start(vec![
            ("eth_blockNumber", Value::String(f["block"].as_str().unwrap().into())),
            ("eth_getBlockByNumber", json!({ "stateRoot": f["state_root"] })),
        ])
        .await;
        let client = RpcClient::new(&server.url());

        // `pending` and friends are not hex, so the head is used.
        let (height, _) = resolve_block(&client, "pending").await.unwrap();
        assert_eq!(format!("0x{height:x}"), f["block"].as_str().unwrap());
    }

    #[tokio::test]
    async fn block_state_root_rejects_missing_field() {
        let server =
            MockRpc::start(vec![("eth_getBlockByNumber", json!({ "number": "0x1" }))]).await;
        let client = RpcClient::new(&server.url());

        let err = block_state_root(&client, "latest").await.unwrap_err();
        assert!(err.to_string().contains("missing stateRoot"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn block_state_root_rejects_wrong_length_root() {
        let server =
            MockRpc::start(vec![("eth_getBlockByNumber", json!({ "stateRoot": "0xab" }))]).await;
        let client = RpcClient::new(&server.url());

        let err = block_state_root(&client, "latest").await.unwrap_err();
        assert!(err.to_string().contains("bad stateRoot length"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn block_state_root_errors_on_null_block() {
        let server = MockRpc::start(vec![("eth_getBlockByNumber", Value::Null)]).await;
        let client = RpcClient::new(&server.url());

        assert!(block_state_root(&client, "999999999").await.is_err());
    }

    // ------------------------------------------------------------------
    // Trie entry points, exercised against the captured mainnet nodes
    // ------------------------------------------------------------------

    #[test]
    fn real_account_proof_verifies_under_its_own_root() {
        let (_block, root, case) = fixture_case("present");
        let address = address_of(case["address"].as_str().unwrap());
        let nodes: Vec<Vec<u8>> = case["response"]["accountProof"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| hex_to_bytes(n.as_str().unwrap()))
            .collect();

        assert_eq!(nodes.len(), 10, "fixture shape changed");
        assert!(crate::mpt::verify_account_proof(root, address, &nodes).unwrap().is_some());
    }

    #[test]
    fn real_account_proof_rejects_a_foreign_address() {
        let (_block, root, case) = fixture_case("present");
        let nodes: Vec<Vec<u8>> = case["response"]["accountProof"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| hex_to_bytes(n.as_str().unwrap()))
            .collect();

        // The proof is genuine but commits to USDT. For an unrelated address
        // the node set may not even reach a verdict, but it must never hand
        // back USDT's account leaf.
        let other = H160::repeat_byte(0x99);
        let got = crate::mpt::verify_account_proof(root, other, &nodes);
        assert!(
            !matches!(got, Ok(Some(_))),
            "a proof for USDT must not verify a different address"
        );
    }

    #[test]
    fn absent_root_errors_without_proof_nodes() {
        // A proof with no nodes cannot contain the claimed root => error.
        let root = H256::repeat_byte(0x11);
        assert!(crate::mpt::verify_account_proof(root, H160::zero(), &[]).is_err());
    }

    #[test]
    fn empty_storage_proof_is_absent() {
        // An empty trie is committed to by its root alone, so a missing proof
        // is still a complete proof that the slot does not exist.
        let res =
            crate::mpt::verify_storage_proof(crate::mpt::EMPTY_ROOT, U256::zero(), &[]).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn empty_storage_proof_is_absent_under_an_unrelated_root() {
        // Only the empty root commits to emptiness; any other root needs nodes.
        assert!(
            crate::mpt::verify_storage_proof(H256::repeat_byte(0x22), U256::zero(), &[]).is_err()
        );
    }

    // ------------------------------------------------------------------
    // Properties
    // ------------------------------------------------------------------

    proptest! {
        /// Whatever 20 bytes go in, formatting them back must parse.
        #[test]
        fn parse_address_roundtrips(raw in any::<[u8; 20]>()) {
            let hex = format!("0x{}", hex::encode(raw));
            prop_assert_eq!(parse_address(&hex).unwrap(), H160(raw));
        }

        /// Only well-formed 20-byte hex may be accepted as an address.
        #[test]
        fn parse_address_rejects_wrong_lengths(len in 0usize..40) {
            prop_assume!(len != 20); // 20 bytes is the only accepted length
            let candidate = format!("0x{}", "ab".repeat(len));
            prop_assert!(parse_address(&candidate).is_err());
        }

        /// Slot parsing must survive the full U256 range, prefixed or not.
        #[test]
        fn parse_slot_roundtrips(value in any::<[u8; 32]>()) {
            let v = U256::from_big_endian(&value);
            let bare = hex::encode(value);
            prop_assert_eq!(parse_slot(&format!("0x{bare}")).unwrap(), v);
            prop_assert_eq!(parse_slot(&bare).unwrap(), v);
        }

        /// Splitting on commas must never change how many slots are parsed.
        #[test]
        fn parse_slot_list_ignores_separators(
            slots in proptest::collection::vec(0u64..=u64::MAX, 1..8),
        ) {
            let joined = slots.iter().map(ToString::to_string).collect::<Vec<_>>().join(" , ");
            prop_assert_eq!(parse_slot_list(&joined).unwrap().len(), slots.len());
        }

        /// Address lists must keep their order, which the caller relies on to
        /// line results up with the addresses it asked about.
        #[test]
        fn parse_address_list_preserves_order(raw in proptest::collection::vec(any::<[u8; 20]>(), 1..6)) {
            let joined = raw.iter().map(|r| format!("0x{}", hex::encode(r))).collect::<Vec<_>>().join(",");
            let parsed = parse_address_list(&joined).unwrap();
            prop_assert_eq!(parsed.len(), raw.len());
            for (got, want) in parsed.iter().zip(raw.iter()) {
                prop_assert_eq!(got, &H160(*want));
            }
        }
    }
}
