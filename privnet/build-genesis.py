#!/usr/bin/env python3
"""Build the private network's genesis from bsc-genesis-contract.

The system contracts a BSC node needs at genesis are not something a private
network can make up: the block builder calls into them before every payload it
assembles. Without them a modern (post-`Bohr`) chain never seals a block at all
-- the builder logs `ETH call returned no data` against `0x2002` (StakeHub) and
gives up. `bnb-chain/bsc-genesis-contract` is the upstream source for those
deployments, so this script takes them from there and applies the private
network's own chain id, validator and fork schedule.

Run directly, or via `privnet.sh system-contracts`:

    python3 privnet/build-genesis.py --out privnet/genesis.json

Provenance
----------
* `BSC_GENESIS_CONTRACT_URL` / `BSC_GENESIS_CONTRACT_REF` pick the upstream
  revision. `genesis-dev.json` is the file used: it carries every address the
  modern system hooks touch, `0x1000`-`0x1008`, `0x2000`-`0x2006` and `0x3000`.

Fork schedule
-------------
The block-gated rungs are kept *above* zero and the timestamp-gated ones are
set to a past timestamp. Both halves are load-bearing, and both exist to keep
go-bsc and reth-bsc agreeing on the genesis hash:

* If London is active at block 0, Parlia requires the post-`Luban` `extraData`
  layout (`count | address | vote`), and go-bsc's `IsCancun`/`IsPrague` start
  returning true at block 0, so go-bsc writes blob/withdrawal fields onto the
  genesis header.
* reth-bsc writes an empty withdrawals root whenever Shanghai is active at
  block 0, while go-bsc writes one only for non-BSC chains.

Either way the two clients derive different hashes from one file, refuse to
peer, and reth-bsc's miner (which needs a peer to unseal its peer gate) never
produces a block. Keeping block 0 pre-London and the timestamp ladder in the
past gives a chain that is modern from block 1 while genesis stays byte-identical
across clients.
"""

from __future__ import annotations

import argparse
import collections
import json
import subprocess
import sys
import tempfile
from pathlib import Path


BSC_GENESIS_CONTRACT_URL = "https://github.com/bnb-chain/bsc-genesis-contract.git"
BSC_GENESIS_CONTRACT_REF = "master"
UPSTREAM_FILE = "genesis-dev.json"

# The development validator used by privnet.sh. NEVER reuse on mainnet.
VALIDATOR = "19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"
VALIDATOR_BALANCE = "0x33B2E3C9FD0803CE8000000"

# Parlia block layout: 32 bytes vanity, then the validator record, then a
# 65-byte seal.
VANITY = "00" * 32
SEAL = "00" * 65

# Block-gated BSC rungs. Everything is at or above 1 so that block 0 itself is
# pre-Luban; see the module docstring.
BLOCK_FORKS = {
    "mirrorSyncBlock": 1,
    "brunoBlock": 1,
    "eulerBlock": 2,
    "nanoBlock": 3,
    "moranBlock": 3,
    "gibbsBlock": 4,
    "planckBlock": 5,
    "lubanBlock": 6,
    "platoBlock": 7,
    "berlinBlock": 8,
    "londonBlock": 8,
    "hertzBlock": 8,
    "hertzfixBlock": 8,
}

# Timestamp-gated BSC rungs, all activated at QANet's timestamp: far enough in
# the past that a fresh chain is immediately on the modern ladder, and later
# than the genesis timestamp so block 0 stays pre-London.
TIMESTAMP = 1754967081
TIMESTAMP_FORKS = (
    "shanghaiTime",
    "keplerTime",
    "feynmanTime",
    "feynmanFixTime",
    "cancunTime",
    "haberTime",
    "haberFixTime",
    "bohrTime",
    "tychoTime",
    "pascalTime",
    "pragueTime",
    "lorentzTime",
    "maxwellTime",
    "fermiTime",
    "osakaTime",
    "mendelTime",
)

# BSC keeps Cancun's blob parameters on every later fork
# (`DefaultPragueBlobConfigBSC = DefaultCancunBlobConfig`).
BLOB_PARAMS = {"target": 3, "max": 6, "baseFeeUpdateFraction": 3338477}

# BSC's system contracts live in two contiguous address ranges; `0xffff..e` is
# the burn account both clients reference as the genesis coinbase.
_SYSTEM_RANGES = ((0x1000, 0x1008), (0x2000, 0x2006), (0x3000, 0x3000))
_BURN_ACCOUNT = 0xFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFFFFFFFFFF


def _is_system_contract(address: str) -> bool:
    value = int(address, 16)
    if value == _BURN_ACCOUNT:
        return True
    return any(low <= value <= high for low, high in _SYSTEM_RANGES)


def _patch_dev_validators(genesis: dict, upstream: dict) -> int:
    """Point the `BSCValidatorSetTool` validator set at our validator.

    `genesis-dev.json` deploys `BSCValidatorSetTool` at `0x1000`, whose `init()`
    decodes a hardcoded `INIT_VALIDATORSET_BYTES` RLP blob describing the upstream
    dev validator -- one entry, whose `consensusAddress`, `feeAddress` and
    `BBCFeeAddress` are all that address, which is why the substitution count is
    3. The system hook calls that `init()`, and it is the only thing that puts a
    validator into the consensus set. Left alone it registers an address whose
    key we do not hold, and the chain stalls on
    `Signed recently, must wait for others`.

    The address is a Solidity `constant`, so it is compiled into the code and
    cannot be fixed from the genesis. Each occurrence is preceded by `0x94`,
    the RLP prefix for a 20-byte string (`0x80 + 20`), so substituting an
    address of the same length leaves every length prefix in the blob valid --
    the result is byte-identical to what recompiling from a patched
    `scripts/validators.js` would produce.

    This, and nothing else, must supply the validator: seeding
    `currentValidatorSet` in genesis storage *as well* produces a set of two
    entries holding the same address, and the two clients then disagree about
    whether the block is even valid (see `GENESIS.md`).

    Returns the number of substitutions made.
    """
    dev_validators = {
        address.lower().removeprefix("0x")
        for address, account in upstream["alloc"].items()
        if not _is_system_contract(address)
    }
    replacements = 0
    for account in genesis["alloc"].values():
        code = account.get("code")
        if not code:
            continue
        patched = code
        for dev in dev_validators:
            needle = dev
            occurrences = patched.lower().count(needle)
            if occurrences:
                patched = patched.replace(needle, VALIDATOR.removeprefix("0x").lower())
                replacements += occurrences
        account["code"] = patched
    return replacements


