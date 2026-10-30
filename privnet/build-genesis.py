#!/usr/bin/env python3
"""Build the private network's genesis files from bsc-genesis-contract.

The system contracts a BSC node needs at genesis are not something a private
network can make up: the block builder calls into them before every payload it
assembles. Without them a modern (post-`Bohr`) chain never seals a block at all
-- the builder logs `ETH call returned no data` against `0x2002` (StakeHub) and
gives up. `bnb-chain/bsc-genesis-contract` is the upstream source for those
deployments, so this script takes them from there and applies the private
network's own chain id, validators, test accounts and fork schedule.

Run directly, or via `privnet.sh system-contracts`:

    python3 -I privnet/build-genesis.py            # regenerate every profile
    python3 -I privnet/build-genesis.py --check    # regenerate into a temp dir and diff

Outputs (all next to this script unless `--out-dir` says otherwise):

    genesis.json          profile `classic`: 1 validator, 3 s blocks (the default)
    genesis-fast.json     profile `fast`:    1 validator, Lorentz/Maxwell/Fermi -> 450 ms
    genesis-pasteur.json  profile `pasteur`: `fast` plus Osaka/Mendel/Pasteur (BEP-675)
    genesis-multi.json    profile `multi`:   3 validators, turn length 4, `pasteur` forks
    accounts.json         every DEV-ONLY key the genesis files fund or register

Provenance
----------
`BSC_GENESIS_CONTRACT_URL` (default: the GitHub repository) and
`BSC_GENESIS_CONTRACT_REF` (default: `PINNED_REF` below, the commit tagged
`v1.2.6`) pick the upstream revision. A full 40-hex ref is verified against the
fetched commit. `genesis-dev.json` is the file used: it carries every address the
modern system hooks touch, `0x1000`-`0x1008`, `0x2000`-`0x2006` and `0x3000`.

Fork schedule
-------------
Block 0 is the one header both clients build from these files, and they disagree
as soon as a fork changes which fields that header carries:

* If London is active at block 0, go-bsc's `IsCancun`/`IsPrague` start
  returning true there, so go-bsc writes blob/withdrawal fields onto the genesis
  header.
* reth-bsc writes an empty withdrawals root whenever Shanghai is active at
  block 0, while go-bsc writes one only for non-BSC chains.

Keeping London at block 8 pins the genesis header to its pre-London shape on
both clients. Luban, in contrast, *is* active at block 0: the post-`Luban`
`extraData` is what registers each validator's BLS vote address, and that vote
path is what fast finality runs on.

Luban at 0 has two ordering consequences, and go-bsc's `CheckConfigForkOrder`
enforces both -- a chain with `parlia` configured is not exempt:

* Every block-gated fork before Luban collapses to 0 as well. Block forks must
  be non-decreasing, so `planckBlock: 5` with `lubanBlock: 0` fails with
  `unsupported fork ordering: planckBlock enabled at block 5, but lubanBlock
  enabled at block 0`.
* Timestamp-gated forks must be non-decreasing too. Every fork a profile does
  not activate is deferred to `DEFERRED_TIMESTAMP`, and so is every fork after
  it, because deferring only the first would invert the order.

Every activated timestamp fork uses QANet's timestamp: far enough in the past
that block 1 is already on the modern ladder, and later than the genesis
timestamp so block 0 stays pre-London. Lorentz, Maxwell and Fermi shrink
Parlia's block interval to 1.5 s, 0.75 s and 0.45 s (`snapshot.rs` in reth-bsc,
`parlia.go` in go-bsc); both clients read them from the `lorentzTime`,
`maxwellTime` and `fermiTime` keys of a custom genesis, so the `fast` profile
runs mainnet's 450 ms blocks.

Validators
----------
`BSCValidatorSet` at `0x1000` takes its initial set from the compiled-in
`INIT_VALIDATORSET_BYTES` constant, which upstream fills with its own dev
validator. Single-validator profiles rewrite that RLP blob in place (same length:
ours replaces upstream's consensus address three times and BLS vote address
once). The multi-validator profile cannot -- three validators do not fit in the
120 bytes the compiled code copies -- so it rewrites the blob as an empty set and
seeds the contract's storage directly; `init()` then registers nothing and the
seeded set stands. See GENESIS.md for the storage layout and how it was checked.
"""

