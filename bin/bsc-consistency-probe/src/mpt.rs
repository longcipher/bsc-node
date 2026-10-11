//! Standalone Ethereum MPT (Merkle Patricia Trie) proof verifier.
//!
//! Used by the "truth" mode: given a client's reported `stateRoot` and an
//! `eth_getProof` account/storage proof, we cryptographically verify that the
//! returned account/storage data is genuinely committed under that root.
//!
//! A client can lie about arbitrary state, but it cannot produce a valid proof
//! for data that is not under the claimed root (every trie node's keccak binds
//! it to its parent). So a successful verification means the client is not
//! fabricating this slice of state. Combined with two-client agreement, that is
//! strong evidence the state root is truthful.

use std::collections::HashMap;

use eyre::{Result, eyre};
use primitive_types::H256;
use rlp::Rlp;
use sha3::{Digest, Keccak256};

/// Keccak-256 digest of `bytes`.
#[must_use]
pub fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut a = [0u8; 32];
    a.copy_from_slice(&out);
    a
}

fn bytes_to_nibbles(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|b| [b >> 4, b & 0x0f]).collect()
}

/// Compact (hex-prefix) decoding of an MPT path.
///
/// Returns `(is_leaf, nibble_path)`.
fn decode_path(path: &[u8]) -> (bool, Vec<u8>) {
    let first = path[0];
    let flag = first >> 4; // high nibble carries the 2-bit prefix flag
    let is_leaf = (flag & 0x2) != 0;
    let is_odd = (flag & 0x1) != 0;
    let mut nibbles = Vec::new();
    if is_odd {
        // low nibble of the first byte is the first key nibble
        nibbles.push(first & 0x0f);
    }
    for &b in &path[1..] {
        nibbles.push(b >> 4);
        nibbles.push(b & 0x0f);
    }
    (is_leaf, nibbles)
}

#[derive(Debug, Clone)]
enum ChildRef {
    None,
    Hash([u8; 32]),
    Inline(Vec<u8>),
}

#[derive(Debug)]
enum Node {
    Empty,
    Leaf(Vec<u8>, Vec<u8>),
    Extension(Vec<u8>, ChildRef),
    Branch(Box<[ChildRef; 16]>, Option<Vec<u8>>),
}

fn decode_child_ref(bytes: &[u8]) -> ChildRef {
    if bytes.is_empty() {
        ChildRef::None
    } else if bytes.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(bytes);
        ChildRef::Hash(a)
    } else {
        ChildRef::Inline(bytes.to_vec())
    }
}

fn decode_node(rlp: &Rlp<'_>) -> Result<Node> {
    // A valid MPT node is a list, or the empty node (RLP 0x80, a bare empty string).
    if rlp.is_data() {
        return Ok(Node::Empty);
    }
    let count = rlp.item_count()?;
    if count == 2 {
        let path_bytes = rlp.at(0)?.data()?.to_vec();
        let val = rlp.at(1)?.data()?.to_vec();
        let (is_leaf, nibbles) = decode_path(&path_bytes);
        if is_leaf {
            Ok(Node::Leaf(nibbles, val))
        } else {
            Ok(Node::Extension(nibbles, decode_child_ref(&val)))
        }
    } else if count == 17 {
        let mut children = Box::new([(); 16].map(|()| ChildRef::None));
        for (i, slot) in children.iter_mut().enumerate() {
            *slot = decode_child_ref(rlp.at(i)?.data()?);
        }
        let v = rlp.at(16)?;
        let val = if v.is_data() {
            let d = v.data()?;
            if d.is_empty() { None } else { Some(d.to_vec()) }
        } else {
            None
        };
        Ok(Node::Branch(children, val))
    } else {
        Err(eyre!("invalid MPT node list length {count}"))
    }
}

fn resolve_child(child: &ChildRef, map: &HashMap<H256, Vec<u8>>) -> Result<Vec<u8>> {
    match child {
        ChildRef::None => Err(eyre!("branch/ext points to empty child")),
        ChildRef::Hash(h) => {
            let key = H256::from(*h);
            map.get(&key).cloned().ok_or_else(|| eyre!("missing proof node for child hash {key}"))
        }
        ChildRef::Inline(b) => Ok(b.clone()),
    }
}

/// Verifies `node_bytes` hashes to `expected`, then descends the trie.
fn verify_node_bytes(
    node_bytes: &[u8],
    expected: &H256,
    path: &[u8],
    map: &HashMap<H256, Vec<u8>>,
) -> Result<Option<Vec<u8>>> {
    let computed = H256::from(keccak(node_bytes));
    if computed != *expected {
        return Err(eyre!("trie node hash mismatch: computed {computed} expected {expected}"));
    }
    let rlp = Rlp::new(node_bytes);
    let node = decode_node(&rlp)?;
    descend(node, path, map)
}