def fetch_upstream(cache: Path) -> dict:
    """Clone bsc-genesis-contract at the pinned ref and return its genesis."""
    if cache.exists():
        subprocess.run(["git", "fetch", "--depth", "1", "origin", BSC_GENESIS_CONTRACT_REF],
                       cwd=cache, check=True, capture_output=True)
    else:
        cache.mkdir(parents=True)
        subprocess.run(["git", "init", "-q", "."], cwd=cache, check=True, capture_output=True)
        subprocess.run(["git", "remote", "add", "origin", BSC_GENESIS_CONTRACT_URL],
                       cwd=cache, check=True, capture_output=True)
    subprocess.run(["git", "fetch", "--depth", "1", "origin", BSC_GENESIS_CONTRACT_REF],
                   cwd=cache, check=True, capture_output=True)
    subprocess.run(["git", "checkout", "-q", "FETCH_HEAD"], cwd=cache, check=True, capture_output=True)
    rev = subprocess.run(["git", "rev-parse", "HEAD"], cwd=cache, check=True,
                         capture_output=True, text=True).stdout.strip()
    path = cache / UPSTREAM_FILE
    if not path.exists():
        raise SystemExit(f"{UPSTREAM_FILE} missing from bsc-genesis-contract")
    return json.loads(path.read_text()), rev


def build(upstream: dict, chain_id: int) -> tuple[dict, int]:
    genesis = collections.OrderedDict()
    config = collections.OrderedDict(upstream["config"])
    config["chainId"] = chain_id
    config.update(BLOCK_FORKS)
    for fork in TIMESTAMP_FORKS:
        config[fork] = TIMESTAMP
    config["blobSchedule"] = collections.OrderedDict(
        (name, dict(BLOB_PARAMS)) for name in ("cancun", "prague", "osaka")
    )
    genesis["config"] = config

    genesis["nonce"] = upstream["nonce"]
    genesis["timestamp"] = upstream["timestamp"]
    # Pre-Luban validator record, which is what block 0 requires because
    # `lubanBlock` is 6.
    genesis["extraData"] = "0x" + VANITY + VALIDATOR + SEAL
    genesis["gasLimit"] = upstream["gasLimit"]
    genesis["difficulty"] = upstream["difficulty"]
    genesis["mixHash"] = upstream["mixHash"]
    genesis["coinbase"] = upstream["coinbase"]
    # Deliberately no `baseFeePerGas`. London activates at block 8, so neither
    # client puts a base fee on the genesis header; setting one makes go-bsc
    # copy it onto the header unconditionally while reth-bsc does not.
    genesis["alloc"] = collections.OrderedDict()
    # Keep every system contract, drop everything else. The upstream genesis
    # funds its own development validators, and a stranded validator is worse
    # than a missing one: it joins the consensus set, takes turns this node
    # cannot sign, and the chain stalls on `Signed recently, must wait for
    # others`.
    for address, account in upstream["alloc"].items():
        if _is_system_contract(address):
            genesis["alloc"][address] = account
    genesis["alloc"][VALIDATOR] = {"balance": VALIDATOR_BALANCE}
    patched = _patch_dev_validators(genesis, upstream)
    genesis["number"] = "0x0"
    genesis["parentHash"] = "0x" + "00" * 32
    return genesis, patched


def main() -> int:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", default=str(here / "genesis.json"),
                        help="output path for the generated genesis")
    parser.add_argument("--chain-id", type=int, default=1337)
    parser.add_argument("--cache", default=None,
                        help="clone cache (default: a temp dir)")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="bsc-genesis-contract-") as tmp:
        cache = Path(args.cache) if args.cache else Path(tmp) / "repo"
        upstream, rev = fetch_upstream(cache)

    genesis, patched = build(upstream, args.chain_id)
    out = Path(args.out)
    out.write_text(json.dumps(genesis, indent=2) + "\n")

    print(f"wrote {out}")
    print(f"  system contracts : {len(genesis['alloc'])} genesis accounts")
    print(f"  validator set    : {VALIDATOR} (from BSCValidatorSetTool bytecode)")
    print(f"  bytecode patches : {patched} dev-validator address(es)")
    print(f"  chain id         : {args.chain_id}")
    print(f"  validator        : 0x{VALIDATOR}")
    print(f"  extraData        : {(len(genesis['extraData']) - 2) // 2} bytes (pre-Luban)")
    print(f"  block rungs      : {min(BLOCK_FORKS.values())}..{max(BLOCK_FORKS.values())}")
    print(f"  timestamp ladder : {TIMESTAMP} (past)")
    print(f"  source           : bsc-genesis-contract@{rev[:12]} ({UPSTREAM_FILE})")
    return 0


if __name__ == "__main__":
    sys.exit(main())