from __future__ import annotations

import argparse
import collections
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

BSC_GENESIS_CONTRACT_URL = "https://github.com/bnb-chain/bsc-genesis-contract.git"
# The commit tagged v1.2.6. genesis-dev.json is byte-identical from here to
# master@8464181 (2026-09-11), which is what the previous, unpinned genesis.json
# was generated from.
PINNED_REF = "041881a02475638b19f3d840871b7621cdebd8f8"
UPSTREAM_FILE = "genesis-dev.json"
DEFAULT_CHAIN_ID = 1337

# ---------------------------------------------------------------------------
# DEV-ONLY keys. Every one of these is public; never fund them on BSC mainnet
# (56) or testnet (97). privnet.sh refuses to talk to those chain ids.
# ---------------------------------------------------------------------------
# (ECDSA key byte, BLS key byte): the key is that byte repeated 32 times.
VALIDATOR_KEYS = ((0x11, 0x07), (0x22, 0x08), (0x33, 0x09))
BUILDER_KEY = 0x44
USER_KEYS = (0x55, 0x66, 0x77)

VALIDATOR_BALANCE = 10**27  # 1e9 BNB
TEST_ACCOUNT_BALANCE = 10**25  # 1e7 BNB
VOTING_POWER = 0x64

# Parlia record layout after Luban: 32 bytes of vanity, a validator count, one
# `address(20) | vote address(48)` pair per validator, the turn-length byte that
# Bohr adds and a 65-byte seal. Block 0 is pre-Bohr (its timestamp is 2020), so
# neither client reads the turn-length byte there; it is kept for layout parity.
VANITY = "00" * 32
SEAL = "00" * 65
GENESIS_TURN_LENGTH_BYTE = 0

# Block-gated BSC rungs. Everything up to and including Luban is at 0: Luban
# active at block 0 is what puts the vote addresses into the genesis
# `extraData`, and go-bsc rejects a schedule whose block forks decrease. London
# stays at 8 so block 0 keeps its pre-London header shape.
BLOCK_FORKS = {
    "mirrorSyncBlock": 0,
    "brunoBlock": 0,
    "eulerBlock": 0,
    "nanoBlock": 0,
    "moranBlock": 0,
    "gibbsBlock": 0,
    "planckBlock": 0,
    "lubanBlock": 0,
    "platoBlock": 7,
    "berlinBlock": 8,
    "londonBlock": 8,
    "hertzBlock": 8,
    "hertzfixBlock": 8,
}

TIMESTAMP = 1754967081  # QANet's ladder timestamp
DEFERRED_TIMESTAMP = 9999999999  # past any realistic run
# In activation order. A profile activates a prefix and defers the rest.
TIMESTAMP_LADDER = (
    "shanghaiTime",
    "keplerTime",
    "feynmanTime",
    "feynmanFixTime",
    "cancunTime",
    "haberTime",
    "haberFixTime",
    "bohrTime",
    "pascalTime",
    "tychoTime",
    "pragueTime",
    "lorentzTime",
    "maxwellTime",
    "fermiTime",
    "osakaTime",
    "mendelTime",
    "pasteurTime",
)
TIMINGS = {
    # 3 s blocks: everything up to Prague/Pascal.
    "classic": "pragueTime",
    # 450 ms blocks: + Lorentz (1.5 s), Maxwell (0.75 s), Fermi (0.45 s).
    "fast": "fermiTime",
    # Mainnet's fork set as of 2026-10: + Osaka, Mendel, Pasteur (BEP-675).
    "pasteur": "pasteurTime",
}

