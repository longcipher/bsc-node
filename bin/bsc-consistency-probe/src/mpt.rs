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

/// Root of an empty trie: `keccak256(rlp(""))`.
///
/// A root is the commitment to the whole trie, so a state or storage root equal
/// to this value proves on its own that the trie holds no keys. Callers can
/// treat absence as proven without any proof nodes.
pub const EMPTY_ROOT: H256 = H256([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

/// Hash of empty contract code: `keccak256(b"")`.
///
/// This is what an EOA or non-contract account carries, so the tests build
/// account leaves with it. The probe never compares against it at runtime,
/// which is why it is test-only rather than dead weight in the binary.
#[cfg(test)]
pub const EMPTY_CODE_HASH: H256 = H256([
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
]);

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
/// Returns `(is_leaf, nibble_path)`. An empty path is rejected rather than
/// indexed: a real leaf/extension node always carries at least one nibble, and
/// a client-supplied node is untrusted input, so a corrupt node must not panic
/// the verifier.
fn decode_path(path: &[u8]) -> Result<(bool, Vec<u8>)> {
    let Some(&first) = path.first() else {
        return Err(eyre!("MPT node has an empty hex-prefix path"));
    };
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
    Ok((is_leaf, nibbles))
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

fn decode_child_ref(item: &Rlp<'_>) -> Result<ChildRef> {
    // A child slot is either an empty string, a bare 32-byte hash, or the
    // node's own encoding inlined when it is shorter than 32 bytes. An
    // inlined child is itself an RLP node, so it arrives here as a nested
    // list rather than a string and must be kept in its encoded form.
    if !item.is_data() {
        return Ok(ChildRef::Inline(item.as_raw().to_vec()));
    }
    let bytes = item.data()?;
    Ok(if bytes.is_empty() {
        ChildRef::None
    } else if bytes.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(bytes);
        ChildRef::Hash(a)
    } else {
        ChildRef::Inline(bytes.to_vec())
    })
}

fn decode_node(rlp: &Rlp<'_>) -> Result<Node> {
    // A valid MPT node is a list, or the empty node (RLP 0x80, a bare empty string).
    if rlp.is_data() {
        return Ok(Node::Empty);
    }
    let count = rlp.item_count()?;
    if count == 2 {
        let path_bytes = rlp.at(0)?.data()?.to_vec();
        let (is_leaf, nibbles) = decode_path(&path_bytes)?;
        if is_leaf {
            Ok(Node::Leaf(nibbles, rlp.at(1)?.data()?.to_vec()))
        } else {
            Ok(Node::Extension(nibbles, decode_child_ref(&rlp.at(1)?)?))
        }
    } else if count == 17 {
        let mut children = Box::new([(); 16].map(|()| ChildRef::None));
        for (i, slot) in children.iter_mut().enumerate() {
            *slot = decode_child_ref(&rlp.at(i)?)?;
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
    // A trie rooted at EMPTY_ROOT is empty by construction, so the key is
    // provably absent no matter what (or whether anything) the client sent as
    // a proof. Rejecting here would misreport a valid absence as a failure.
    if root == EMPTY_ROOT {
        return Ok(None);
    }

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
    use primitive_types::{H160, U256};
    use proptest::prelude::*;

    use super::*;

    // ------------------------------------------------------------------
    // Trie builders.
    //
    // The verifier is only meaningful if it is fed well-formed tries *and*
    // deliberately malformed ones, so the tests construct nodes directly
    // rather than depending on a node implementation to produce them.
    //
    // These helpers hand-roll RLP because `rlp`'s `Encodable for Vec<u8>`
    // encodes a *list of integers*, whereas MPT nodes need byte strings.
    // ------------------------------------------------------------------

    /// RLP length-of-length prefix for payloads longer than 55 bytes.
    fn rlp_len_prefix(len: usize, short_tag: u8) -> Vec<u8> {
        let bytes = len.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len() - 1);
        let len_bytes = &bytes[first..];
        let mut out = vec![short_tag + len_bytes.len() as u8];
        out.extend_from_slice(len_bytes);
        out
    }

    /// RLP-encodes a byte string.
    fn rlp_string(bytes: &[u8]) -> Vec<u8> {
        if bytes.len() == 1 && bytes[0] < 0x80 {
            return bytes.to_vec();
        }
        let mut out = if bytes.len() <= 55 {
            vec![0x80 + bytes.len() as u8]
        } else {
            rlp_len_prefix(bytes.len(), 0xb7)
        };
        out.extend_from_slice(bytes);
        out
    }

    /// RLP-encodes a list from already-encoded items.
    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let payload = items.concat();
        let mut out = if payload.len() <= 55 {
            vec![0xc0 + payload.len() as u8]
        } else {
            rlp_len_prefix(payload.len(), 0xf7)
        };
        out.extend_from_slice(&payload);
        out
    }

    /// Big-endian bytes with leading zeros stripped, which is the canonical
    /// RLP form for an integer (`U256::decode` rejects leading zeros).
    fn minimal_be(bytes: &[u8]) -> Vec<u8> {
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
        bytes[first..].to_vec()
    }

    /// Hex-prefix (compact) encoding: the inverse of [`decode_path`].
    fn encode_path(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
        let flag: u8 = if is_leaf { 2 } else { 0 };
        let mut out = Vec::new();
        if nibbles.len() % 2 == 1 {
            // Odd length: the first nibble rides in the low half of the flag
            // byte, and the remaining nibbles pack two per byte.
            out.push(((flag | 1) << 4) | nibbles[0]);
            out.extend(nibbles[1..].chunks(2).map(|c| (c[0] << 4) | c[1]));
        } else {
            out.push(flag << 4);
            out.extend(nibbles.chunks(2).map(|c| (c[0] << 4) | c[1]));
        }
        out
    }

    /// RLP-encoded child slot content per the spec: an empty slot is the empty
    /// string, a child whose node is 32 bytes or larger is referenced by a
    /// 32-byte hash string, and anything smaller is inlined as the node's own
    /// (already RLP-encoded) bytes.
    fn child_ref(node: &[u8]) -> Vec<u8> {
        if node.is_empty() {
            vec![0x80]
        } else if node.len() < 32 {
            node.to_vec()
        } else {
            rlp_string(&keccak(node))
        }
    }

    fn leaf_node(nibbles: &[u8], value: &[u8]) -> Vec<u8> {
        rlp_list(&[rlp_string(&encode_path(nibbles, true)), rlp_string(value)])
    }

    fn extension_node(nibbles: &[u8], child: &[u8]) -> Vec<u8> {
        rlp_list(&[rlp_string(&encode_path(nibbles, false)), child_ref(child)])
    }

    /// Builds a branch node; `None` child slots and `None` value encode as
    /// the empty string, matching the spec.
    fn branch_node(children: [&[u8]; 16], value: Option<&[u8]>) -> Vec<u8> {
        let mut chunks: Vec<Vec<u8>> = children.iter().map(|c| child_ref(c)).collect();
        chunks.push(rlp_string(value.unwrap_or(&[])));
        rlp_list(&chunks)
    }

    /// Bytes a storage trie stores for `value`: the RLP encoding of the integer,
    /// which the leaf node then wraps as its own byte string.
    fn storage_value(value: U256) -> Vec<u8> {
        rlp_string(&minimal_be(&value.to_big_endian()))
    }

    fn hash_of(node: &[u8]) -> H256 {
        H256::from(keccak(node))
    }

    /// Nibble path for an account address, matching [`verify_account_proof`].
    fn account_nibbles(addr: H160) -> Vec<u8> {
        bytes_to_nibbles(&keccak(addr.as_bytes()))
    }

    /// Nibble path for a storage slot, matching [`verify_storage_proof`].
    fn slot_nibbles(slot: U256) -> Vec<u8> {
        bytes_to_nibbles(&keccak(&slot.to_big_endian()))
    }

    // ------------------------------------------------------------------
    // keccak / constants
    // ------------------------------------------------------------------

    #[test]
    fn keccak_known_vectors() {
        // Canonical Ethereum keccak-256 values. Both constants below were
        // independently cross-checked against live BSC mainnet
        // `eth_getProof` responses (empty `codeHash` and empty `storageHash`).
        assert_eq!(
            hex::encode(keccak(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
        assert_eq!(H256(keccak(b"")), EMPTY_CODE_HASH);
        // keccak256(rlp("")) == keccak256([0x80]) is the empty-trie root.
        assert_eq!(H256(keccak(&[0x80])), EMPTY_ROOT);
    }

    #[test]
    fn empty_root_and_empty_code_hash_are_distinct() {
        assert_ne!(EMPTY_ROOT, EMPTY_CODE_HASH);
    }

    // ------------------------------------------------------------------
    // bytes_to_nibbles / decode_path
    // ------------------------------------------------------------------

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
        let (is_leaf, n) = decode_path(&p).unwrap();
        assert!(!is_leaf);
        assert_eq!(n, vec![0x1, 0x2, 0x3, 0x4]);
    }

    #[test]
    fn decode_path_odd_leaf() {
        // flag nibble 0x3 => leaf, odd => first key nibble is low nibble of 0x31 (0x1),
        // then 0x23 -> nibbles 0x2, 0x3
        let p = [0x31u8, 0x23];
        let (is_leaf, n) = decode_path(&p).unwrap();
        assert!(is_leaf);
        assert_eq!(n, vec![0x1, 0x2, 0x3]);
    }

    #[test]
    fn decode_path_even_leaf_and_odd_extension() {
        // 0x2 => leaf, even length.
        let (is_leaf, n) = decode_path(&[0x20, 0xab]).unwrap();
        assert!(is_leaf);
        assert_eq!(n, vec![0xa, 0xb]);

        // 0x1 => extension, odd length; the low nibble is the first key nibble.
        let (is_leaf, n) = decode_path(&[0x1f]).unwrap();
        assert!(!is_leaf);
        assert_eq!(n, vec![0xf]);
    }

    #[test]
    fn decode_path_rejects_empty_path() {
        // Untrusted nodes must not panic the verifier on a malformed path.
        assert!(decode_path(&[]).is_err());
    }

    // ------------------------------------------------------------------
    // decode_child_ref / decode_node
    // ------------------------------------------------------------------

    #[test]
    fn decode_child_ref_kinds() {
        assert!(matches!(decode_child_ref(&Rlp::new(&[0x80])).unwrap(), ChildRef::None));
        assert!(matches!(
            decode_child_ref(&Rlp::new(&rlp_string(&[0x01, 0x02]))).unwrap(),
            ChildRef::Inline(_)
        ));
        let h = decode_child_ref(&Rlp::new(&rlp_string(&[0x11; 32]))).unwrap();
        assert!(matches!(h, ChildRef::Hash(a) if a == [0x11; 32]));
    }

    #[test]
    fn decode_child_ref_keeps_inline_node_encoded() {
        // An inlined child is a nested RLP node; keeping only its payload
        // would silently drop the list header and corrupt the hash chain.
        let node = leaf_node(&[0x1], b"v");
        let inline = decode_child_ref(&Rlp::new(&node)).unwrap();
        assert!(matches!(inline, ChildRef::Inline(ref v) if v == &node));
    }

    #[test]
    fn decode_node_empty_is_data() {
        assert!(matches!(decode_node(&Rlp::new(&[0x80])).unwrap(), Node::Empty));
    }

    #[test]
    fn decode_node_leaf_and_extension() {
        let leaf = leaf_node(&[0x1, 0x2, 0x3], b"v");
        let Node::Leaf(nibbles, value) = decode_node(&Rlp::new(&leaf)).unwrap() else {
            panic!("expected a leaf node");
        };
        assert_eq!(nibbles, vec![0x1, 0x2, 0x3]);
        assert_eq!(value, b"v".to_vec());

        let child = leaf_node(&[0x9], b"c");
        let ext = extension_node(&[0x4, 0x5], &child);
        let Node::Extension(nibbles, child_ref) = decode_node(&Rlp::new(&ext)).unwrap() else {
            panic!("expected an extension node");
        };
        assert_eq!(nibbles, vec![0x4, 0x5]);
        assert!(matches!(child_ref, ChildRef::Inline(ref v) if v == &child));
    }

    #[test]
    fn decode_node_extension_child_is_hashed_when_large() {
        let big = vec![0x5a; 40];
        let ext = extension_node(&[0x1], &big);
        let Node::Extension(_, child) = decode_node(&Rlp::new(&ext)).unwrap() else {
            panic!("expected an extension node");
        };
        assert!(matches!(child, ChildRef::Hash(a) if a == keccak(&big)));
    }

    #[test]
    fn decode_node_branch_slots_and_value() {
        let small = leaf_node(&[0x1], b"s");
        let large = vec![0x5a; 40];
        let mut children: [&[u8]; 16] = [&[]; 16];
        children[2] = &small; // under 32 bytes => inlined
        children[9] = &large; // 32 bytes or more => hashed
        let branch = branch_node(children, Some(b"here"));

        let Node::Branch(slots, value) = decode_node(&Rlp::new(&branch)).unwrap() else {
            panic!("expected a branch node");
        };
        assert!(matches!(slots[2], ChildRef::Inline(ref v) if v == &small));
        assert!(matches!(slots[9], ChildRef::Hash(ref h) if *h == keccak(&large)));
        assert!(matches!(slots[5], ChildRef::None));
        assert_eq!(value, Some(b"here".to_vec()));

        // A branch with no value of its own reports `None`, not an empty vec.
        let branch = branch_node([&[]; 16], None);
        let Node::Branch(_, value) = decode_node(&Rlp::new(&branch)).unwrap() else {
            panic!("expected a branch node");
        };
        assert_eq!(value, None);
    }

    #[test]
    fn decode_node_rejects_bad_arity() {
        // A 3-item list is neither a leaf/extension (2) nor a branch (17).
        let node = rlp_list(&[rlp_string(&[1]), rlp_string(&[2]), rlp_string(&[3])]);
        assert!(decode_node(&Rlp::new(&node)).is_err());
    }

    // ------------------------------------------------------------------
    // verify_proof: single leaf
    // ------------------------------------------------------------------

    #[test]
    fn verify_proof_single_leaf_present() {
        let key = bytes_to_nibbles(&keccak(b"alpha"));
        let leaf = leaf_node(&key, b"payload");
        let root = hash_of(&leaf);

        let got = verify_proof(root, &key, &[leaf]).unwrap();
        assert_eq!(got, Some(b"payload".to_vec()));
    }

    #[test]
    fn verify_proof_single_leaf_diverging_key_is_absent() {
        let key = bytes_to_nibbles(&keccak(b"alpha"));
        let mut other = bytes_to_nibbles(&keccak(b"beta"));
        other[63] = (other[63] + 1) % 16;
        let leaf = leaf_node(&key, b"payload");

        // A different final nibble => the key cannot be in this one-leaf trie.
        assert_eq!(verify_proof(hash_of(&leaf), &other, &[leaf]).unwrap(), None);
    }

    #[test]
    fn verify_proof_key_is_prefix_of_leaf_path_is_absent() {
        let key = bytes_to_nibbles(&keccak(b"alpha"));
        let leaf = leaf_node(&key, b"payload");

        // A shorter path can only terminate on a branch node, never on this
        // leaf, so the key must be reported absent rather than matched.
        assert_eq!(verify_proof(hash_of(&leaf), &key[..4], &[leaf]).unwrap(), None);
    }

    // ------------------------------------------------------------------
    // verify_proof: branch
    // ------------------------------------------------------------------

    #[test]
    fn verify_proof_branch_descends_into_child() {
        let key = bytes_to_nibbles(&keccak(b"gamma"));
        let leaf = leaf_node(&key[1..], b"under-branch");
        let mut children: [&[u8]; 16] = [&[]; 16];
        children[key[0] as usize] = &leaf;
        let branch = branch_node(children, None);

        assert_eq!(
            verify_proof(hash_of(&branch), &key, &[branch, leaf]).unwrap(),
            Some(b"under-branch".to_vec())
        );
    }

    #[test]
    fn verify_proof_branch_missing_slot_is_absent() {
        let key = bytes_to_nibbles(&keccak(b"gamma"));
        let leaf = leaf_node(&key[1..], b"under-branch");
        let mut children: [&[u8]; 16] = [&[]; 16];
        children[key[0] as usize] = &leaf;
        let branch = branch_node(children, None);

        // Same trie, but the key routes to a slot the branch does not fill.
        let mut other = key.clone();
        other[0] = (other[0] + 1) % 16;
        assert_eq!(verify_proof(hash_of(&branch), &other, &[branch]).unwrap(), None);
    }

    #[test]
    fn verify_proof_branch_own_value_is_returned() {
        let branch = branch_node([&[]; 16], Some(b"at-the-branch"));

        // The key that ends exactly at the branch node lives in slot 16.
        assert_eq!(
            verify_proof(hash_of(&branch), &[], &[branch.clone()]).unwrap(),
            Some(b"at-the-branch".to_vec())
        );
        // Anything longer must descend, and there is nothing to descend into.
        assert_eq!(verify_proof(hash_of(&branch), &[0x1], &[branch]).unwrap(), None);
    }

    // ------------------------------------------------------------------
    // verify_proof: extension and multi-level tries
    // ------------------------------------------------------------------

    #[test]
    fn verify_proof_extension_descends() {
        let key = bytes_to_nibbles(&keccak(b"delta"));
        let leaf = leaf_node(&key[2..], b"under-extension");
        let ext = extension_node(&key[..2], &leaf);

        assert_eq!(
            verify_proof(hash_of(&ext), &key, &[ext, leaf]).unwrap(),
            Some(b"under-extension".to_vec())
        );
    }

    #[test]
    fn verify_proof_extension_diverging_key_is_absent() {
        let key = bytes_to_nibbles(&keccak(b"delta"));
        let leaf = leaf_node(&key[2..], b"under-extension");
        let ext = extension_node(&key[..2], &leaf);

        let mut other = key.clone();
        other[0] ^= 0x0f;
        assert_eq!(verify_proof(hash_of(&ext), &other, &[ext]).unwrap(), None);
    }

    #[test]
    fn verify_proof_three_levels_extension_branch_leaf() {
        // extension -> branch -> leaf, the shape a real state trie produces
        // once it holds enough accounts.
        let key = bytes_to_nibbles(&keccak(b"epsilon"));
        let leaf = leaf_node(&key[3..], b"deep");
        let mut children: [&[u8]; 16] = [&[]; 16];
        children[key[2] as usize] = &leaf;
        let branch = branch_node(children, None);
        let ext = extension_node(&key[..2], &branch);
        let proof = vec![ext.clone(), branch, leaf];

        assert_eq!(verify_proof(hash_of(&ext), &key, &proof).unwrap(), Some(b"deep".to_vec()));

        // Any other key under the same two-nibble prefix is provably absent.
        let mut sibling = key.clone();
        sibling[2] = (sibling[2] + 1) % 16;
        assert_eq!(verify_proof(hash_of(&ext), &sibling, &proof).unwrap(), None);
    }

    #[test]
    fn verify_proof_inlines_child_smaller_than_32_bytes() {
        // Nodes under 32 bytes are embedded in the parent rather than hashed.
        // A short synthetic path keeps the leaf small enough to inline.
        let key = vec![0x1u8, 0x2];
        let leaf = leaf_node(&key, b"tiny");
        assert!(leaf.len() < 32, "leaf must be small enough to inline");
        let ext = extension_node(&[], &leaf);

        // An empty extension prefix matches every key and routes into the leaf.
        assert_eq!(verify_proof(hash_of(&ext), &key, &[ext]).unwrap(), Some(b"tiny".to_vec()));
    }

    // ------------------------------------------------------------------
    // verify_proof: empty trie
    // ------------------------------------------------------------------

    #[test]
    fn verify_proof_empty_root_with_explicit_empty_node() {
        // The canonical proof for an empty trie is the single node 0x80.
        let key = slot_nibbles(U256::zero());
        assert_eq!(verify_proof(EMPTY_ROOT, &key, &[vec![0x80]]).unwrap(), None);
    }

    #[test]
    fn verify_proof_empty_root_needs_no_nodes() {
        // The root alone commits to "this trie is empty", so an absent node
        // list is still a complete proof of absence.
        let key = slot_nibbles(U256::one());
        assert_eq!(verify_proof(EMPTY_ROOT, &key, &[]).unwrap(), None);
    }

    #[test]
    fn verify_proof_embedded_empty_node_reports_absent() {
        // A crafted proof can inline the empty node under a non-empty root;
        // the walker must read it as "no key here", not as a leaf.
        let ext = rlp_list(&[rlp_string(&encode_path(&[0x1], false)), rlp_string(&[0x80])]);
        assert_eq!(verify_proof(hash_of(&ext), &[0x1, 0x2, 0x3], &[ext]).unwrap(), None);
    }

    // ------------------------------------------------------------------
    // verify_proof: failure modes
    // ------------------------------------------------------------------

    #[test]
    fn verify_proof_rejects_missing_root() {
        let root = H256::repeat_byte(0x33);
        assert!(verify_proof(root, &slot_nibbles(U256::zero()), &[]).is_err());
    }

    #[test]
    fn verify_proof_rejects_tampered_node() {
        let key = bytes_to_nibbles(&keccak(b"tamper"));
        let leaf = leaf_node(&key, b"payload");
        let root = hash_of(&leaf);

        // Flip a byte: the node no longer hashes to the claimed root, so the
        // root lookup fails and nothing can be proven from it.
        let mut tampered = leaf.clone();
        tampered[1] ^= 0xff;
        assert!(verify_proof(root, &key, &[tampered]).is_err());
    }

    #[test]
    fn verify_node_bytes_rejects_hash_mismatch() {
        // Defence in depth: a node presented under the wrong root must be
        // rejected even if a caller reaches it directly.
        let key = bytes_to_nibbles(&keccak(b"mismatch"));
        let leaf = leaf_node(&key, b"payload");
        let wrong_root = H256::repeat_byte(0x44);
        let err = verify_node_bytes(&leaf, &wrong_root, &key, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "unexpected: {err}");
    }

    #[test]
    fn verify_proof_rejects_swapped_sibling() {
        // Two valid leaves; supplying only the sibling's bytes must fail,
        // otherwise a client could prove a different key's value.
        let key = bytes_to_nibbles(&keccak(b"zeta"));
        let mut sibling_key = key.clone();
        sibling_key[63] ^= 0x01;
        let leaf = leaf_node(&key, b"mine");
        let sibling = leaf_node(&sibling_key, b"theirs");

        assert!(verify_proof(hash_of(&leaf), &key, &[sibling]).is_err());
    }

    #[test]
    fn verify_proof_rejects_missing_intermediate_node() {
        let key = bytes_to_nibbles(&keccak(b"eta"));
        let leaf = leaf_node(&key[1..], b"payload");
        let mut children: [&[u8]; 16] = [&[]; 16];
        children[key[0] as usize] = &leaf;
        let branch = branch_node(children, None);

        // The branch references the leaf by hash but the leaf is not supplied.
        let err = verify_proof(hash_of(&branch), &key, &[branch]).unwrap_err();
        assert!(err.to_string().contains("missing proof node"), "unexpected: {err}");
    }

    #[test]
    fn verify_proof_rejects_extension_with_empty_child() {
        // Malformed but hash-valid: an extension whose child slot is empty.
        let ext = rlp_list(&[rlp_string(&encode_path(&[0x1], false)), vec![0x80]]);
        let err = verify_proof(hash_of(&ext), &[0x1, 0x2], &[ext]).unwrap_err();
        assert!(err.to_string().contains("empty child"), "unexpected: {err}");
    }

    #[test]
    fn verify_proof_survives_corrupt_nodes() {
        // Untrusted bytes must produce errors or a verdict, never a panic.
        for bytes in [vec![], vec![0x01], vec![0x80, 0x80], vec![0xc0]] {
            let root = hash_of(&bytes);
            let _ = verify_proof(root, &slot_nibbles(U256::zero()), &[bytes]);
        }
    }

    // ------------------------------------------------------------------
    // verify_account_proof / verify_storage_proof / decode_account
    // ------------------------------------------------------------------

    fn account_value(nonce: u64, balance: U256, storage_root: H256, code_hash: H256) -> Vec<u8> {
        let nonce_bytes = minimal_be(&nonce.to_be_bytes());
        let balance_bytes = minimal_be(&balance.to_big_endian());
        rlp_list(&[
            rlp_string(&nonce_bytes),
            rlp_string(&balance_bytes),
            rlp_string(storage_root.as_bytes()),
            rlp_string(code_hash.as_bytes()),
        ])
    }

    #[test]
    fn account_proof_roundtrips_a_real_account_leaf() {
        let addr = H160::repeat_byte(0x42);
        let leaf = leaf_node(
            &account_nibbles(addr),
            &account_value(7, U256::from(1_000_000u64), EMPTY_ROOT, EMPTY_CODE_HASH),
        );
        let root = hash_of(&leaf);

        let decoded =
            verify_account_proof(root, addr, &[leaf]).unwrap().expect("account must be present");
        let (nonce, balance, storage_root, code_hash) = decode_account(&decoded).unwrap();
        assert_eq!(nonce, U256::from(7u64));
        assert_eq!(balance, U256::from(1_000_000u64));
        assert_eq!(storage_root, EMPTY_ROOT);
        assert_eq!(code_hash, EMPTY_CODE_HASH);
    }

    #[test]
    fn account_proof_absent_account_under_single_leaf_root() {
        let present = H160::repeat_byte(0x42);
        let mut absent_bytes = [0x43u8; 20];
        absent_bytes[19] = 0x44;
        let leaf = leaf_node(
            &account_nibbles(present),
            &account_value(0, U256::zero(), EMPTY_ROOT, EMPTY_CODE_HASH),
        );

        assert_eq!(
            verify_account_proof(hash_of(&leaf), H160(absent_bytes), &[leaf]).unwrap(),
            None
        );
    }

    #[test]
    fn storage_proof_present_slot() {
        let slot = U256::from(42u64);
        let value = U256::from(0xbeefu64);
        let leaf = leaf_node(&slot_nibbles(slot), &storage_value(value));

        let got = verify_storage_proof(hash_of(&leaf), slot, &[leaf]).unwrap();
        assert_eq!(got, Some(value));
    }

    #[test]
    fn storage_proof_absent_slot_under_empty_root() {
        // The common real-world shape: an account with no storage yet.
        let slot = U256::zero();
        assert_eq!(verify_storage_proof(EMPTY_ROOT, slot, &[vec![0x80]]).unwrap(), None);
        assert_eq!(verify_storage_proof(EMPTY_ROOT, slot, &[]).unwrap(), None);
    }

    #[test]
    fn storage_proof_diverging_slot_is_absent() {
        let slot = U256::from(42u64);
        let mut other = slot_nibbles(slot);
        other[63] ^= 0x01;
        let leaf = leaf_node(&other, &storage_value(U256::from(1u64)));

        assert_eq!(verify_storage_proof(hash_of(&leaf), slot, &[leaf]).unwrap(), None);
    }

    #[test]
    fn storage_proof_rejects_non_minimal_integer_value() {
        // A leaf value padded with a leading zero is not a canonical RLP
        // integer; the verifier must error rather than report a value.
        let slot = U256::from(7u64);
        let leaf = leaf_node(&slot_nibbles(slot), &[0x00, 0x01]);
        assert!(verify_storage_proof(hash_of(&leaf), slot, &[leaf]).is_err());
    }

    #[test]
    fn decode_account_reads_all_four_fields() {
        let encoded = account_value(
            3,
            U256::from(12345u64),
            H256::repeat_byte(0xab),
            H256::repeat_byte(0xcd),
        );

        let (nonce, balance, storage_root, code_hash) = decode_account(&encoded).unwrap();
        assert_eq!(nonce, U256::from(3u64));
        assert_eq!(balance, U256::from(12345u64));
        assert_eq!(storage_root, H256::repeat_byte(0xab));
        assert_eq!(code_hash, H256::repeat_byte(0xcd));
    }

    #[test]
    fn decode_account_rejects_short_list() {
        let encoded = rlp_list(&[rlp_string(&[1]), rlp_string(&[2]), rlp_string(&[3])]);
        assert!(decode_account(&encoded).is_err());
    }

    #[test]
    fn decode_account_rejects_non_minimal_integer() {
        // RLP integers must not carry leading zeros; a padded encoding is
        // ambiguous and must be rejected rather than silently accepted.
        let encoded = rlp_list(&[
            rlp_string(&[0x00, 0x01]),
            rlp_string(&[]),
            rlp_string(&H256::zero().as_bytes().to_vec()),
            rlp_string(&H256::zero().as_bytes().to_vec()),
        ]);
        assert!(decode_account(&encoded).is_err());
    }

    #[test]
    fn decode_account_rejects_non_rlp_input() {
        assert!(decode_account(&[0xff, 0xff, 0xff]).is_err());
    }

    // ------------------------------------------------------------------
    // Properties
    // ------------------------------------------------------------------

    proptest! {
        /// The hex-prefix codec must be a true bijection on nibbles: anything
        /// `decode_path` produces must re-encode to the same bytes.
        #[test]
        fn decode_path_roundtrips_encode_path(nibbles in proptest::collection::vec(0u8..=0x0f, 1..40), is_leaf: bool) {
            let encoded = encode_path(&nibbles, is_leaf);
            let (decoded_leaf, decoded) = decode_path(&encoded).unwrap();
            prop_assert_eq!(decoded, nibbles);
            prop_assert_eq!(decoded_leaf, is_leaf);
        }

        /// Every nibble a decoded path yields must be in range, otherwise the
        /// branch table index in `descend` could go out of bounds.
        #[test]
        fn decode_path_yields_only_valid_nibbles(bytes in proptest::collection::vec(any::<u8>(), 1..40)) {
            let (_, nibbles) = decode_path(&bytes).unwrap();
            prop_assert!(nibbles.iter().all(|n| *n <= 0x0f));
        }

        #[test]
        fn bytes_to_nibbles_expands_evenly(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let n = bytes_to_nibbles(&bytes);
            prop_assert_eq!(n.len(), bytes.len() * 2);
            for (i, &b) in bytes.iter().enumerate() {
                prop_assert_eq!(n[i * 2], b >> 4);
                prop_assert_eq!(n[i * 2 + 1], b & 0x0f);
            }
        }

        /// A one-leaf trie proves exactly the key it was built for.
        #[test]
        fn single_leaf_trie_proves_only_its_own_key(
            seed in any::<[u8; 32]>(),
            value in proptest::collection::vec(any::<u8>(), 0..24),
        ) {
            let key = bytes_to_nibbles(&keccak(&seed));
            let leaf = leaf_node(&key, &value);
            let root = hash_of(&leaf);
            prop_assert_eq!(verify_proof(root, &key, &[leaf.clone()]).unwrap(), Some(value));

            // Flip the last nibble of the path: the key is now a different one.
            let mut other = key.clone();
            let last = other.len() - 1;
            other[last] = (other[last] + 1) % 16;
            prop_assert_eq!(verify_proof(root, &other, &[leaf]).unwrap(), None);
        }

        /// A proof is bound to the root: a different root can never verify.
        #[test]
        fn proof_never_verifies_under_a_foreign_root(
            seed in any::<[u8; 32]>(),
            other_root in any::<[u8; 32]>(),
        ) {
            let key = bytes_to_nibbles(&keccak(&seed));
            let leaf = leaf_node(&key, b"payload");
            let root = H256::from(other_root);
            prop_assume!(root != hash_of(&leaf));
            prop_assert!(verify_proof(root, &key, &[leaf]).is_err());
        }

        /// Corrupting any single byte of a proof must never yield a value.
        #[test]
        fn no_single_byte_corruption_verifies(
            seed in any::<[u8; 32]>(),
            pick in 0usize..4096,
            mask in any::<u8>(),
        ) {
            let key = bytes_to_nibbles(&keccak(&seed));
            let leaf = leaf_node(&key, b"payload");
            let root = hash_of(&leaf);
            let mut corrupt = leaf.clone();
            // Fold both generated values into the node instead of rejecting
            // cases whose index misses it, so every case is actually exercised.
            let len = corrupt.len();
            corrupt[pick % len] ^= if mask == 0 { 1 } else { mask };
            prop_assert!(verify_proof(root, &key, &[corrupt]).is_err());
        }

        #[test]
        fn keccak_is_deterministic(a in any::<[u8; 32]>(), b in any::<[u8; 32]>()) {
            prop_assert_eq!(keccak(&a), keccak(&a));
            prop_assume!(a != b);
            prop_assert_ne!(keccak(&a), keccak(&b));
        }

        /// Account and slot keys are 32-byte hashes, so their nibble paths are
        /// always exactly 64 nibbles long.
        #[test]
        fn trie_keys_are_always_64_nibbles(
            addr in any::<[u8; 20]>(),
            slot in any::<[u8; 32]>(),
        ) {
            prop_assert_eq!(account_nibbles(H160(addr)).len(), 64);
            prop_assert_eq!(slot_nibbles(U256::from_big_endian(&slot)).len(), 64);
        }
    }
}
