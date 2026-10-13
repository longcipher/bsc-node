#!/usr/bin/env bash
# Regression check for the daily breathe block, the failure that is invisible
# for the first 24 hours and then permanent.
#
# A "breathe block" is one whose parent lies in an earlier UTC day. It is the
# only place `updateValidatorSetV2` runs (`consensus/parlia/feynmanfork.go`),
# and a private network cannot serve that call:
#
#   1. bsc-genesis-contract's genesis-dev.json ships a pre-Feynman
#      `BSCValidatorSet` at 0x1000 whose runtime code has no
#      `updateValidatorSetV2` selector (0x6550d581).
#   2. go-bsc never replaces it: `upgradeBuildInSystemContract` selects its
#      table by genesis hash, custom chains fall to `defaultNet`, and
#      `feynmanUpgrade[defaultNet]` is nil, so `applySystemContractUpgrade`
#      no-ops.
#
# The call therefore hits the contract's dispatcher default, which compiles to
# INVALID, and it does so *inside* `finalizeAndAssemble` -- so no block is built
# at all and the miner retries forever.
#
# Rather than wait a day to observe that, this script shrinks the interval so a
# boundary is crossed within a couple of minutes, then runs the other side:
#
#   case A  interval=120s   must STALL    with `invalid opcode: INVALID`
#   case B  interval=1e15  must PRODUCE  (the default privnet.sh now ships)
#
# Both cases run `privnet.sh` itself, so case B also proves the fix is wired in.
# `isBreatheBlock` is consensus, so every node is given the same interval.
#
# Usage:
#   ./privnet/ab-breathe-interval.sh            # both cases, ~8 minutes
#   ./privnet/ab-breathe-interval.sh --case-b   # only the "must produce" side
#
# Environment: GETH_BIN / RETH_BIN / PRIVNET_DIR, as privnet.sh.
# Exits non-zero if either case does not behave as expected.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRIVNET_DIR="${PRIVNET_DIR:-$SCRIPT_DIR/data}"
STALL_INTERVAL=120
STABLE_INTERVAL=1000000000000000
# Two samples this far apart, with no growth between them, is a stall. The
# network seals roughly every 0.5s, so this is generous on purpose: it must not
# be so tight that a slow start is misread as a dead chain.
SAMPLE_GAP=15
CASE_FILTER="${1:-all}"

log() { echo "[ab] $*"; }
die() { echo "[ab] FAIL: $*" >&2; exit 1; }

rpc() { # rpc <port> <method>
  curl -s -m 10 -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"$2\",\"params\":[],\"id\":1}" \
    "http://127.0.0.1:$1"
}

block_number() { # block_number <port> -> decimal, or -1 when unreachable
  local out
  out=$(rpc "$1" eth_blockNumber | python3 -c \
    'import json,sys; print(int(json.load(sys.stdin)["result"],16))' 2>/dev/null) \
    || out=""
  echo "${out:--1}"
}

# A stall is "height did not move" AND "the miner logged the breathe-block
# failure". The log signature is what distinguishes a genuine stall from a
# network that is merely slow to start.
stall_signature_count() { # stall_signature_count -> occurrences
  local n="$PRIVNET_DIR/go1/node.log"
  [ -f "$n" ] || { echo 0; return; }
  grep -c 'apply message failed' "$n" || true
}

start_network() { # start_network <interval>
  local interval="$1"
  # `privnet.sh clean` stops the nodes and wipes the four datadirs, which is the
  # same teardown privnet.sh uses between runs. It never removes the directory
  # itself, so a caller-set PRIVNET_DIR cannot be destroyed from here.
  "$SCRIPT_DIR/privnet.sh" clean >/dev/null 2>&1 || true
  log "starting mixed network, BREATHE_BLOCK_INTERVAL=$interval"
  BREATHE_BLOCK_INTERVAL="$interval" "$SCRIPT_DIR/privnet.sh" mixed \
    >"$PRIVNET_DIR/startup.log" 2>&1 || true
  # privnet.sh exits 0 on success; a missing binary is fatal here.
  [ -f "$PRIVNET_DIR/go1/pid" ] || die "network did not start (see $PRIVNET_DIR/startup.log)"
}

height_over() { # height_over <seconds> -> blocks sealed during that window
  local before after
  before=$(block_number 8545)
  sleep "$1"
  after=$(block_number 8545)
  echo $(( after - before ))
}