PROFILES = collections.OrderedDict(
    (
        ("classic", {"out": "genesis.json", "timing": "classic", "validators": 1, "turn_length": None}),
        ("fast", {"out": "genesis-fast.json", "timing": "fast", "validators": 1, "turn_length": None}),
        ("pasteur", {"out": "genesis-pasteur.json", "timing": "pasteur", "validators": 1, "turn_length": None}),
        ("multi", {"out": "genesis-multi.json", "timing": "pasteur", "validators": 3, "turn_length": 4}),
    )
)
ACCOUNTS_FILE = "accounts.json"

# BSC keeps Cancun's blob parameters on every later fork
# (`DefaultPragueBlobConfigBSC = DefaultCancunBlobConfig`).
BLOB_PARAMS = {"target": 3, "max": 6, "baseFeeUpdateFraction": 3338477}

# BSC's system contracts live in three contiguous address ranges. `0xffff..fe`
# is the genesis coinbase; upstream lists it with a zero balance and it is
# dropped, as it always was here, so the genesis state trie stays unchanged.
_SYSTEM_RANGES = ((0x1000, 0x1008), (0x2000, 0x2006), (0x3000, 0x3000))
_GENESIS_COINBASE = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFE
VALIDATOR_CONTRACT = "0x0000000000000000000000000000000000001000"

# Storage layout of the `BSCValidatorSet` bytecode genesis-dev.json deploys
# (System.alreadyInit occupies slot 0). Verified by seeding these slots,
# running `init()` and reading back `getMiningValidators()`, `getTurnLength()`,
# `currentValidatorSetMap()` and a `deposit()` -- see GENESIS.md.
SLOT_CURRENT_VALIDATOR_SET = 1  # Validator[]: 4 slots per entry
SLOT_CURRENT_VALIDATOR_SET_MAP = 4  # mapping(address => index + 1)
SLOT_VALIDATOR_EXTRA_SET = 11  # ValidatorExtra[]: 22 slots per entry, voteAddress at +2
SLOT_TURN_LENGTH = 23
VALIDATOR_STRUCT_SLOTS = 4
VALIDATOR_EXTRA_STRUCT_SLOTS = 22
VALIDATOR_EXTRA_VOTE_OFFSET = 2


# ---------------------------------------------------------------------------
# Minimal crypto, so the addresses and vote keys are derived rather than pinned.
# keccak256 is Ethereum's (pre-NIST padding), which hashlib does not provide.
# ---------------------------------------------------------------------------
_KECCAK_RC = (
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
)
_KECCAK_ROT = (
    (0, 36, 3, 41, 18), (1, 44, 10, 45, 2), (62, 6, 43, 15, 61),
    (28, 55, 25, 21, 56), (27, 20, 39, 8, 14),
)
_M64 = (1 << 64) - 1


def _rol64(value: int, shift: int) -> int:
    return ((value << shift) | (value >> (64 - shift))) & _M64 if shift else value


