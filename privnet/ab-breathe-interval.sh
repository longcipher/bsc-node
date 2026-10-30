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
#   case B  interval=1e15  must PRODUCE  (the default privnet.sh ships)
#
# Both cases run `privnet.sh mixed` itself, so case B also proves the fix is
# wired in. `isBreatheBlock` is consensus, so every node gets the same interval.
#
# The network runs in a private temporary directory on its own ports, so a
# private network already running from privnet/data (or anywhere else) is never
# touched, and the trap tears everything down however the script exits.
#
# Usage:
#   ./privnet/ab-breathe-interval.sh            # both cases, ~8 minutes
#   ./privnet/ab-breathe-interval.sh --case-b   # only the "must produce" side
#
# Environment:
#   GETH_BIN / RETH_BIN   as privnet.sh (go-bsc is required: the stall is
#                         observed on the Go miner)
#   AB_PRIVNET_DIR        runtime directory [a fresh mktemp -d, removed on exit]
#   AB_PORT_OFFSET        PORT_OFFSET for the network [2000]
#   AB_KEEP=1             keep the directory (logs) instead of removing it
#
# Exits 0 when both cases behave, 1 when either does not, 77 when go-bsc is not
# installed (skipped).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PRIVNET="$SCRIPT_DIR/privnet.sh"
STALL_INTERVAL=120
STABLE_INTERVAL=1000000000000000
# Two samples this far apart, with no growth between them, is a stall. The
# network seals roughly every 3s, so this is generous on purpose: it must not
# be so tight that a slow start is misread as a dead chain.
SAMPLE_GAP=15
CASE_FILTER="${1:-all}"
AB_PORT_OFFSET="${AB_PORT_OFFSET:-2000}"
AB_KEEP="${AB_KEEP:-0}"

log() { echo "[ab] $*"; }
die() { echo "[ab] FAIL: $*" >&2; exit 1; }

case "$CASE_FILTER" in
  all|--case-b) ;;
  *) die "unknown argument: $CASE_FILTER (expected --case-b, or nothing)" ;;
esac
[[ "$AB_PORT_OFFSET" =~ ^-?[0-9]+$ ]] || die "AB_PORT_OFFSET must be an integer"

if ! command -v "${GETH_BIN:-geth}" >/dev/null 2>&1; then
  echo "[ab] SKIP: go-bsc binary '${GETH_BIN:-geth}' not found (set GETH_BIN); the breathe-block stall is observed on a Go miner" >&2
  exit 77
fi

CREATED_DIR=0
if [ -n "${AB_PRIVNET_DIR:-}" ]; then
  AB_DIR="${AB_PRIVNET_DIR%/}"
  [ "$AB_DIR" != "$SCRIPT_DIR/data" ] || die "AB_PRIVNET_DIR must not be the shared privnet/data"
  mkdir -p "$AB_DIR"
else
  AB_DIR="$(mktemp -d "${TMPDIR:-/tmp}/privnet-ab.XXXXXX")"
  CREATED_DIR=1
fi
export PRIVNET_DIR="$AB_DIR" PORT_OFFSET="$AB_PORT_OFFSET" IPC_DIR="$AB_DIR/ipc"
[ ! -f "$AB_DIR/network.env" ] || die "$AB_DIR already holds a network; pick an empty AB_PRIVNET_DIR"

