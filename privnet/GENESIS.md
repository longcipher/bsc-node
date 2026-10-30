# The private network's genesis

`genesis.json` is **generated**, not hand-maintained:

```bash
./privnet.sh system-contracts      # refresh from bnb-chain/bsc-genesis-contract
./privnet.sh clean                 # both clients must re-init against a new genesis
./privnet.sh mixed
```

`build-genesis.py` takes the system-contract deployments from
[`bnb-chain/bsc-genesis-contract`](https://github.com/bnb-chain/bsc-genesis-contract)
(`genesis-dev.json`, pinned to a revision) and applies the private network's own
chain id, validator and fork schedule on top.

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

The timestamp ladder is split: the forks that keep 3-second blocks activate at
a past timestamp (QANet's `1754967081`), while `lorentzTime`, `maxwellTime`,
`fermiTime` and everything scheduled after them (`osakaTime`, `mendelTime`,
`pasteurTime`) sit at `9999999999`, past any realistic run. The block rungs are
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
`planckBlock` to 0 and moves `osakaTime`, `mendelTime` and `pasteurTime` to
`9999999999` alongside the deferred `lorentzTime`/`maxwellTime`/`fermiTime`.
The deferral is also what keeps 3-second blocks: Lorentz, Maxwell and Fermi
shrink Parlia's block interval to 1.5s, 0.75s and 0.45s, so a private network
that activated them would stop matching the `period: 3s` the script assumes.

A `blobSchedule` entry is required for every fork the timestamp ladder activates
(`invalid chain configuration: missing entry for fork "cancun" in blobSchedule`).
BSC keeps Cancun's parameters on every later fork, which is what the generated
`cancun` / `prague` / `osaka` entries say.

## The validator set lives in two places

A single-validator private network needs the validator in both, and seeding it a
*third* time silently breaks cross-client validation:

| Where | What | Symptom if missing |
|---|---|---|
| genesis `extraData` | seeds block 0's snapshot | `invalid validators bytes` / `unauthorized validator` from the start |
| `BSCValidatorSetTool` bytecode | the `init()` the system hook calls, which registers the epoch-boundary snapshot | `unauthorized validator` at block 201 |

The second one is the awkward one. `bsc-genesis-contract` ships
`BSCValidatorSetTool` at `0x1000` with the dev validator baked into
`INIT_VALIDATORSET_BYTES` — a Solidity `constant`, so it is compiled into the
code and cannot be set from the genesis. `build-genesis.py` substitutes a
same-length address into it, which leaves every RLP length prefix valid and is
byte-identical to what recompiling from a patched `scripts/validators.js`
would produce.

Do **not** also seed `currentValidatorSet` in genesis storage. Doing so gives a
set of two entries holding the same address, and the two clients then disagree
about whether a block is valid at all.

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
registers a single validator and nothing else seeds a second.

This also means the genesis carries **no `storage`** at all, which is worth
noting because getting it wrong is silent: `currentValidatorSet` sits at storage
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
| block 0 | pre-London header (`bsc-spec`'s `MuirGlacier` era) with Luban active, 167-byte vote-bearing `extraData` |
| block 1+ | `Prague` — Lorentz, Maxwell, Fermi, Osaka, Mendel and Pasteur stay deferred at `9999999999` |
| base fee | `0`, constant (`BaseFeeParamsKind::Constant(1, 1)`) |
| epoch / period | 200 blocks / 3 s |

`privnet.sh` asserts the chain id on every node at startup, because a failed
`geth init` otherwise leaves a partial `geth/` directory that makes the next
run skip init entirely and come up silently on **BSC mainnet**.
