# The private network's genesis

Every `genesis*.json` and `accounts.json` here is **generated**, not hand-maintained:

```bash
./privnet.sh system-contracts      # regenerate (network stopped)
./privnet.sh clean                 # every client must re-init against a new genesis
./privnet.sh mixed
python3 -I build-genesis.py --check  # CI: regenerate into a temp dir and diff
```

`build-genesis.py` takes the system-contract deployments from
[`bnb-chain/bsc-genesis-contract`](https://github.com/bnb-chain/bsc-genesis-contract)
(`genesis-dev.json`) and applies the private network's own chain id, validators,
test accounts and fork schedule on top. The upstream revision is pinned to
`041881a02475638b19f3d840871b7621cdebd8f8` (tag `v1.2.6`; `genesis-dev.json` is
byte-identical up to `master@8464181`, which the previous unpinned `genesis.json`
came from). `BSC_GENESIS_CONTRACT_REF` and `BSC_GENESIS_CONTRACT_URL` override it.
A full 40-hex ref is checked against the commit actually fetched. The script asserts
how many bytes each patch rewrites, so an upstream change that moves them fails the
build instead of producing a quietly wrong genesis.

| Profile | File | Validators | Activated forks | Block interval |
|---|---|---|---|---|
| `classic` | `genesis.json` | 1 | up to Prague/Pascal | 3 s |
| `fast` | `genesis-fast.json` | 1 | + Lorentz, Maxwell, Fermi | 450 ms |
| `pasteur` | `genesis-pasteur.json` | 1 | + Osaka, Mendel, Pasteur | 450 ms |
| `multi` | `genesis-multi.json` | 3, turn length 4 | as `pasteur` | 450 ms |

Every profile funds the same DEV-ONLY accounts (`accounts.json`): the validators with
1e9 BNB, and a builder and three users with 1e7 BNB each. User transactions should come
from those accounts. The validator key's nonce is also consumed by the system
transactions its node signs.

## Why the system contracts are not optional

A modern BSC node calls into system contracts *before it assembles every
payload*. Without those deployments the chain does not merely run a stale
spec — it produces no blocks at all:

```
ERROR Empty eth call output, to: 0x0000000000000000000000000000000000002002, data: 0xc473318f
ERROR failed to apply pre-execution changes for empty payload
```

`0x2002` is `STAKE_HUB_CONTRACT` and `0xc473318f` is `getValidators()`. Both
clients do this, in their own words: go-bsc accepts transactions and then logs
nothing at all, never once emitting `Commit new sealing work`.

The addresses that matter are `0x1000`-`0x1008`, `0x2000`-`0x2006` and
`0x3000`. They carry **no `storage`** in any upstream genesis, and they do not
need any: reth-bsc seeds its genesis snapshot from the genesis header's
`extraData` (`parse_validators_from_header`), not from contract state.

They are still what unblocks the epoch boundary. With no StakeHub the chain
stalled at block 199 — the epoch-200 validator set cannot be assembled — which
is the failure recorded in `docs/privnet.md`. With them the chain runs past it.

## Why block 0 stays pre-London

The timestamp ladder is split: the forks a profile activates sit at a past
timestamp (QANet's `1754967081`), and every fork after them sits at
`9999999999`, past any realistic run. In `classic` that tail is `lorentzTime`,
`maxwellTime`, `fermiTime`, `osakaTime`, `mendelTime` and `pasteurTime`. In
`fast` it is the last three, and `pasteur`/`multi` activate everything (Jenner, a
QANet-only fork, is left unscheduled). Either way block 0's timestamp (2020) is
before all of them. The block rungs are
`0..8` with `lubanBlock: 0` and `londonBlock: 8`. All three halves are
load-bearing, and all exist to stop go-bsc and reth-bsc from deriving
**different genesis hashes from one file**, which is fatal: they refuse to
peer, and reth-bsc's miner has a peer gate (`is_network_ready_to_mine`), so it
never unseals.

Four divergences were found and fixed, in this order:

**1. Genesis base fee.** Unpinned, go-bsc derives `0` and reth-bsc derives
1 gwei. But pinning `baseFeePerGas` in the genesis is only correct *when London
is active at block 0* — reth honours the field only then (`make_genesis_header`),
while go-bsc copies it onto the header unconditionally (`head.BaseFee = g.BaseFee`).
With London at block 8, pinning it makes go-bsc set a base fee reth-bsc does
not. So the generated genesis deliberately has **no** `baseFeePerGas`.

**2. Post-London header fields.** go-bsc gates Cancun and Prague on
`IsLondon(num)`:

```go
func (c *ChainConfig) IsCancun(num *big.Int, time uint64) bool {
    return c.IsLondon(num) && isTimestampForked(c.CancunTime, time)
}
```

With London at block 8, go-bsc writes a *pre-London* genesis header. reth-bsc
evaluated its own fork schedule against block 0 and wrote a *Cancun* one.
Setting `londonBlock: 0` makes both agree, but it also puts Cancun and Prague
into the genesis header, so London stays at block 8. Luban is block-gated and
independent of London: it is deliberately at block 0 (see below).

**3. Withdrawals root.** reth-bsc writes an empty withdrawals root whenever
Shanghai is active at block 0; go-bsc writes one only for non-BSC chains:

```go
if conf.IsNotInBSC() && conf.IsShanghai(num, g.Timestamp) {
    head.WithdrawalsHash = &types.EmptyWithdrawalsHash
```

`shanghaiTime: 0` therefore produced a genesis hash mismatch on its own. It is
set to the same past timestamp as the rest of the ladder.

**4. `extraData` layout.** `lubanBlock: 0` makes Parlia read the post-`Luban`
record at block 0: `vanity(32) | count(1) | count * (address(20) | vote(48))`,
the turn-length byte that `Bohr` adds, and a 65-byte seal — **167 bytes** for one
validator. The pre-`Luban` record was 117 bytes
(`vanity(32) | address(20) | seal(65)`); parsing one with the other's layout
fails as `unauthorized validator` or `invalid validators bytes`.

The vote field is the validator's BLS public key: blst's `sk_to_pk()` for the
key `privnet.sh` passes as `--bls.private-key` (`0x0707..07`). It has to match,
or the attestations the miner signs are dropped as unauthorized, which is why
`build-genesis.py` pins it as `VOTE_ADDRESS` next to the key's comment.

## Fork ordering is strict

go-bsc rejects a genesis whose forks are not a contiguous ascending chain, and
it **defaults any missing timestamp fork to 0** once London is on. So there is
no such thing as a partial ladder past London: either the whole chain is
present and ordered, or `geth init` fails with e.g.

```
unsupported fork ordering: feynmanFixTime not enabled, but cancunTime enabled at timestamp 0
```

Luban at 0 tightens this further: every block fork before it must also be 0,
and the deferred tail must stay non-decreasing. Two orderings that look
harmless both fail `geth init`:

```
unsupported fork ordering: planckBlock enabled at block 5, but lubanBlock enabled at block 0
unsupported fork ordering: fermiTime enabled at timestamp 9999999999, but osakaTime enabled at timestamp 1754967081
```

That is why `build-genesis.py` collapses `mirrorSyncBlock` through
`planckBlock` to 0, and why a profile always defers a *suffix* of the ladder:
`classic` defers Lorentz onwards, `fast` defers Osaka onwards. The deferral is
also what keeps `classic` on 3-second blocks. Lorentz, Maxwell and Fermi shrink
Parlia's block interval to 1.5 s, 0.75 s and 0.45 s, and both clients read them
from the `lorentzTime`/`maxwellTime`/`fermiTime` keys of a custom genesis. reth-bsc
does so in `chainspec/parser.rs`, and its snapshot then switches `block_interval`
(`snapshot.rs`) and the epoch length (200, then 500 from block 500, then 1000 from
block 1000). Measured on reth-bsc with `genesis-fast.json`: consecutive blocks are
450 ms apart (the millisecond part is in `mixHash`, per BEP-520), and finality holds
past blocks 500 and 1000.

A `blobSchedule` entry is required for every fork the timestamp ladder activates
(`invalid chain configuration: missing entry for fork "cancun" in blobSchedule`).
BSC keeps Cancun's parameters on every later fork, which is what the generated
`cancun` / `prague` / `osaka` entries say.

## The validator set lives in two places

A private network needs each validator in both, and seeding one a *second* way on
top silently breaks cross-client validation:

| Where | What | Symptom if missing |
|---|---|---|
| genesis `extraData` | seeds block 0's snapshot: validators and BLS vote addresses | `invalid validators bytes` / `unauthorized validator` from the start |
| `BSCValidatorSet` at `0x1000` | what `getMiningValidators()` returns at every epoch boundary: validators *and vote addresses* | `unauthorized validator` at block 201; with a wrong vote address, no finality from block 200 |

The second one is the awkward one. `bsc-genesis-contract` ships `0x1000` with the
dev validator baked into `INIT_VALIDATORSET_BYTES`, the RLP blob
`[0, [[consensus, bscFee, fee, votingPower, blsKey]]]` that `init()` decodes at
block 1. It is a Solidity `constant`, so it is compiled into the code and cannot
be set from the genesis. For one validator, `build-genesis.py` rewrites the blob
in place: ours replaces upstream's address (three times) and BLS key (once). The
result has the same length, so every RLP length prefix stays valid, and it is
byte-identical to what recompiling from a patched `scripts/validators.js` would
produce.

### The BLS key in the blob is what fast finality runs on

Earlier versions substituted only the address and left upstream's BLS key
(`0x85e6972f…cb5588`) in the blob. Blocks 1-199 still voted, because the genesis
`extraData` carries our key (`0xa4cafe0e…f41705`). From the epoch-200 header on,
the snapshot takes its vote addresses from the contract, which held upstream's
key. reth-bsc's vote producer then found its own key outside the set and
skipped every vote: `skip vote production reason="not-validator"` in the miner's
debug log, and `cannot collect enough votes` on every block that the
long-running chain's current log covers (block 290 onwards). `eth_getBlockByNumber("finalized")` stayed at genesis
forever. The epoch-200 header of that chain still shows upstream's key. With the
key substituted too, the epoch header carries ours and `finalized` keeps pace with
the head (lag 1) through every later epoch.

`privnet.sh` also checks the other end. It reads the vote address reth-bsc logs at
startup (`Initialized BLS signer successfully`) and refuses to continue unless it
equals the genesis one in `accounts.json`.

Do **not** also seed `currentValidatorSet` in genesis storage for a profile whose
blob already registers the validator: `init()` *appends*, so the set ends up with
two entries holding the same address, and the two clients then disagree about
whether a block is valid at all (next section).

### Three validators: an empty blob plus seeded storage

Three validators do not fit in the 120 bytes the compiled code copies (the
`CODECOPY` length is a `PUSH1`, and the memory allocation another), so `multi`
cannot rewrite the blob. Instead it rewrites the blob to `[0, [], <zero padding>]`,
which `decodeValidatorSet` accepts as a valid package with no validators. `init()`
therefore registers nothing and only sets `alreadyInit`, and the set is seeded
directly in the contract's genesis storage:

| Slot | Variable | Seeded value |
|---|---|---|
| 1 | `Validator[] currentValidatorSet` | length 3; entry *i* at `keccak(1) + 4i`: consensus, fee, `BBCFee \| votingPower << 160` |
| 4 | `mapping(address => uint256) currentValidatorSetMap` | `keccak(addr . 4)` = index + 1 |
| 11 | `ValidatorExtra[] validatorExtraSet` | length 3; `voteAddress` (48-byte `bytes`) at `keccak(11) + 22i + 2`, data at its `keccak` |
| 23 | `uint256 turnLength` | 4 (`getTurnLength()` returns 1 while it is 0) |

Slot 0 is `System.alreadyInit`, so every variable sits one slot later than
`contracts/BSCValidatorSet.sol` suggests. The layout was checked against the
deployed bytecode, not the source. A throwaway anvil instance got `0x1000`'s
patched code and the seeded storage, `init()` ran, and the contract then returned
the three validators and their vote addresses from `getMiningValidators()`, 4 from
`getTurnLength()`, and 1, 2, 3 from `currentValidatorSetMap()`. A coinbase
`deposit()` credited the right entry. On reth-bsc the epoch-200 header carries all
three validators, and from block 204 each validator seals 4 blocks in a row, with
finality intact. Storage keys and values carry the `0x` prefix:
`common.Hash.UnmarshalText` in go-bsc rejects them without it. The profile has not
been run on go-bsc, because no go-bsc binary was available.

## Why one validator, and not two copies of it

A validator set containing the same address twice is rejected by reth-bsc and
accepted by go-bsc, from block 202 on:

```
BSC validation error: proposer 0x19E7E376… is over limit
```

The two clients size their over-signing window differently, and the difference
only shows up when the set has duplicates:

```go
// go-bsc: Snapshot.Validators is a map[common.Address]int
func (s *Snapshot) minerHistoryCheckLen() uint64 {
    return (uint64(len(s.Validators))/2+1)*uint64(s.TurnLength) - 1
}
```

```rust
// reth-bsc: validators is a Vec<Address>
pub fn miner_history_check_len(&self) -> u64 {
    (self.validators.len() / 2 + 1) as u64 * turn - 1
}
```

With `[ours, ours]` and `turn_length = 1`, go-bsc's map holds **one** key so the
formula yields `(1/2+1)*1-1 = 0` — an empty window, `SignRecently` is false, and
the block is accepted. reth-bsc's `Vec` holds **two**, yielding
`(2/2+1)*1-1 = 1` — a window covering the previous block, which our validator
signed, so `sign_recently` is true and `verify_seal` returns
`SignerOverLimit`. Every later block is then discarded with
`links to previously rejected block`.

With exactly one validator both clients compute `0` and agree, so the fix is to
have one — which the bytecode patch alone already gives, since the tool
registers a single validator and nothing else seeds a second. `multi` has three
*distinct* validators, so both clients size the window from the same three
entries.

This also means the single-validator genesis files carry **no `storage`** at all
(only `genesis-multi.json` seeds `0x1000`, as above), which is worth noting
because getting it wrong is silent: `currentValidatorSet` sits at storage
slot **1**, not slot 0 as `contracts/BSCValidatorSet.sol` implies (the shipped
bytecode was compiled from a source with one more leading state variable than
the checked-in `.sol`), and genesis storage keys need a `0x` prefix or
`common.Hash.UnmarshalText` rejects them and drops the entry. A genesis with
storage that does nothing is indistinguishable from one without it until a node
refuses to seal.

## What the chain looks like

| | |
|---|---|
| chain id | 1337 |
| validator | `0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A` (key `0x1111…1111`, **never** on mainnet) |
| vote address | `0xa4cafe0e…f41705` (BLS key `0x0707…0707`, **never** on mainnet) |
| `multi` validators | + `0x1563915e…3115508` (`0x2222…`, BLS `0x0808…`), `0x5CbDd86a…EeC07FB` (`0x3333…`, BLS `0x0909…`) |
| test accounts | builder `0x7564105E…967bDaC` (`0x4444…`), users `0xe1fAE9b4…`, `0xdb2430B4…`, `0xAe72A48c…` (`0x5555…`, `0x6666…`, `0x7777…`) |
| block 0 | pre-London header (`bsc-spec`'s `MuirGlacier` era) with Luban active, vote-bearing `extraData` (167 bytes for one validator, 303 for three) |
| block 1+ | `classic`: `Prague`, with Lorentz onwards deferred to `9999999999`; `fast`: + Fermi; `pasteur`/`multi`: + Pasteur |
| base fee | `0`, constant (`BaseFeeParamsKind::Constant(1, 1)`) |
| epoch / period | `classic`: 200 blocks / 3 s; fast profiles: 200, then 500 (from block 500), then 1000 (from block 1000) / 450 ms |

`privnet.sh` asserts the chain id on every node at startup, because a failed
`geth init` otherwise leaves a partial `geth/` directory that makes the next
run skip init entirely and come up silently on **BSC mainnet**.