cleanup() {
  local rc=$?
  "$PRIVNET" clean >/dev/null 2>&1 || echo "[ab] WARNING: privnet.sh clean failed in $AB_DIR" >&2
  if [ "$CREATED_DIR" = 1 ] && [ "$AB_KEEP" != 1 ]; then
    rm -rf "$AB_DIR"
  else
    log "kept $AB_DIR"
  fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

GO1_HTTP=$((8545 + AB_PORT_OFFSET))
NODE_PORTS=("$GO1_HTTP" $((8547 + AB_PORT_OFFSET)) $((8549 + AB_PORT_OFFSET)) $((8553 + AB_PORT_OFFSET)))

rpc() { # rpc <port> <method> [params-json]
  curl -s -m 10 -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"$2\",\"params\":${3:-[]},\"id\":1}" \
    "http://127.0.0.1:$1" 2>/dev/null || true
}

block_number() { # block_number <port> -> decimal; an unreachable node is a failure, not a stall
  local out
  out=$(rpc "$1" eth_blockNumber | python3 -I -c '
import json, sys
try:
    print(int(json.load(sys.stdin)["result"], 16))
except Exception:
    pass')
  [ -n "$out" ] || die "the node on port $1 is unreachable (crashed?); see $AB_DIR/*/node.log"
  echo "$out"
}

block_id() { # block_id <port> <height> -> "hash stateRoot"
  rpc "$1" eth_getBlockByNumber "[\"$(printf '0x%x' "$2")\",false]" | python3 -I -c '
import json, sys
try:
    b = json.load(sys.stdin)["result"]
    print(b["hash"], b["stateRoot"])
except Exception:
    pass'
}

# A stall is "height did not move" AND "the miner logged the breathe-block
# failure". The log signature is what distinguishes a genuine stall from a
# network that is merely slow to start.
stall_signature_count() {
  local n="$AB_DIR/go1/node.log"
  [ -f "$n" ] || { echo 0; return; }
  grep -c 'apply message failed' "$n" || true
}

start_network() { # start_network <interval> <health: 1|0>
  "$PRIVNET" clean >/dev/null 2>&1 || true
  log "starting mixed network in $AB_DIR (port offset $AB_PORT_OFFSET), BREATHE_BLOCK_INTERVAL=$1"
  # Case A must not be judged by the start-up health check: the stall it waits
  # for can begin within the check's window.
  if ! BREATHE_BLOCK_INTERVAL="$1" START_HEALTH="$2" "$PRIVNET" mixed >"$AB_DIR/startup.log" 2>&1; then
    tail -n 30 "$AB_DIR/startup.log" >&2 || true
    die "network did not start (see $AB_DIR/startup.log)"
  fi
}

height_over() { # height_over <seconds> -> blocks sealed during that window
  local before after
  before=$(block_number "$GO1_HTTP")
  sleep "$1"
  after=$(block_number "$GO1_HTTP")
  echo $((after - before))
}

expect_stall() {
  log "case A: interval=${STALL_INTERVAL}s, expecting a stall"
  start_network "$STALL_INTERVAL" 0

  # The genesis lands block 0 in 2020, so block 1 already crosses a boundary,
  # and `interval` then creates one every ${STALL_INTERVAL}s. Allow a few
  # boundaries' worth of time for the first one to bite.
  local i sigs growth
  for i in 1 2 3 4 5 6 7 8; do
    sleep 30
    block_number "$GO1_HTTP" >/dev/null # the miner must still be alive
    sigs=$(stall_signature_count)
    if [ "$sigs" -gt 0 ]; then
      log "stalled after ~$((i * 30))s (${sigs}x 'apply message failed')"
      break
    fi
  done

  growth=$(height_over "$SAMPLE_GAP")
  sigs=$(stall_signature_count)
  log "height growth over ${SAMPLE_GAP}s = ${growth} blocks, failure log lines = ${sigs}"

  [ "$sigs" -gt 0 ] || die "expected the breathe-block failure in go1's log"
  [ "$growth" -le 0 ] || die "expected a stalled chain, but it sealed ${growth} blocks"
  log "PASS: the chain is dead, exactly as the bug predicts"
}

expect_production() {
  log "case B: interval=${STABLE_INTERVAL}s (the privnet.sh default), expecting blocks"
  start_network "$STABLE_INTERVAL" 1

  local growth sigs
  growth=$(height_over "$SAMPLE_GAP")
  sigs=$(stall_signature_count)
  log "height growth over ${SAMPLE_GAP}s = ${growth} blocks, failure log lines = ${sigs}"

  [ "$growth" -gt 0 ] || die "the chain does not produce blocks; the fix is not working"
  [ "$sigs" -eq 0 ] || die "breathe-block failure still logged ${sigs} times"

  # Consensus is the other half of the fix: a node that disagrees about the
  # interval would reject these blocks, so all four must reach the same tip.
  local tip=-1 port h ref id mismatches=0
  for port in "${NODE_PORTS[@]}"; do
    h=$(block_number "$port")
    if [ "$tip" -lt 0 ] || [ "$h" -lt "$tip" ]; then tip="$h"; fi
  done
  [ "$tip" -gt 0 ] || die "no node reported a height"
  ref=$(block_id "$GO1_HTTP" "$tip")
  [ -n "$ref" ] || die "go1 has no block at height $tip"
  for port in "${NODE_PORTS[@]}"; do
    id=$(block_id "$port" "$tip")
    [ -n "$id" ] || die "the node on port $port has no block at height $tip"
    if [ "$id" != "$ref" ]; then
      log "  port $port disagrees: $id"
      mismatches=$((mismatches + 1))
    fi
  done
  [ "$mismatches" -eq 0 ] || die "$mismatches node(s) diverged at height $tip -- the interval is consensus"

  log "PASS: ${growth} blocks sealed, all four nodes agree at height $tip"
}

case "$CASE_FILTER" in
  --case-b) expect_production ;;
  all) expect_stall; expect_production ;;
esac
log "done; the network and its directory are torn down on exit"