expect_stall() {
  log "case A: interval=${STALL_INTERVAL}s, expecting a stall"
  start_network "$STALL_INTERVAL"

  # The genesis lands block 0 in 2020, so block 1 already crosses a boundary,
  # and `interval` then creates one every ${STALL_INTERVAL}s. Allow a few
  # boundaries' worth of time for the first one to bite.
  local i
  for i in 1 2 3 4 5 6 7 8; do
    sleep 30
    local sigs
    sigs=$(stall_signature_count)
    if [ "$sigs" -gt 0 ]; then
      log "stalled after ~$(( i * 30 ))s (${sigs}x 'apply message failed')"
      break
    fi
  done

  local growth sigs
  growth=$(height_over "$SAMPLE_GAP")
  sigs=$(stall_signature_count)
  log "height growth over ${SAMPLE_GAP}s = ${growth} blocks, failure log lines = ${sigs}"

  [ "$sigs" -gt 0 ] || die "expected the breathe-block failure in go1's log"
  [ "$growth" -le 0 ] || die "expected a stalled chain, but it sealed ${growth} blocks"
  log "PASS: the chain is dead, exactly as the bug predicts"
}

expect_production() {
  log "case B: interval=${STABLE_INTERVAL}s (the privnet.sh default), expecting blocks"
  start_network "$STABLE_INTERVAL"

  local growth sigs
  growth=$(height_over "$SAMPLE_GAP")
  sigs=$(stall_signature_count)
  log "height growth over ${SAMPLE_GAP}s = ${growth} blocks, failure log lines = ${sigs}"

  [ "$growth" -gt 0 ] || die "the chain does not produce blocks; the fix is not working"
  [ "$sigs" -eq 0 ] || die "breathe-block failure still logged ${sigs} times"

  # Consensus is the other half of the fix: a node that disagrees about the
  # interval would reject these blocks, so all four must reach the same tip.
  local tip ref hash mismatches=0
  tip=$(python3 -c '
import json,subprocess,sys
lo=1<<62
for p in (8545,8547,8549,8553):
    out=subprocess.run(["curl","-s","-m","10","-X","POST","-H","content-type: application/json",
        "-d",json.dumps({"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}),
        f"http://127.0.0.1:{p}"],capture_output=True,text=True).stdout
    try: lo=min(lo,int(json.loads(out)["result"],16))
    except Exception: pass
print(lo if lo < 1<<62 else 0)')
  [ "$tip" -gt 0 ] || die "no node reported a height"
  ref=$(python3 -c "
import json,subprocess
out=subprocess.run(['curl','-s','-m','10','-X','POST','-H','content-type: application/json',
  '-d',json.dumps({'jsonrpc':'2.0','method':'eth_getBlockByNumber',
  'params':[hex($tip),False],'id':1}),'http://127.0.0.1:8545'],
  capture_output=True,text=True).stdout
b=json.loads(out)['result']
print(b['hash'], b['stateRoot'])")
  for port in 8545 8547 8549 8553; do
    hash=$(python3 -c "
import json,subprocess
out=subprocess.run(['curl','-s','-m','10','-X','POST','-H','content-type: application/json',
  '-d',json.dumps({'jsonrpc':'2.0','method':'eth_getBlockByNumber',
  'params':[hex($tip),False],'id':1}),'http://127.0.0.1:$port'],
  capture_output=True,text=True).stdout
b=json.loads(out)['result']
print(b['hash'], b['stateRoot'])" 2>/dev/null) || hash="<unreachable>"
    [ "$hash" = "$ref" ] || { log "  port $port disagrees: $hash"; mismatches=$(( mismatches + 1 )); }
  done
  [ "$mismatches" -eq 0 ] || die "$mismatches node(s) diverged at height $tip -- the interval is consensus"

  log "PASS: ${growth} blocks sealed, all four nodes agree at height $tip"
}

case "$CASE_FILTER" in
  --case-b) expect_production ;;
  all)      expect_stall; expect_production ;;
  *) die "unknown argument: $CASE_FILTER (expected --case-b, or nothing)" ;;
esac

# Leave the tree the way the user left it: no network running, no data.
"$SCRIPT_DIR/privnet.sh" clean >/dev/null 2>&1 || true
log "done; network stopped and its datadirs wiped"
log "start one again with: ./privnet.sh mixed"