fn descend(node: Node, path: &[u8], map: &HashMap<H256, Vec<u8>>) -> Result<Option<Vec<u8>>> {
    match node {
        Node::Empty => Ok(None),
        Node::Leaf(p, val) => {
            if p == path {
                Ok(Some(val))
            } else {
                // Path diverges => key is provably absent under this root.
                Ok(None)
            }
        }
        Node::Extension(p, child) => {
            if !path.starts_with(&p) {
                return Ok(None); // diverges => absent
            }
            let cb = resolve_child(&child, map)?;
            let exp = H256::from(keccak(&cb));
            verify_node_bytes(&cb, &exp, &path[p.len()..], map)
        }
        Node::Branch(children, val) => {
            if path.is_empty() {
                return Ok(val);
            }
            match &children[path[0] as usize] {
                ChildRef::None => Ok(None),
                other => {
                    let cb = resolve_child(other, map)?;
                    let exp = H256::from(keccak(&cb));
                    verify_node_bytes(&cb, &exp, &path[1..], map)
                }
            }
        }
    }
}

/// Verifies a proof (list of RLP trie nodes) against `root`, for `key_nibbles`.
///
/// Returns the leaf value if the key is present, or `None` if provably absent.
pub fn verify_proof(root: H256, key_nibbles: &[u8], proof: &[Vec<u8>]) -> Result<Option<Vec<u8>>> {
    let mut map = HashMap::new();
    for n in proof {
        map.insert(H256::from(keccak(n)), n.clone());
    }
    match map.get(&root) {
        Some(node_bytes) => verify_node_bytes(node_bytes, &root, key_nibbles, &map),
        None => Err(eyre!("root node not present in proof (claimed root {root})")),
    }
}

/// Verifies an account proof under the block's `stateRoot`.
pub fn verify_account_proof(
    state_root: H256,
    address: primitive_types::H160,
    proof: &[Vec<u8>],
) -> Result<Option<Vec<u8>>> {
    let key = keccak(address.as_bytes());
    let nibbles = bytes_to_nibbles(&key);
    verify_proof(state_root, &nibbles, proof)
}

/// Verifies a storage slot proof under the account's `storage_root`.
pub fn verify_storage_proof(
    storage_root: H256,
    slot: primitive_types::U256,
    proof: &[Vec<u8>],
) -> Result<Option<primitive_types::U256>> {
    let slot_bytes = slot.to_big_endian();
    let key = keccak(&slot_bytes);
    let nibbles = bytes_to_nibbles(&key);
    let leaf = verify_proof(storage_root, &nibbles, proof)?;
    match leaf {
        Some(v) => {
            let rlp = Rlp::new(&v);
            let val: primitive_types::U256 = rlp.as_val()?;
            Ok(Some(val))
        }
        None => Ok(None),
    }
}

/// Decodes an RLP-encoded account leaf `[nonce, balance, storageRoot, codeHash]`.
pub fn decode_account(
    val: &[u8],
) -> Result<(primitive_types::U256, primitive_types::U256, H256, H256)> {
    let rlp = Rlp::new(val);
    let nonce: primitive_types::U256 = rlp.at(0)?.as_val()?;
    let balance: primitive_types::U256 = rlp.at(1)?.as_val()?;
    let storage_root: H256 = rlp.at(2)?.as_val()?;
    let code_hash: H256 = rlp.at(3)?.as_val()?;
    Ok((nonce, balance, storage_root, code_hash))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn nibble_roundtrip() {
        let bytes = [0xabu8, 0xcd];
        let n = bytes_to_nibbles(&bytes);
        assert_eq!(n, vec![0xa, 0xb, 0xc, 0xd]);
    }

    #[test]
    fn decode_path_even_extension() {
        // flag nibble 0x0 => extension, even => key nibbles from next bytes
        let p = [0x00u8, 0x12, 0x34];
        let (is_leaf, n) = decode_path(&p);
        assert!(!is_leaf);
        assert_eq!(n, vec![0x1, 0x2, 0x3, 0x4]);
    }

    #[test]
    fn decode_path_odd_leaf() {
        // flag nibble 0x3 => leaf, odd => first key nibble is low nibble of 0x31 (0x1),
        // then 0x23 -> nibbles 0x2, 0x3
        let p = [0x31u8, 0x23];
        let (is_leaf, n) = decode_path(&p);
        assert!(is_leaf);
        assert_eq!(n, vec![0x1, 0x2, 0x3]);
    }

    proptest! {
        #[test]
        fn bytes_to_nibbles_expands_evenly(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let n = bytes_to_nibbles(&bytes);
            prop_assert_eq!(n.len(), bytes.len() * 2);
            for (i, &b) in bytes.iter().enumerate() {
                prop_assert_eq!(n[i * 2], b >> 4);
                prop_assert_eq!(n[i * 2 + 1], b & 0x0f);
            }
        }
    }
}