def keccak256(data: bytes) -> bytes:
    rate = 136
    msg = bytearray(data) + b"\x01"
    msg += b"\x00" * ((-len(msg)) % rate)
    msg[-1] |= 0x80
    state = [[0] * 5 for _ in range(5)]
    for off in range(0, len(msg), rate):
        for i in range(rate // 8):
            state[i % 5][i // 5] ^= int.from_bytes(msg[off + 8 * i:off + 8 * i + 8], "little")
        for rc in _KECCAK_RC:
            c = [state[x][0] ^ state[x][1] ^ state[x][2] ^ state[x][3] ^ state[x][4] for x in range(5)]
            d = [c[(x - 1) % 5] ^ _rol64(c[(x + 1) % 5], 1) for x in range(5)]
            a = [[state[x][y] ^ d[x] for y in range(5)] for x in range(5)]
            b = [[0] * 5 for _ in range(5)]
            for x in range(5):
                for y in range(5):
                    b[y][(2 * x + 3 * y) % 5] = _rol64(a[x][y], _KECCAK_ROT[x][y])
            state = [[b[x][y] ^ (~b[(x + 1) % 5][y] & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
            state[0][0] ^= rc
    return b"".join(state[i % 5][i // 5].to_bytes(8, "little") for i in range(4))


def _ec_add(p1, p2, mod):
    if p1 is None:
        return p2
    if p2 is None:
        return p1
    if p1[0] == p2[0] and (p1[1] + p2[1]) % mod == 0:
        return None
    if p1 == p2:
        lam = 3 * p1[0] * p1[0] * pow(2 * p1[1], -1, mod) % mod
    else:
        lam = (p2[1] - p1[1]) * pow(p2[0] - p1[0], -1, mod) % mod
    x = (lam * lam - p1[0] - p2[0]) % mod
    return x, (lam * (p1[0] - x) - p1[1]) % mod


def _ec_mul(scalar, point, mod):
    acc = None
    while scalar:
        if scalar & 1:
            acc = _ec_add(acc, point, mod)
        point = _ec_add(point, point, mod)
        scalar >>= 1
    return acc


_SECP_P = 2**256 - 2**32 - 977
_SECP_G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)
_BLS_P = int(
    "1A0111EA397FE69A4B1BA7B6434BACD764774B84F38512BF6730D2A0F6B0F6241EABFFFEB153FFFFB9FEFFFFFFFFAAAB", 16
)
_BLS_R = 0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001
_BLS_G1 = (
    int("17F1D3A73197D7942695638C4FA9AC0FC3688C4F9774B905A14E3A3F171BAC586C55E83FF97A1AEFFB3AF00ADB22C6BB", 16),
    int("08B3F481E3AAA0F1A09E30ED741D8AE4FCF5E095D5D00AF600DB18CB2C04B3EDD03CC744A2888AE40CAA232946C5E7E1", 16),
)


def checksum_address(raw_hex: str) -> str:
    raw_hex = raw_hex.lower().removeprefix("0x")
    digest = keccak256(raw_hex.encode()).hex()
    return "0x" + "".join(c.upper() if int(digest[i], 16) >= 8 else c for i, c in enumerate(raw_hex))


def eth_address(secret: int) -> str:
    x, y = _ec_mul(secret, _SECP_G, _SECP_P)
    return checksum_address(keccak256(x.to_bytes(32, "big") + y.to_bytes(32, "big"))[12:].hex())


def bls_public_key(secret: int) -> str:
    """blst min_pk `sk_to_pk()`, compressed (48 bytes): the BSC vote address."""
    if not 0 < secret < _BLS_R:
        raise ValueError("BLS secret key out of range")
    x, y = _ec_mul(secret, _BLS_G1, _BLS_P)
    raw = bytearray(x.to_bytes(48, "big"))
    raw[0] |= 0x80  # compressed
    if y > _BLS_P - y:
        raw[0] |= 0x20  # lexicographically largest y
    return raw.hex()


def _key(byte: int) -> int:
    return int.from_bytes(bytes([byte]) * 32, "big")


def dev_accounts() -> dict:
    validators = []
    for i, (ecdsa, bls) in enumerate(VALIDATOR_KEYS, start=1):
        validators.append(collections.OrderedDict((
            ("name", f"validator{i}"),
            ("address", eth_address(_key(ecdsa))),
            ("privateKey", f"0x{_key(ecdsa):064x}"),
            ("blsPrivateKey", f"0x{_key(bls):064x}"),
            ("voteAddress", "0x" + bls_public_key(_key(bls))),
        )))
    builder = collections.OrderedDict((
        ("name", "builder"),
        ("address", eth_address(_key(BUILDER_KEY))),
        ("privateKey", f"0x{_key(BUILDER_KEY):064x}"),
    ))
    users = [
        collections.OrderedDict((
            ("name", f"user{i}"),
            ("address", eth_address(_key(byte))),
            ("privateKey", f"0x{_key(byte):064x}"),
        ))
        for i, byte in enumerate(USER_KEYS, start=1)
    ]
    return collections.OrderedDict((
        ("_warning", ("DEV-ONLY keys: every private key below is public. Never fund them on "
                      "BSC mainnet (56) or testnet (97); privnet.sh refuses those chain ids.")),
        ("validators", validators),
        ("builder", builder),
        ("users", users),
        ("validatorBalanceWei", hex(VALIDATOR_BALANCE)),
        ("testAccountBalanceWei", hex(TEST_ACCOUNT_BALANCE)),
    ))


# ---------------------------------------------------------------------------
# RLP for the `INIT_VALIDATORSET_BYTES` blob.
# ---------------------------------------------------------------------------
def _rlp_length(length: int, offset: int) -> bytes:
    if length < 56:
        return bytes([offset + length])
    encoded = length.to_bytes((length.bit_length() + 7) // 8, "big")
    return bytes([offset + 55 + len(encoded)]) + encoded


def rlp_encode(item) -> bytes:
    if isinstance(item, int):
        item = item.to_bytes((item.bit_length() + 7) // 8, "big") if item else b""
    if isinstance(item, bytes):
        if len(item) == 1 and item[0] < 0x80:
            return item
        return _rlp_length(len(item), 0x80) + item
    payload = b"".join(rlp_encode(x) for x in item)
    return _rlp_length(len(payload), 0xC0) + payload


def validator_set_blob(entries) -> bytes:
    """`validators.js`'s encoding: [0, [[consensus, bscFee, fee, power, bls], ...]]."""
    return rlp_encode([0, [[bytes.fromhex(a[2:])] * 3 + [VOTING_POWER, bytes.fromhex(v[2:])]
                           for a, v in entries]])


def empty_validator_set_blob(length: int) -> bytes:
    """[0, [], <zero padding>]: decodes as a valid package with no validators.

    `decodeValidatorSet` stops at the third item, and the padding keeps the blob
    the exact length the compiled code copies.
    """
    for pad in range(length):
        blob = rlp_encode([0, [], bytes(pad)])
        if len(blob) == length:
            return blob
    raise SystemExit(f"cannot pad an empty validator set to {length} bytes")


# ---------------------------------------------------------------------------
# Genesis assembly
# ---------------------------------------------------------------------------
def _is_system_contract(address: str) -> bool:
    value = int(address, 16)
    return any(low <= value <= high for low, high in _SYSTEM_RANGES)


def _upstream_dev_validator(upstream: dict) -> tuple[str, str, bytes]:
    """The one dev validator upstream bakes into `0x1000`, and its blob there."""
    devs = [a for a in upstream["alloc"]
            if not _is_system_contract(a) and int(a, 16) != _GENESIS_COINBASE]
    if len(devs) != 1:
        raise SystemExit(f"expected exactly one upstream dev validator, found {len(devs)}: {devs}")
    dev = "0x" + devs[0].lower().removeprefix("0x")
    code = _validator_code(upstream)
    # Its BLS key is the 48-byte string that follows the voting power.
    match = re.search(dev[2:] + r"94" + dev[2:] + r"94" + dev[2:] + r"64b0([0-9a-f]{96})", code)
    if not match:
        raise SystemExit("upstream INIT_VALIDATORSET_BYTES not found in 0x1000 bytecode")
    bls = "0x" + match.group(1)
    blob = validator_set_blob([(dev, bls)])
    return dev, bls, blob


def _validator_code(genesis: dict) -> str:
    for address, account in genesis["alloc"].items():
        if int(address, 16) == int(VALIDATOR_CONTRACT, 16):
            return account["code"].lower().removeprefix("0x")
    raise SystemExit("0x1000 (BSCValidatorSet) missing from the upstream genesis")


def _slot(value: int) -> str:
    return "0x" + value.to_bytes(32, "big").hex()


def _array_base(slot: int) -> int:
    return int.from_bytes(keccak256(slot.to_bytes(32, "big")), "big")


def validator_set_storage(entries, turn_length: int | None) -> dict:
    """Storage that makes `BSCValidatorSet` hold `entries` once `init()` ran on an empty blob."""
    storage = {SLOT_CURRENT_VALIDATOR_SET: len(entries), SLOT_VALIDATOR_EXTRA_SET: len(entries)}
    set_base = _array_base(SLOT_CURRENT_VALIDATOR_SET)
    extra_base = _array_base(SLOT_VALIDATOR_EXTRA_SET)
    for index, (address, vote) in enumerate(entries):
        addr = int(address, 16)
        entry = set_base + VALIDATOR_STRUCT_SLOTS * index
        storage[entry] = addr  # consensusAddress
        storage[entry + 1] = addr  # feeAddress
        storage[entry + 2] = addr | (VOTING_POWER << 160)  # BBCFeeAddress | votingPower | jailed=0
        map_key = keccak256(addr.to_bytes(32, "big") + SLOT_CURRENT_VALIDATOR_SET_MAP.to_bytes(32, "big"))
        storage[int.from_bytes(map_key, "big")] = index + 1
        vote_slot = extra_base + VALIDATOR_EXTRA_STRUCT_SLOTS * index + VALIDATOR_EXTRA_VOTE_OFFSET
        raw = bytes.fromhex(vote[2:])
        storage[vote_slot] = len(raw) * 2 + 1  # long `bytes`: data lives at keccak(slot)
        data = _array_base(vote_slot)
        storage[data] = int.from_bytes(raw[:32], "big")
        storage[data + 1] = int.from_bytes(raw[32:].ljust(32, b"\x00"), "big")
    if turn_length is not None:
        storage[SLOT_TURN_LENGTH] = turn_length
    return collections.OrderedDict((_slot(k), _slot(v)) for k, v in sorted(storage.items()))


def build(upstream: dict, chain_id: int, timing: str, validators: int,
          turn_length: int | None, accounts: dict) -> tuple[dict, dict]:
    if timing not in TIMINGS:
        raise SystemExit(f"unknown timing {timing!r}; expected one of {', '.join(TIMINGS)}")
    if not 1 <= validators <= len(accounts["validators"]):
        raise SystemExit(f"--validators must be 1..{len(accounts['validators'])}")
    if validators == 1 and turn_length not in (None, 1):
        raise SystemExit("a turn length only matters with more than one validator")
    if turn_length is not None and not 1 <= turn_length <= 64:
        raise SystemExit("--turn-length must be 1..64")

    chosen = sorted(accounts["validators"][:validators], key=lambda v: int(v["address"], 16))
    entries = [(v["address"], v["voteAddress"]) for v in chosen]

    genesis = collections.OrderedDict()
    config = collections.OrderedDict(upstream["config"])
    config["chainId"] = chain_id
    config.update(BLOCK_FORKS)
    last = TIMESTAMP_LADDER.index(TIMINGS[timing])
    for index, fork in enumerate(TIMESTAMP_LADDER):
        config[fork] = TIMESTAMP if index <= last else DEFERRED_TIMESTAMP
    config["blobSchedule"] = collections.OrderedDict(
        (name, dict(BLOB_PARAMS)) for name in ("cancun", "prague", "osaka")
    )
    genesis["config"] = config

    genesis["nonce"] = upstream["nonce"]
    genesis["timestamp"] = upstream["timestamp"]
    # Post-Luban validator record: block 0 carries it because `lubanBlock` is 0,
    # and it is what seeds the genesis snapshot's validators and vote addresses.
    record = f"{len(entries):02x}" + "".join(a[2:].lower() + v[2:] for a, v in entries)
    genesis["extraData"] = "0x" + VANITY + record + f"{GENESIS_TURN_LENGTH_BYTE:02x}" + SEAL
    genesis["gasLimit"] = upstream["gasLimit"]
    genesis["difficulty"] = upstream["difficulty"]
    genesis["mixHash"] = upstream["mixHash"]
    genesis["coinbase"] = upstream["coinbase"]
    # Deliberately no `baseFeePerGas`. London activates at block 8, so neither
    # client puts a base fee on the genesis header; setting one makes go-bsc
    # copy it onto the header unconditionally while reth-bsc does not.

    # Keep every system contract, drop everything else: a stranded upstream dev
    # validator joins the consensus set, takes turns no node can sign, and the
    # chain stalls on `Signed recently, must wait for others`.
    alloc = collections.OrderedDict()
    for address, account in upstream["alloc"].items():
        if _is_system_contract(address):
            alloc[address] = json.loads(json.dumps(account))

    dev, dev_bls, dev_blob = _upstream_dev_validator(upstream)
    validator_key = next(a for a in alloc if int(a, 16) == int(VALIDATOR_CONTRACT, 16))
    code = alloc[validator_key]["code"]
    prefix, body = code[:2], code[2:]
    # The upstream dev validator must appear only inside the blob, and the blob
    # exactly once, or the in-place rewrite would be patching the wrong bytes.
    counts = {
        "dev address": (body.lower().count(dev[2:]), 3),
        "dev BLS key": (body.lower().count(dev_bls[2:]), 1),
        "validator-set blob": (body.lower().count(dev_blob.hex()), 1),
    }
    for other, account in alloc.items():
        if other != validator_key and dev[2:] in account.get("code", "").lower():
            raise SystemExit(f"upstream dev validator also appears in {other}'s code")
    for what, (got, want) in counts.items():
        if got != want:
            raise SystemExit(f"expected {want} occurrence(s) of the upstream {what} in 0x1000, found {got}")

    if validators == 1:
        new_blob = validator_set_blob(entries)
        storage = None
    else:
        new_blob = empty_validator_set_blob(len(dev_blob))
        storage = validator_set_storage(entries, turn_length)
    if len(new_blob) != len(dev_blob):
        raise SystemExit(f"replacement blob is {len(new_blob)} bytes, the code copies {len(dev_blob)}")
    start = body.lower().index(dev_blob.hex())
    body = body[:start] + new_blob.hex() + body[start + len(new_blob.hex()):]
    alloc[validator_key]["code"] = prefix + body
    if storage:
        alloc[validator_key]["storage"] = storage

    for validator in accounts["validators"][:validators]:
        alloc[validator["address"].removeprefix("0x")] = {"balance": hex(VALIDATOR_BALANCE)}
    for account in [accounts["builder"], *accounts["users"]]:
        alloc[account["address"].removeprefix("0x")] = {"balance": hex(TEST_ACCOUNT_BALANCE)}
    genesis["alloc"] = alloc
    genesis["number"] = "0x0"
    genesis["parentHash"] = "0x" + "00" * 32

    report = collections.OrderedDict((
        ("validators", [a for a, _ in entries]),
        ("blob", "rewritten in place" if validators == 1 else "emptied; set seeded in storage"),
        ("storage slots", len(storage or {})),
        ("turn length", turn_length or "contract default (1)"),
        ("activated", ", ".join(TIMESTAMP_LADDER[: last + 1][-3:]) + " (and earlier)"),
        ("deferred", ", ".join(TIMESTAMP_LADDER[last + 1:]) or "none"),
    ))
    return genesis, report


def fetch_upstream(cache: Path, url: str, ref: str) -> tuple[dict, str]:
    """Fetch bsc-genesis-contract at `ref` and return its dev genesis and commit."""
    def git(*args, capture=True):
        return subprocess.run(["git", *args], cwd=cache, check=True,
                              capture_output=capture, text=True).stdout.strip()

    if not (cache / ".git").exists():
        cache.mkdir(parents=True, exist_ok=True)
        git("init", "-q", ".")
        git("remote", "add", "origin", url)
    else:
        git("remote", "set-url", "origin", url)
    try:
        git("fetch", "-q", "--depth", "1", "origin", ref)
    except subprocess.CalledProcessError as err:
        raise SystemExit(f"git fetch {url} {ref} failed: {err.stderr.strip()}") from err
    git("checkout", "-q", "FETCH_HEAD")
    rev = git("rev-parse", "HEAD")
    if re.fullmatch(r"[0-9a-f]{40}", ref) and rev != ref:
        raise SystemExit(f"fetched {rev}, expected pinned {ref}")
    path = cache / UPSTREAM_FILE
    if not path.exists():
        raise SystemExit(f"{UPSTREAM_FILE} missing from bsc-genesis-contract@{rev}")
    return json.loads(path.read_text()), rev


def render(document: dict) -> str:
    return json.dumps(document, indent=2) + "\n"


def generate(upstream: dict, chain_id: int, out_dir: Path, profiles, quiet: bool = False) -> list[Path]:
    accounts = dev_accounts()
    written = []
    for name, spec in profiles:
        genesis, report = build(upstream, chain_id, spec["timing"], spec["validators"],
                                spec["turn_length"], accounts)
        out = out_dir / spec["out"]
        out.write_text(render(genesis))
        written.append(out)
        if quiet:
            continue
        print(f"wrote {out} (profile {name}, timing {spec['timing']})")
        for key, value in report.items():
            print(f"  {key:<14}: {value}")
    accounts_out = out_dir / ACCOUNTS_FILE
    accounts_out.write_text(render(accounts))
    written.append(accounts_out)
    if not quiet:
        print(f"wrote {accounts_out} (DEV-ONLY keys)")
    return written


def check(upstream: dict, chain_id: int, here: Path, profiles) -> int:
    failures = 0
    with tempfile.TemporaryDirectory(prefix="privnet-genesis-check-") as tmp:
        for fresh in generate(upstream, chain_id, Path(tmp), profiles, quiet=True):
            committed = here / fresh.name
            if not committed.exists():
                print(f"MISSING  {committed}")
                failures += 1
                continue
            want, got = fresh.read_text(), committed.read_text()
            if want == got:
                print(f"OK       {committed.name}")
                continue
            failures += 1
            print(f"DIFFERS  {committed.name}")
            diff = difflib.unified_diff(got.splitlines(), want.splitlines(),
                                        f"committed/{committed.name}", f"regenerated/{committed.name}",
                                        lineterm="", n=1)
            for count, line in enumerate(diff):
                if count >= 40:
                    print("  ... (diff truncated)")
                    break
                print("  " + line[:200])
    sys.stdout.flush()
    if failures:
        print(f"{failures} file(s) out of date; run: python3 -I privnet/build-genesis.py", file=sys.stderr)
        return 1
    print("all generated files match")
    return 0


def main() -> int:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    parser.add_argument("--check", action="store_true",
                        help="regenerate into a temp dir and diff against the committed files")
    parser.add_argument("--out-dir", default=str(here), help="where to write (default: next to this script)")
    parser.add_argument("--profile", action="append", choices=list(PROFILES),
                        help="only these profiles (default: all; --check always uses all)")
    parser.add_argument("--chain-id", type=int, default=DEFAULT_CHAIN_ID)
    parser.add_argument("--cache", default=None, help="bsc-genesis-contract clone cache (default: a temp dir)")
    args = parser.parse_args()

    if args.chain_id in (56, 97):
        parser.error("chain id 56/97 is BSC mainnet/testnet; these genesis files carry DEV-ONLY keys")
    url = os.environ.get("BSC_GENESIS_CONTRACT_URL", BSC_GENESIS_CONTRACT_URL)
    ref = os.environ.get("BSC_GENESIS_CONTRACT_REF", PINNED_REF)

    with tempfile.TemporaryDirectory(prefix="bsc-genesis-contract-") as tmp:
        cache = Path(args.cache) if args.cache else Path(tmp) / "repo"
        upstream, rev = fetch_upstream(cache, url, ref)
    print(f"source: {url} @ {rev} ({UPSTREAM_FILE})", flush=True)
    if rev != PINNED_REF:
        print(f"note: not the pinned revision {PINNED_REF}", file=sys.stderr)

    if args.check:
        return check(upstream, args.chain_id, here, PROFILES.items())
    profiles = [(name, PROFILES[name]) for name in (args.profile or PROFILES)]
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    generate(upstream, args.chain_id, out_dir, profiles)
    return 0


if __name__ == "__main__":
    sys.exit(main())
