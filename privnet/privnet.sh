#!/usr/bin/env bash
# Private BSC networks for client interop and block-builder end-to-end tests.
#
#   ./privnet.sh MODE [start]      start a network (default: mixed)
#   ./privnet.sh ACTION            act on the network recorded in $PRIVNET_DIR
#
# Modes (genesis profile in brackets; see GENESIS.md):
#   go        go1 (miner) + go2                              [classic, 3 s]
#   rust      rust1 (miner) + rust2                          [classic, 3 s]
#   mixed     go1 (miner) + go2 + rust1 + rust2              [classic, 3 s]
#   fast      rust1 (miner) + rust2, Lorentz/Maxwell/Fermi   [fast, 450 ms]
#   pasteur   as fast, plus Osaka/Mendel/Pasteur (BEP-675)   [pasteur, 450 ms]
#   multi     rust1..rust3, three validators, turn length 4  [multi, 450 ms]
#
# Actions:
#   status            block / finalized / peers per node (no waiting)
#   health            verify the network: processes, chain id, head advances,
#                     peers, hash agreement, finality; exits non-zero if not
#   env               print shell exports for a builder e2e script
#   accounts          print the DEV-ONLY validator / builder / test accounts
#   stop              stop the nodes recorded in $PRIVNET_DIR
#   clean             stop, then wipe chain data, vote journals and IPC sockets
#   handover-to-rust  mixed only: stop go1 mining, restart rust1 as the miner
#   handover-to-go    mixed only: restart rust1 as a follower, resume go1
#   system-contracts  regenerate every genesis file (build-genesis.py)
#   genesis-check     verify the committed genesis files are reproducible
#
# Environment:
#   GETH_BIN          go-bsc binary                 [geth on PATH]
#   RETH_BIN          reth-bsc binary               [reth-bsc on PATH]
#   PRIVNET_DIR       runtime directory             [<script-dir>/data]
#   PORT_OFFSET       added to every port below     [0]
#   IPC_DIR           IPC sockets (<node>.ipc)      [$PRIVNET_DIR/ipc]
#   PROFILE           genesis profile for go|rust|mixed: classic|fast|pasteur
#   BREATHE_BLOCK_INTERVAL   breathe-block interval, seconds [1e15: never]
#   VOTE_WARMUP_BLOCKS       blocks a miner waits before voting [3]
#   ETH_PROOF_WINDOW         reth --rpc.eth-proof-window [100000]
#   RETH_EXTRA_ARGS          extra arguments for every reth-bsc node
#   HEALTH_BLOCKS     blocks the head must advance  [5]
#   HEALTH_TIMEOUT    seconds for head / peers      [90 at 3 s, 45 at 450 ms]
#   FINALITY_TIMEOUT  seconds for finality          [120 at 3 s, 60 at 450 ms]
#   REQUIRE_FINALITY  1/0; default 1 for rust-only modes, 0 for go/mixed
#   START_HEALTH      0 skips the health check at the end of start [1]
#   KEEP_ON_FAILURE   1 leaves nodes running when start fails [0]
#   STOP_TIMEOUT      seconds to wait for a node to exit [60]
#
# Ports (before PORT_OFFSET):
#   node    p2p    http   authrpc  mev-grpc   ipc
#   go1     30311  8545   -        -          $IPC_DIR/go1.ipc
#   go2     30312  8547   -        -          $IPC_DIR/go2.ipc
#   rust1   30313  8549   8555     8552       $IPC_DIR/rust1.ipc
#   rust2   30314  8553   8556     8554       $IPC_DIR/rust2.ipc
#   rust3   30315  8557   8558     8559       $IPC_DIR/rust3.ipc
#
# Exit codes: 0 ok, 1 failure, 2 usage, 77 skipped (a required binary, such as
# go-bsc's geth for go/mixed, is not installed).
#
# reth-bsc must contain the private-network fixes in docs/privnet.md. Until
# bnb-chain/reth-bsc#546 merges, build https://github.com/akfork/reth-bsc
# (branch fix/fix-privnet) with `cargo build --release`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKIP_EXIT=77

usage() {
  sed -n '2,/^set -euo pipefail/{/^set -euo/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

ARG1="${1:-mixed}"
case "$ARG1" in
  go|rust|mixed|fast|pasteur|multi)
    MODE="$ARG1"; ACTION="${2:-start}"
    [ "$ACTION" = start ] || { usage >&2; exit 2; } ;;
  stop|status|health|env|accounts|clean|handover-to-rust|handover-to-go|system-contracts|genesis-check)
    MODE=""; ACTION="$ARG1" ;;
  -h|--help|help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac

PRIVNET_DIR="${PRIVNET_DIR:-$SCRIPT_DIR/data}"
case "$PRIVNET_DIR" in /*) ;; *) PRIVNET_DIR="$PWD/$PRIVNET_DIR" ;; esac
PRIVNET_DIR="${PRIVNET_DIR%/}"
GETH_BIN="${GETH_BIN:-geth}"
RETH_BIN="${RETH_BIN:-reth-bsc}"
PORT_OFFSET="${PORT_OFFSET:-0}"
IPC_DIR="${IPC_DIR:-$PRIVNET_DIR/ipc}"
VOTE_WARMUP_BLOCKS="${VOTE_WARMUP_BLOCKS:-3}"
ETH_PROOF_WINDOW="${ETH_PROOF_WINDOW:-100000}"
HEALTH_BLOCKS="${HEALTH_BLOCKS:-5}"
START_HEALTH="${START_HEALTH:-1}"
KEEP_ON_FAILURE="${KEEP_ON_FAILURE:-0}"
STOP_TIMEOUT="${STOP_TIMEOUT:-60}"
ACCOUNTS="$SCRIPT_DIR/accounts.json"
STATE="$PRIVNET_DIR/network.env"
GETH_PASSWORD="testpassword123"

# A "breathe block" is one whose parent lies in an earlier UTC day
# (`isBreatheBlock` in consensus/parlia). It is the only place
# `updateValidatorSetV2` runs, so it is the one system transaction a private
# network cannot serve: bsc-genesis-contract's genesis-dev.json ships a
# pre-Feynman `BSCValidatorSet` at 0x1000 that has no `updateValidatorSetV2`
# selector, and neither client replaces it on a custom chain (go-bsc's
# `feynmanUpgrade[defaultNet]` is nil, so `applySystemContractUpgrade` no-ops).
# The call then halts with `invalid opcode: INVALID` *inside block assembly*,
# so the miner can never seal again and the chain is dead until the datadir is
# wiped. Crossing a UTC day is enough, running or not.
#
# `isBreatheBlock` is consensus, so every node must derive the same answer:
#   go-bsc   : --override.breatheblockinterval <seconds>
#   reth-bsc : BREATHE_BLOCK_INTERVAL=<seconds>
#
# A private network's validator set never changes, so the daily refresh buys
# nothing; an interval larger than any timestamp turns the rule off. Set
# BREATHE_BLOCK_INTERVAL=86400 to restore the production schedule. Never change
# it on an already-running chain: the block layout forks.
BREATHE_BLOCK_INTERVAL="${BREATHE_BLOCK_INTERVAL:-1000000000000000}"

log() { echo "[privnet] $*"; }
warn() { echo "[privnet] WARNING: $*" >&2; }
die() { echo "[privnet] ERROR: $*" >&2; exit 1; }
skip() { echo "[privnet] SKIP: $*" >&2; exit "$SKIP_EXIT"; }

banner() {
  cat >&2 <<'EOF'
[privnet] ======================================================================
[privnet]  DEV-ONLY NETWORK. Every validator, BLS, builder and test-account key
[privnet]  used here is PUBLIC (privnet/accounts.json). Never fund or use them on
[privnet]  BSC mainnet (56) or testnet (97); this script refuses those chain ids.
[privnet] ======================================================================
EOF
}

[[ "$PORT_OFFSET" =~ ^-?[0-9]+$ ]] || die "PORT_OFFSET must be an integer, got '$PORT_OFFSET'"
for v in HEALTH_BLOCKS STOP_TIMEOUT VOTE_WARMUP_BLOCKS ETH_PROOF_WINDOW; do
  [[ "${!v}" =~ ^[0-9]+$ ]] || die "$v must be a non-negative integer, got '${!v}'"
done
[ "$VOTE_WARMUP_BLOCKS" -ge 1 ] || die "VOTE_WARMUP_BLOCKS must be >= 1 (reth-bsc ignores 0)"

# ---------------------------------------------------------------------------
# JSON-RPC
# ---------------------------------------------------------------------------
rpc() { # rpc <port> <method> [params-json] -> raw response, empty if unreachable
  curl -s -m "${RPC_TIMEOUT:-5}" -X POST -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"method\":\"$2\",\"params\":${3:-[]},\"id\":1}" \
    "http://127.0.0.1:$1" 2>/dev/null || true
}

hexnum() { # stdin: JSON-RPC response -> decimal of its hex result, or nothing
  python3 -I -c '
import json, sys
try:
    print(int(json.load(sys.stdin)["result"], 16))
except Exception:
    pass'
}

chain_id_of() { rpc "$1" eth_chainId | hexnum; }
head_of() { rpc "$1" eth_blockNumber | hexnum; }
peers_of() { rpc "$1" net_peerCount | hexnum; }

block_field() { # block_field <port> <tag|0xN> <field> -> value (number as decimal)
  rpc "$1" eth_getBlockByNumber "[\"$2\",false]" | python3 -I -c '
import json, sys
try:
    value = json.load(sys.stdin)["result"][sys.argv[1]]
    print(int(value, 16) if sys.argv[1] == "number" else value)
except Exception:
    pass' "$3"
}

# ---------------------------------------------------------------------------
# DEV-key fencing: nothing here may run against BSC mainnet or testnet.
# ---------------------------------------------------------------------------
fence_chain_id() { # fence_chain_id <chain-id> <what>
  case "$1" in
    56) die "REFUSING: $2 is on chain id 56 (BSC mainnet). This harness uses DEV-ONLY public keys." ;;
    97) die "REFUSING: $2 is on chain id 97 (BSC testnet). This harness uses DEV-ONLY public keys." ;;
  esac
  return 0
}

fence_port() { # fence_port <port> <what>: refuse if a 56/97 node answers there
  local id
  id=$(chain_id_of "$1")
  if [ -n "$id" ]; then fence_chain_id "$id" "$2 (port $1)"; fi
  return 0
}

# ---------------------------------------------------------------------------
# Topology, ports and profiles
# ---------------------------------------------------------------------------
node_ports() { # node_ports <node>: sets P2P HTTP AUTH GRPC (PORT_OFFSET applied; 0 = unused)
  local p h a g
  case "$1" in
    go1) p=30311 h=8545 a=0 g=0 ;;
    go2) p=30312 h=8547 a=0 g=0 ;;
    rust1) p=30313 h=8549 a=8555 g=8552 ;;
    rust2) p=30314 h=8553 a=8556 g=8554 ;;
    rust3) p=30315 h=8557 a=8558 g=8559 ;;
    *) die "unknown node: $1" ;;
  esac
  P2P=$((p + PORT_OFFSET))
  HTTP=$((h + PORT_OFFSET))
  AUTH=0 GRPC=0
  if [ "$a" -ne 0 ]; then AUTH=$((a + PORT_OFFSET)); fi
  if [ "$g" -ne 0 ]; then GRPC=$((g + PORT_OFFSET)); fi
  local port
  for port in "$P2P" "$HTTP" "$AUTH" "$GRPC"; do
    [ "$port" -eq 0 ] || { [ "$port" -ge 1024 ] && [ "$port" -le 65535 ]; } ||
      die "PORT_OFFSET=$PORT_OFFSET puts a port of $1 out of range ($port)"
  done
}

ipc_path() { printf '%s/%s.ipc\n' "$IPC_DIR" "$1"; }
node_dir() { printf '%s/%s\n' "$PRIVNET_DIR" "$1"; }
is_go() { case "$1" in go*) return 0 ;; *) return 1 ;; esac; }

genesis_for() { # genesis_for <profile> -> genesis file
  case "$1" in
    classic) echo "$SCRIPT_DIR/genesis.json" ;;
    fast) echo "$SCRIPT_DIR/genesis-fast.json" ;;
    pasteur) echo "$SCRIPT_DIR/genesis-pasteur.json" ;;
    multi) echo "$SCRIPT_DIR/genesis-multi.json" ;;
    *) die "unknown profile: $1 (expected classic, fast, pasteur or multi)" ;;
  esac
}

block_ms_for() { # block_ms_for <profile> -> Parlia block interval in ms
  case "$1" in classic) echo 3000 ;; *) echo 450 ;; esac
}

select_mode() { # select_mode <mode>: sets NET_* for a fresh start
  NET_MODE="$1"
  case "$1" in
    go) NET_NODES="go1 go2" NET_MINERS="go1" ;;
    rust|fast|pasteur) NET_NODES="rust1 rust2" NET_MINERS="rust1" ;;
    mixed) NET_NODES="go1 go2 rust1 rust2" NET_MINERS="go1" ;;
    multi) NET_NODES="rust1 rust2 rust3" NET_MINERS="rust1 rust2 rust3" ;;
  esac
  case "$1" in
    go|rust|mixed)
      NET_PROFILE="${PROFILE:-classic}"
      [ "$NET_PROFILE" != multi ] || die "PROFILE=multi needs the multi mode (three validators)" ;;
    *)
      NET_PROFILE="$1"
      [ -z "${PROFILE:-}" ] || [ "$PROFILE" = "$1" ] || die "mode $1 uses profile $1, not PROFILE=$PROFILE" ;;
  esac
  NET_GENESIS="$(genesis_for "$NET_PROFILE")"
  NET_BLOCK_MS="$(block_ms_for "$NET_PROFILE")"
  case "$1" in go|mixed) NET_REQUIRE_FINALITY="${REQUIRE_FINALITY:-0}" ;; *) NET_REQUIRE_FINALITY="${REQUIRE_FINALITY:-1}" ;; esac
  NET_PORT_OFFSET="$PORT_OFFSET"
  NET_IPC_DIR="$IPC_DIR"
}

write_state() {
  mkdir -p "$PRIVNET_DIR"
  {
    echo "# written by privnet.sh; read by its stop/status/health/env actions"
    printf 'NET_MODE=%q\nNET_PROFILE=%q\nNET_GENESIS=%q\nNET_GENESIS_SHA=%q\n' \
      "$NET_MODE" "$NET_PROFILE" "$NET_GENESIS" "$NET_GENESIS_SHA"
    printf 'NET_CHAIN_ID=%q\nNET_BLOCK_MS=%q\nNET_NODES=%q\nNET_MINERS=%q\n' \
      "$NET_CHAIN_ID" "$NET_BLOCK_MS" "$NET_NODES" "$NET_MINERS"
    printf 'NET_PORT_OFFSET=%q\nNET_IPC_DIR=%q\nNET_REQUIRE_FINALITY=%q\n' \
      "$NET_PORT_OFFSET" "$NET_IPC_DIR" "$NET_REQUIRE_FINALITY"
  } >"$STATE.tmp"
  mv "$STATE.tmp" "$STATE"
}

load_state() { # load_state [optional]: read $STATE; ports/IPC follow the recorded network
  if [ ! -f "$STATE" ]; then
    [ "${1:-}" = optional ] && return 1
    die "no network recorded in $PRIVNET_DIR (start one first, or set PRIVNET_DIR)"
  fi
  # shellcheck source=/dev/null
  . "$STATE"
  if [ "$PORT_OFFSET" != "$NET_PORT_OFFSET" ] && [ "$PORT_OFFSET" != 0 ]; then
    warn "PORT_OFFSET=$PORT_OFFSET ignored; the network in $PRIVNET_DIR was started with $NET_PORT_OFFSET"
  fi
  PORT_OFFSET="$NET_PORT_OFFSET"
  IPC_DIR="$NET_IPC_DIR"
  return 0
}

health_defaults() {
  if [ "$NET_BLOCK_MS" -ge 3000 ]; then
    HEALTH_TIMEOUT="${HEALTH_TIMEOUT:-90}" FINALITY_TIMEOUT="${FINALITY_TIMEOUT:-120}"
  else
    HEALTH_TIMEOUT="${HEALTH_TIMEOUT:-45}" FINALITY_TIMEOUT="${FINALITY_TIMEOUT:-60}"
  fi
}

account() { # account <dotted.path> -> value from accounts.json
  python3 -I -c '
import json, sys
value = json.load(open(sys.argv[1]))
for key in sys.argv[2].split("."):
    value = value[int(key)] if key.isdigit() else value[key]
print(value)' "$ACCOUNTS" "$1"
}

validator_index() { # validator_index <node> -> index into accounts.json validators
  case "$1" in go1|rust1) echo 0 ;; rust2) echo 1 ;; rust3) echo 2 ;; *) die "$1 is not a validator node" ;; esac
}

# ---------------------------------------------------------------------------
# Processes: pidfiles, verified against /proc before any signal is sent.
# ---------------------------------------------------------------------------
resolve_bin() { # resolve_bin <bin> -> canonical path, or nothing
  local path
  path=$(command -v "$1" 2>/dev/null) || return 1
  readlink -f "$path" 2>/dev/null || printf '%s\n' "$path"
}

pid_alive() { # pid_alive <pid>: running and not a zombie
  kill -0 "$1" 2>/dev/null || return 1
  if [ -r "/proc/$1/stat" ]; then
    local state
    state=$(sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | cut -d' ' -f1)
    [ "$state" != Z ] || return 1
  fi
  return 0
}

pid_is_ours() { # pid_is_ours <node-dir>: print the pid if its pidfile names a live process running that node
  local dir="$1" pid want exe i found=0
  [ -f "$dir/pid" ] || return 1
  pid=$(cat "$dir/pid" 2>/dev/null) || return 1
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || return 1
  pid_alive "$pid" || return 1
  if [ -r "/proc/$pid/cmdline" ]; then
    local -a argv=()
    mapfile -d '' -t argv <"/proc/$pid/cmdline" || return 1
    exe=$(readlink "/proc/$pid/exe" 2>/dev/null || true)
    exe="${exe% (deleted)}"
    want=$(cat "$dir/pid.exe" 2>/dev/null || true)
    if [ -n "$want" ]; then
      [ "$exe" = "$want" ] || return 1
    else
      case "${exe##*/}" in reth-bsc|geth) ;; *) return 1 ;; esac
    fi
    for ((i = 0; i < ${#argv[@]}; i++)); do
      if [ "${argv[$i]}" = "--datadir" ] && [ "${argv[$((i + 1))]:-}" = "$dir" ]; then found=1; fi
      if [ "${argv[$i]}" = "--datadir=$dir" ]; then found=1; fi
    done
    [ "$found" = 1 ] || return 1
  else
    ps -p "$pid" -o args= 2>/dev/null | grep -qF -- "--datadir $dir" || return 1
  fi
  printf '%s\n' "$pid"
}

datadir_users() { # datadir_users <dir>: pids of any process whose argv names <dir> as --datadir
  [ -d /proc ] || return 0
  python3 -I -c '
import os, sys
target = sys.argv[1].rstrip("/")
for pid in filter(str.isdigit, os.listdir("/proc")):
    try:
        argv = [a.decode(errors="replace") for a in open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")]
    except OSError:
        continue
    for i, arg in enumerate(argv):
        nxt = argv[i + 1].rstrip("/") if i + 1 < len(argv) else ""
        if (arg == "--datadir" and nxt == target) or arg == "--datadir=" + target:
            print(pid)
            break' "$1"
}

stop_node() { # stop_node <node>: SIGTERM, bounded wait, SIGKILL only as a last resort
  local name="$1" dir pid i
  dir=$(node_dir "$name")
  if ! pid=$(pid_is_ours "$dir"); then
    if [ -f "$dir/pid" ]; then
      warn "$name: pidfile names pid $(cat "$dir/pid"), which is not running this node; not signalling it"
    fi
    rm -f "$dir/pid" "$dir/pid.exe"
    return 0
  fi
  log "stopping $name (pid $pid)"
  kill -TERM "$pid" 2>/dev/null || true
  for ((i = 0; i < STOP_TIMEOUT * 4; i++)); do
    if ! pid_alive "$pid"; then
      wait "$pid" 2>/dev/null || true
      rm -f "$dir/pid" "$dir/pid.exe"
      return 0
    fi
    sleep 0.25
  done
  warn "$name (pid $pid) did not exit within ${STOP_TIMEOUT}s; sending SIGKILL"
  if pid_is_ours "$dir" >/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
  for ((i = 0; i < 40; i++)); do
    if ! pid_alive "$pid"; then
      wait "$pid" 2>/dev/null || true
      rm -f "$dir/pid" "$dir/pid.exe"
      return 0
    fi
    sleep 0.25
  done
  die "$name (pid $pid) is still alive after SIGKILL"
}

port_in_use() { timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$1" 2>/dev/null; }

preflight_ports() { # every port of every node must be free; a 56/97 node there is refused loudly
  local n port kind
  for n in $NET_NODES; do
    node_ports "$n"
    for kind in P2P HTTP AUTH GRPC; do
      port="${!kind}"
      [ "$port" -ne 0 ] || continue
      if port_in_use "$port"; then
        fence_port "$port" "whatever listens where $n's $kind port should be"
        die "port $port ($n $kind) is already in use; stop its owner or set PORT_OFFSET (see README)"
      fi
    done
  done
}

check_ipc_dir() {
  local longest
  longest="$(ipc_path rust3)"
  [ "${#longest}" -le 100 ] ||
    die "IPC socket paths would be ${#longest} bytes (Unix limit: 107): $longest; set IPC_DIR to a shorter directory"
}

tail_logs() { # tail_logs <node>
  local dir f
  dir=$(node_dir "$1")
  for f in "$dir/node.log" "$dir/node.out"; do
    [ -s "$f" ] || continue
    echo "----- last lines of $f -----" >&2
    tail -n 15 "$f" >&2 || true
  done
}

wait_rpc() { # wait_rpc <node> <port> <timeout-secs>
  local name="$1" port="$2" timeout="$3" deadline
  deadline=$((SECONDS + timeout))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if [ -n "$(chain_id_of "$port")" ]; then return 0; fi
    if ! pid_is_ours "$(node_dir "$name")" >/dev/null; then
      tail_logs "$name"
      die "$name exited during startup (see $(node_dir "$name")/node.log)"
    fi
    sleep 1
  done
  tail_logs "$name"
  die "$name: RPC on port $port did not come up within ${timeout}s"
}

# Fails loudly when a node answers on the wrong chain.
#
# `geth init` creates <datadir>/geth *before* it writes the genesis block, so a
# failed init leaves a directory that makes every later run skip init. The node
# then finds a chaindata with no genesis, falls back to the BSC mainnet chain
# config, and serves chain id 56 -- healthy-looking at every level a status
# check can see, so the chain id is asserted explicitly.
assert_chain_id() { # assert_chain_id <node> <port>
  local got
  got=$(chain_id_of "$2")
  fence_chain_id "${got:-0}" "$1"
  [ "$got" = "$NET_CHAIN_ID" ] || die "$1 on port $2 is on chain id ${got:-?}, expected $NET_CHAIN_ID (genesis not applied?)"
}

enode_of() { # enode_of <node>: enode from admin_nodeInfo, host/port pinned to loopback
  local port enode
  node_ports "$1"
  port="$P2P"
  enode=$(rpc "$HTTP" admin_nodeInfo | python3 -I -c '
import json, sys
try:
    print(json.load(sys.stdin)["result"]["enode"])
except Exception:
    pass')
  [[ "$enode" =~ ^enode://([0-9a-f]{128})@ ]] ||
    die "$1: admin_nodeInfo returned no enode (is the admin API enabled on port $HTTP?)"
  printf 'enode://%s@127.0.0.1:%s\n' "${BASH_REMATCH[1]}" "$port"
}

add_peer() { # add_peer <node> <enode>: trusted, so it is re-dialled after a drop
  local out
  node_ports "$1"
  out=$(rpc "$HTTP" admin_addTrustedPeer "[\"$2\"]")
  out+=$(rpc "$HTTP" admin_addPeer "[\"$2\"]")
  [[ "$out" == *'"result":true'* ]] || die "$1: admin_addPeer $2 failed: ${out:-no response}"
}

check_vote_address() { # check_vote_address <node> <expected 0x..>: reth-bsc logs its BLS public key
  local dir got i
  dir=$(node_dir "$1")
  for ((i = 0; i < 20; i++)); do
    got=$(grep -a 'Initialized BLS signer successfully' "$dir/node.out" 2>/dev/null |
      grep -ao '0x[0-9a-f]\{96\}' | tail -1 || true)
    [ -z "$got" ] || break
    sleep 0.5
  done
  [ -n "$got" ] || die "$1: reth-bsc did not report its BLS vote address (see $dir/node.out)"
  [ "$got" = "$2" ] || die "$1: BLS vote address $got does not match the genesis ($2); votes would be dropped"
}

launch() { # launch <node> <exe> <env assignments...> -- <args...>: start in <node-dir>, record pid + exe
  local name="$1" exe="$2" dir pid
  shift 2
  local -a envs=()
  while [ "$1" != -- ]; do envs+=("$1"); shift; done
  shift
  dir=$(node_dir "$name")
  local -a launcher=()
  if command -v setsid >/dev/null 2>&1; then launcher=(setsid); fi
  # The node's cwd is its own datadir, so nothing it writes relative to the
  # cwd (reth-bsc's default `voteJournal/`, for one) lands in the caller's cwd.
  (cd "$dir" && exec env "${envs[@]}" "${launcher[@]}" "$exe" "$@") \
    >"$dir/node.out" 2>"$dir/node.log" </dev/null &
  pid=$!
  printf '%s\n' "$pid" >"$dir/pid"
  printf '%s\n' "$exe" >"$dir/pid.exe"
  STARTED+=("$name")
  sleep 0.5
  pid_is_ours "$dir" >/dev/null || { tail_logs "$name"; die "$name exited immediately (see $dir/node.log)"; }
}

init_go_node() { # init_go_node <node>
  local name="$1" dir key
  dir=$(node_dir "$name")
  mkdir -p "$dir"
  if [ ! -f "$dir/keystore_imported" ]; then
    key=$(account "validators.0.privateKey")
    printf '%s\n' "${key#0x}" >"$dir/validator.key"
    printf '%s\n' "$GETH_PASSWORD" >"$dir/passfile"
    "$GETH_BIN" account import --datadir "$dir" --password "$dir/passfile" "$dir/validator.key" >/dev/null 2>&1 ||
      die "geth account import failed for $name"
    touch "$dir/keystore_imported"
  fi
  if [ ! -f "$dir/geth/chaindata/CURRENT" ]; then
    # A failed `geth init` must be fatal and must not leave the partial `geth/`
    # directory behind: its presence is what makes the next run skip init and
    # come up on mainnet.
    local out
    if ! out=$("$GETH_BIN" init --datadir "$dir" "$NET_GENESIS" 2>&1); then
      rm -rf "$dir/geth"
      die "geth init failed for $name: $(printf '%s' "$out" | grep -iE '^(Fatal|Error)' | tail -1)"
    fi
  fi
  if [ ! -f "$dir/nodekey.hex" ]; then head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' >"$dir/nodekey.hex"; fi
}

start_go_node() { # start_go_node <node> <miner|follower>
  local name="$1" role="$2" dir exe addr
  dir=$(node_dir "$name")
  node_ports "$name"
  init_go_node "$name"
  exe=$(resolve_bin "$GETH_BIN")
  local -a args=(--datadir "$dir" --networkid "$NET_CHAIN_ID" --port "$P2P" --nodiscover --nat none
    --http --http.addr 127.0.0.1 --http.port "$HTTP"
    --http.api "eth,net,web3,txpool,parlia,miner,mev,admin,debug"
    --ipcpath "$(ipc_path "$name")" --nodekeyhex "$(cat "$dir/nodekey.hex")" --verbosity 3
    --override.breatheblockinterval "$BREATHE_BLOCK_INTERVAL")
  if [ "$role" = miner ]; then
    addr=$(account "validators.0.address")
    # go-bsc has no flag for MEV: the validator only accepts BEP-322 bids when the
    # config file enables it (builders are then added with miner_addBuilder). Its
    # BEP-675 gRPC service would listen on a fixed port (8552) that collides with
    # rust1's, so it is turned off; go nodes take BEP-322 bids over HTTP only.
    printf '[Eth.Miner.Mev]\nEnabled = true\nGRPCDisabled = true\n' >"$dir/mev.toml"
    args+=(--config "$dir/mev.toml"
      --mine --miner.etherbase "$addr" --unlock "$addr" --password "$dir/passfile"
      --allow-insecure-unlock --miner.recommit 1s)
  else
    # --syncmode full is required; snap fails on tiny private chains with
    # "missing trie node ... layer stale".
    args+=(--syncmode full)
  fi
  log "starting go-bsc $name ($role): p2p $P2P, http $HTTP, ipc $(ipc_path "$name")"
  launch "$name" "$exe" -- "${args[@]}"
  wait_rpc "$name" "$HTTP" 60
  assert_chain_id "$name" "$HTTP"
}

start_rust_node() { # start_rust_node <node> <miner|follower> [trusted-enode...]
  local name="$1" role="$2" dir exe
  shift 2
  dir=$(node_dir "$name")
  node_ports "$name"
  mkdir -p "$dir/logs" "$dir/voteJournal"
  exe=$(resolve_bin "$RETH_BIN")
  # -d (no discovery) is required: a custom chain otherwise falls back to public
  # bootnodes and gets banned by mainnet peers.
  local -a args=(node --chain "$NET_GENESIS" --datadir "$dir" --addr 127.0.0.1 --port "$P2P" -d
    --color never --log.file.directory "$dir/logs"
    --http --http.addr 127.0.0.1 --http.port "$HTTP"
    --http.api "eth,net,web3,admin,txpool,miner,mev,debug"
    --authrpc.addr 127.0.0.1 --authrpc.port "$AUTH" --ipcpath "$(ipc_path "$name")"
    --rpc.eth-proof-window "$ETH_PROOF_WINDOW")
  if [ $# -gt 0 ]; then
    local trusted
    trusted=$(IFS=,; printf '%s' "$*")
    args+=(--trusted-peers "$trusted")
  fi
  local vote=""
  if [ "$role" = miner ]; then
    local idx
    idx=$(validator_index "$name")
    vote=$(account "validators.$idx.voteAddress")
    # Mining must be enabled at startup; the miner_start RPC does not start the
    # sealing loop. BidBlock admission additionally needs Pasteur at the head.
    args+=(--mining.enabled --mining.private-key "$(account "validators.$idx.privateKey")"
      --bls.private-key "$(account "validators.$idx.blsPrivateKey")"
      --mining.gas-limit 40000000 --mining.bid-block-enabled --mining.mev-grpc-port "$GRPC")
  fi
  local -a extra=()
  if [ -n "${RETH_EXTRA_ARGS:-}" ]; then read -r -a extra <<<"$RETH_EXTRA_ARGS"; fi
  log "starting reth-bsc $name ($role): p2p $P2P, http $HTTP, authrpc $AUTH, ipc $(ipc_path "$name")$(
    [ "$role" = miner ] && printf ', mev-grpc %s' "$GRPC")"
  # BREATHE_BLOCK_INTERVAL must match every other node's: it is consensus.
  launch "$name" "$exe" \
    BSC_DEBUG_BUILDER=true \
    BREATHE_BLOCK_INTERVAL="$BREATHE_BLOCK_INTERVAL" \
    BSC_VOTE_JOURNAL_DIR="$dir/voteJournal" \
    BSC_VOTE_WARMUP_BLOCKS="$VOTE_WARMUP_BLOCKS" \
    -- "${args[@]}" "${extra[@]}"
  wait_rpc "$name" "$HTTP" 90
  assert_chain_id "$name" "$HTTP"
  if [ -n "$vote" ]; then check_vote_address "$name" "$vote"; fi
}

print_versions() {
  local mode="$1"
  if [[ " $NET_NODES " == *" rust"* ]]; then
    log "reth-bsc: $(resolve_bin "$RETH_BIN") -- $("$RETH_BIN" --version 2>/dev/null | head -2 | tr '\n' ' ')"
  fi
  if [[ " $NET_NODES " == *" go"* ]]; then
    log "go-bsc:   $(resolve_bin "$GETH_BIN") -- $("$GETH_BIN" version 2>/dev/null | grep -iE '^(version|git commit)' | tr '\n' ' ')"
  fi
  log "mode $mode, profile $NET_PROFILE ($NET_BLOCK_MS ms blocks), genesis $NET_GENESIS"
  log "breathe-block interval ${BREATHE_BLOCK_INTERVAL}s (all nodes must agree), port offset $PORT_OFFSET"
}

# ---------------------------------------------------------------------------
# Health
# ---------------------------------------------------------------------------
min_max_of() { # min_max_of <what: head|finalized> <nodes...> -> "min max", or nothing if a node is silent
  local what="$1" n v min="" max=""
  shift
  for n in "$@"; do
    node_ports "$n"
    if [ "$what" = head ]; then v=$(head_of "$HTTP"); else v=$(block_field "$HTTP" finalized number); fi
    [ -n "$v" ] || return 1
    if [ -z "$min" ] || [ "$v" -lt "$min" ]; then min="$v"; fi
    if [ -z "$max" ] || [ "$v" -gt "$max" ]; then max="$v"; fi
  done
  echo "$min $max"
}

expected_peers() { # expected_peers <node>
  case "$NET_MODE:$1" in
    multi:*) echo 2 ;;
    mixed:go1) echo 2 ;;
    *) echo 1 ;;
  esac
}

do_health() { # do_health: 0 if healthy, 1 otherwise; every failure is printed
  load_state
  health_defaults
  NET_REQUIRE_FINALITY="${REQUIRE_FINALITY:-$NET_REQUIRE_FINALITY}"
  local n dir id mm h0 hmin fails=0 deadline idx
  local -a nodes
  read -r -a nodes <<<"$NET_NODES"
  log "health: ${nodes[*]} ($NET_MODE/$NET_PROFILE, chain id $NET_CHAIN_ID)"

  for n in "${nodes[@]}"; do
    dir=$(node_dir "$n")
    node_ports "$n"
    if ! pid_is_ours "$dir" >/dev/null; then
      echo "[privnet] FAIL: $n is not running (pidfile $dir/pid)" >&2; fails=$((fails + 1)); continue
    fi
    id=$(chain_id_of "$HTTP")
    if [ -z "$id" ]; then
      echo "[privnet] FAIL: $n RPC on port $HTTP is unreachable" >&2; fails=$((fails + 1)); continue
    fi
    fence_chain_id "$id" "$n"
    if [ "$id" != "$NET_CHAIN_ID" ]; then
      echo "[privnet] FAIL: $n is on chain id $id, expected $NET_CHAIN_ID" >&2; fails=$((fails + 1))
    fi
  done
  [ "$fails" -eq 0 ] || { do_status >&2 || true; return 1; }

  # Head: every node must advance HEALTH_BLOCKS past the highest head seen now.
  mm=$(min_max_of head "${nodes[@]}") || { echo "[privnet] FAIL: a node stopped answering" >&2; return 1; }
  h0=${mm#* }
  deadline=$((SECONDS + HEALTH_TIMEOUT))
  while :; do
    if mm=$(min_max_of head "${nodes[@]}"); then
      hmin=${mm% *}
      [ "$hmin" -lt $((h0 + HEALTH_BLOCKS)) ] || break
    else
      hmin="?"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "[privnet] FAIL: head did not advance $HEALTH_BLOCKS blocks past $h0 within ${HEALTH_TIMEOUT}s (lowest node at $hmin)" >&2
      fails=$((fails + 1))
      break
    fi
    sleep 1
  done
  [ "$fails" -ne 0 ] || log "  head: all nodes advanced from $h0 to >= $((h0 + HEALTH_BLOCKS))"

  # Peers.
  local want got
  for n in "${nodes[@]}"; do
    node_ports "$n"
    want=$(expected_peers "$n")
    deadline=$((SECONDS + 15))
    while :; do
      got=$(peers_of "$HTTP")
      if [ -n "$got" ] && [ "$got" -ge "$want" ]; then break; fi
      if [ "$SECONDS" -ge "$deadline" ]; then
        echo "[privnet] FAIL: $n has ${got:-?} peer(s), expected >= $want" >&2
        fails=$((fails + 1))
        break
      fi
      sleep 1
    done
  done
  [ "$fails" -ne 0 ] || log "  peers: every node has its expected peers"

  # Agreement: one canonical hash per height.
  if mm=$(min_max_of head "${nodes[@]}"); then
    local height ref hash
    height=$(( ${mm% *} > 1 ? ${mm% *} - 1 : 1 ))
    node_ports "${nodes[0]}"
    ref=$(block_field "$HTTP" "$(printf '0x%x' "$height")" hash)
    for n in "${nodes[@]:1}"; do
      node_ports "$n"
      hash=$(block_field "$HTTP" "$(printf '0x%x' "$height")" hash)
      if [ -z "$ref" ] || [ "$hash" != "$ref" ]; then
        echo "[privnet] FAIL: $n has ${hash:-no block} at height $height, ${nodes[0]} has ${ref:-none}" >&2
        fails=$((fails + 1))
      fi
    done
    [ "$fails" -ne 0 ] || log "  agreement: all nodes have $ref at height $height"
  fi

  # Multi-validator: every validator must be sealing.
  if [ "$NET_MODE" = multi ] && [ "$fails" -eq 0 ]; then
    local miners=""
    deadline=$((SECONDS + HEALTH_TIMEOUT))
    while :; do
      node_ports rust1
      local top b
      top=$(head_of "$HTTP")
      miners=""
      for ((b = top; b > 0 && b > top - 48; b--)); do
        miners+="$(block_field "$HTTP" "$(printf '0x%x' "$b")" miner | tr 'A-F' 'a-f') "
      done
      local missing=0 v
      for idx in 0 1 2; do
        v=$(account "validators.$idx.address" | tr 'A-F' 'a-f')
        [[ " $miners " == *" $v "* ]] || missing=$((missing + 1))
      done
      [ "$missing" -ne 0 ] || break
      if [ "$SECONDS" -ge "$deadline" ]; then
        echo "[privnet] FAIL: $missing validator(s) sealed nothing in the last 48 blocks" >&2
        fails=$((fails + 1))
        break
      fi
      sleep 2
    done
    [ "$fails" -ne 0 ] || log "  validators: all three sealed blocks in the last 48"
  fi

  # Finality: `finalized` must be > 0 and move forward on every node.
  if [ "$fails" -eq 0 ]; then
    local f0 fmin
    mm=$(min_max_of finalized "${nodes[@]}") || mm="0 0"
    f0=${mm% *}
    deadline=$((SECONDS + FINALITY_TIMEOUT))
    while :; do
      if mm=$(min_max_of finalized "${nodes[@]}"); then fmin=${mm% *}; else fmin=0; fi
      if [ "$fmin" -gt 0 ] && [ "$fmin" -gt "$f0" ]; then
        log "  finality: finalized advanced from $f0 to $fmin on every node"
        break
      fi
      if [ "$SECONDS" -ge "$deadline" ]; then
        if [ "$NET_REQUIRE_FINALITY" = 1 ]; then
          echo "[privnet] FAIL: finalized did not advance past $f0 within ${FINALITY_TIMEOUT}s (lowest node at $fmin)" >&2
          fails=$((fails + 1))
        else
          warn "finalized stayed at $fmin (not required for $NET_MODE: the go-bsc miner has no BLS vote key wired)"
        fi
        break
      fi
      sleep 1
    done
  fi

  # BEP-675: a Pasteur chain must advertise BidBlock admission on every miner.
  # reth-bsc gates it on Pasteur at the head, and Pasteur on London (block 8).
  if [ "$fails" -eq 0 ] && { [ "$NET_PROFILE" = pasteur ] || [ "$NET_PROFILE" = multi ]; }; then
    for n in $NET_MINERS; do
      is_go "$n" && continue
      node_ports "$n"
      deadline=$((SECONDS + HEALTH_TIMEOUT))
      until [[ "$(rpc "$HTTP" mev_params)" == *'"BidBlockEnabled":true'* ]]; do
        if [ "$SECONDS" -ge "$deadline" ]; then
          echo "[privnet] FAIL: $n mev_params never reported BidBlockEnabled=true" >&2
          fails=$((fails + 1))
          break
        fi
        sleep 1
      done
    done
    [ "$fails" -ne 0 ] || log "  mev: BidBlockEnabled=true on $NET_MINERS (BEP-675)"
  fi

  do_status
  if [ "$fails" -ne 0 ]; then
    echo "[privnet] UNHEALTHY: $fails check(s) failed" >&2
    return 1
  fi
  log "HEALTHY"
}

do_status() {
  load_state optional || { log "no network recorded in $PRIVNET_DIR"; return 0; }
  local n dir pid head fin peers
  printf '%-6s %-9s %-8s %-9s %-9s %-5s %-24s %s\n' NODE ROLE PID HEAD FINALIZED PEERS RPC IPC
  for n in $NET_NODES; do
    dir=$(node_dir "$n")
    node_ports "$n"
    local role=follower
    [[ " $NET_MINERS " == *" $n "* ]] && role=miner
    if pid=$(pid_is_ours "$dir"); then
      head=$(head_of "$HTTP"); fin=$(block_field "$HTTP" finalized number); peers=$(peers_of "$HTTP")
    else
      pid=DEAD head=- fin=- peers=-
    fi
    printf '%-6s %-9s %-8s %-9s %-9s %-5s %-24s %s\n' "$n" "$role" "$pid" "${head:-?}" "${fin:-?}" \
      "${peers:-?}" "http://127.0.0.1:$HTTP" "$(ipc_path "$n")"
  done
}

do_env() {
  load_state
  local miner follower n first=""
  for n in $NET_MINERS; do [ -n "$first" ] || first="$n"; done
  miner="$first"
  follower=""
  for n in $NET_NODES; do [[ " $NET_MINERS " == *" $n "* ]] || { follower="$n"; break; }; done
  node_ports "$miner"
  fence_port "$HTTP" "$miner"
  banner
  echo "# DEV-ONLY privnet ($NET_MODE/$NET_PROFILE) in $PRIVNET_DIR"
  printf 'export PRIVNET_DIR=%q\n' "$PRIVNET_DIR"
  printf 'export PRIVNET_MODE=%q\nexport PRIVNET_PROFILE=%q\n' "$NET_MODE" "$NET_PROFILE"
  printf 'export PRIVNET_CHAIN_ID=%q\nexport PRIVNET_GENESIS=%q\n' "$NET_CHAIN_ID" "$NET_GENESIS"
  printf 'export PRIVNET_BLOCK_INTERVAL_MS=%q\n' "$NET_BLOCK_MS"
  printf 'export PRIVNET_RPC_URL=%q\n' "http://127.0.0.1:$HTTP"
  printf 'export PRIVNET_IPC_PATH=%q\n' "$(ipc_path "$miner")"
  is_go "$miner" || printf 'export PRIVNET_MEV_GRPC=%q\n' "127.0.0.1:$GRPC"
  local rpcs="" ipcs=""
  for n in $NET_MINERS; do
    node_ports "$n"
    rpcs+="${rpcs:+,}http://127.0.0.1:$HTTP"
    ipcs+="${ipcs:+,}$(ipc_path "$n")"
  done
  printf 'export PRIVNET_VALIDATOR_RPC_URLS=%q\nexport PRIVNET_VALIDATOR_IPC_PATHS=%q\n' "$rpcs" "$ipcs"
  if [ -n "$follower" ]; then
    node_ports "$follower"
    printf 'export PRIVNET_FOLLOWER_RPC_URL=%q\n' "http://127.0.0.1:$HTTP"
  fi
  printf 'export PRIVNET_BUILDER_ADDRESS=%q\nexport PRIVNET_BUILDER_KEY=%q\n' \
    "$(account builder.address)" "$(account builder.privateKey)"
  local i
  for i in 0 1 2; do
    printf 'export PRIVNET_USER%d_ADDRESS=%q\nexport PRIVNET_USER%d_KEY=%q\n' \
      $((i + 1)) "$(account "users.$i.address")" $((i + 1)) "$(account "users.$i.privateKey")"
  done
}

do_accounts() {
  banner
  python3 -I -c '
import json, sys
a = json.load(open(sys.argv[1]))
print("%-11s %-42s %s" % ("ROLE", "ADDRESS", "PRIVATE KEY (DEV-ONLY)"))
for v in a["validators"]:
    print("%-11s %-42s %s" % (v["name"], v["address"], v["privateKey"]))
    print("%-11s %-42s %s" % ("  bls", v["voteAddress"][:18] + "...", v["blsPrivateKey"]))
for v in [a["builder"], *a["users"]]:
    print("%-11s %-42s %s" % (v["name"], v["address"], v["privateKey"]))
print("balances: validators %s wei, builder/users %s wei" % (a["validatorBalanceWei"], a["testAccountBalanceWei"]))
' "$ACCOUNTS"
}

do_stop() {
  local -a nodes=()
  if load_state optional; then
    read -r -a nodes <<<"$NET_NODES"
  else
    local n
    for n in rust3 rust2 rust1 go2 go1; do [ -d "$(node_dir "$n")" ] && nodes+=("$n"); done
  fi
  local i
  for ((i = ${#nodes[@]} - 1; i >= 0; i--)); do stop_node "${nodes[$i]}"; done
  return 0
}

do_clean() {
  do_stop
  local n dir users
  for n in go1 go2 rust1 rust2 rust3; do
    dir=$(node_dir "$n")
    [ -e "$dir" ] || continue
    users=$(datadir_users "$dir")
    [ -z "$users" ] || die "refusing to wipe $dir: still in use by pid(s) $(echo "$users" | tr '\n' ' ')"
    rm -rf "$dir"
    [ ! -e "$dir" ] || die "failed to remove $dir"
  done
  local ipc_dir="$IPC_DIR"
  if load_state optional; then ipc_dir="$NET_IPC_DIR"; fi
  rm -f "$ipc_dir"/go1.ipc "$ipc_dir"/go2.ipc "$ipc_dir"/rust1.ipc "$ipc_dir"/rust2.ipc "$ipc_dir"/rust3.ipc
  rmdir "$ipc_dir" 2>/dev/null || true
  rm -f "$STATE" "$PRIVNET_DIR/startup.log"
  # Older versions of this script let reth-bsc write its vote journal relative
  # to the caller's cwd, which left a stale journal (refusing votes on every
  # height it lists) next to the script.
  if [ "$PRIVNET_DIR" = "$SCRIPT_DIR/data" ] && [ -d "$SCRIPT_DIR/voteJournal" ]; then
    log "removing the legacy vote journal $SCRIPT_DIR/voteJournal"
    rm -rf "$SCRIPT_DIR/voteJournal"
  fi
  log "wiped chain data, vote journals and IPC sockets under $PRIVNET_DIR"
}

# ---------------------------------------------------------------------------
# start
# ---------------------------------------------------------------------------
STARTED=()
START_OK=0
on_start_exit() {
  local rc=$?
  [ "$START_OK" = 1 ] && return 0
  [ "${#STARTED[@]}" -gt 0 ] || return 0
  if [ "$KEEP_ON_FAILURE" = 1 ]; then
    warn "start failed (exit $rc); KEEP_ON_FAILURE=1 leaves ${STARTED[*]} running for inspection"
    return 0
  fi
  warn "start failed (exit $rc); stopping the nodes this run started: ${STARTED[*]}"
  local i
  for ((i = ${#STARTED[@]} - 1; i >= 0; i--)); do (stop_node "${STARTED[$i]}") || true; done
}

need_bin() { # need_bin <bin> <VAR> <why>
  command -v "$1" >/dev/null 2>&1 && return 0
  if [ "$2" = GETH_BIN ]; then
    skip "go-bsc binary '$1' not found (set GETH_BIN); $3 needs it. Rust-only modes: rust, fast, pasteur, multi"
  fi
  die "binary not found: $1 (set $2); $3 needs it"
}

start_mode() { # start_mode <mode>
  local mode="$1" n
  select_mode "$mode"
  banner
  [[ " $NET_NODES " != *" go"* ]] || need_bin "$GETH_BIN" GETH_BIN "mode $mode"
  [[ " $NET_NODES " != *" rust"* ]] || need_bin "$RETH_BIN" RETH_BIN "mode $mode"
  need_bin python3 python3 "privnet.sh"
  need_bin curl curl "privnet.sh"
  [ -f "$NET_GENESIS" ] || die "genesis file missing: $NET_GENESIS (run: $0 system-contracts)"
  [ -f "$ACCOUNTS" ] || die "accounts file missing: $ACCOUNTS (run: $0 system-contracts)"
  NET_CHAIN_ID=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["config"]["chainId"])' "$NET_GENESIS")
  fence_chain_id "$NET_CHAIN_ID" "genesis $NET_GENESIS"
  NET_GENESIS_SHA=$(python3 -I -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$NET_GENESIS")
  check_ipc_dir

  # Refuse to reuse datadirs initialised from another genesis, or a running net.
  local old_mode="" old_sha=""
  if [ -f "$STATE" ]; then
    old_mode=$(sed -n 's/^NET_MODE=//p' "$STATE")
    old_sha=$(sed -n 's/^NET_GENESIS_SHA=//p' "$STATE")
  fi
  for n in go1 go2 rust1 rust2 rust3; do
    if pid_is_ours "$(node_dir "$n")" >/dev/null; then
      die "$n is already running from $PRIVNET_DIR ($old_mode); run '$0 stop' first"
    fi
  done
  for n in $NET_NODES; do
    if [ -d "$(node_dir "$n")" ] && [ -n "$old_sha" ] && [ "$old_sha" != "$NET_GENESIS_SHA" ]; then
      die "$(node_dir "$n") holds a chain from another genesis ($old_mode); run '$0 clean' first"
    fi
  done
  preflight_ports

  trap on_start_exit EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  mkdir -p "$PRIVNET_DIR" "$IPC_DIR"
  for n in $NET_NODES; do mkdir -p "$(node_dir "$n")"; done
  write_state
  print_versions "$mode"

  case "$mode" in
    go)
      start_go_node go1 miner
      start_go_node go2 follower
      add_peer go1 "$(enode_of go2)"
      ;;
    mixed)
      start_go_node go1 miner
      start_go_node go2 follower
      add_peer go1 "$(enode_of go2)"
      start_rust_node rust1 follower "$(enode_of go1)"
      start_rust_node rust2 follower "$(enode_of rust1)"
      add_peer go1 "$(enode_of rust1)"
      ;;
    rust|fast|pasteur)
      start_rust_node rust1 miner
      start_rust_node rust2 follower "$(enode_of rust1)"
      ;;
    multi)
      start_rust_node rust1 miner
      start_rust_node rust2 miner "$(enode_of rust1)"
      start_rust_node rust3 miner "$(enode_of rust1)" "$(enode_of rust2)"
      add_peer rust1 "$(enode_of rust2)"
      add_peer rust1 "$(enode_of rust3)"
      add_peer rust2 "$(enode_of rust3)"
      ;;
  esac

  if [ "$START_HEALTH" = 1 ]; then
    if ! do_health; then
      for n in $NET_NODES; do tail_logs "$n"; done
      die "the network started but is not healthy"
    fi
  else
    log "START_HEALTH=0: skipping the health check"
    do_status
  fi
  START_OK=1
  log "started ($mode/$NET_PROFILE). Builder e2e settings: $0 env"
}

# ---------------------------------------------------------------------------
# Miner handover (mixed only)
# ---------------------------------------------------------------------------
wait_head_advance() { # wait_head_advance <node> <blocks> <timeout>
  local h0 h deadline
  node_ports "$1"
  h0=$(head_of "$HTTP")
  [ -n "$h0" ] || die "$1 is not answering on port $HTTP"
  deadline=$((SECONDS + $3))
  while [ "$SECONDS" -lt "$deadline" ]; do
    h=$(head_of "$HTTP")
    if [ -n "$h" ] && [ "$h" -ge $((h0 + $2)) ]; then
      log "$1 advanced from $h0 to $h"
      return 0
    fi
    sleep 1
  done
  die "$1 did not advance $2 blocks past $h0 within $3s"
}

prepare_handover() {
  load_state
  health_defaults
  [ "$NET_MODE" = mixed ] || die "handover needs a mixed network; $PRIVNET_DIR runs $NET_MODE"
  need_bin "$GETH_BIN" GETH_BIN "handover"
  need_bin "$RETH_BIN" RETH_BIN "handover"
  node_ports go1
  fence_port "$HTTP" go1
}

do_handover_to_rust() {
  prepare_handover
  node_ports go1
  log "stopping the Go miner (miner_stop on :$HTTP)"
  [[ "$(rpc "$HTTP" miner_stop)" == *'"result"'* ]] || die "miner_stop on go1 failed"
  # Waits for the process to exit before reopening the same datadir.
  stop_node rust1
  start_rust_node rust1 miner "$(enode_of go1)"
  NET_MINERS="rust1"
  write_state
  wait_head_advance go1 3 "$HEALTH_TIMEOUT"
  log "handover complete: rust1 is mining. Check with: $0 health"
}

do_handover_to_go() {
  prepare_handover
  stop_node rust1
  start_rust_node rust1 follower "$(enode_of go1)"
  node_ports go1
  log "starting the Go miner (miner_start on :$HTTP)"
  [[ "$(rpc "$HTTP" miner_start)" == *'"result"'* ]] || die "miner_start on go1 failed"
  NET_MINERS="go1"
  write_state
  wait_head_advance rust1 3 "$HEALTH_TIMEOUT"
  log "handover complete: go1 is mining. Check with: $0 health"
}

# Regenerate the genesis files from bnb-chain/bsc-genesis-contract (pinned).
#
# A modern BSC chain calls its system contracts before assembling every payload,
# so without them the block builder gives up (`ETH call returned no data`
# against StakeHub at 0x2002) and no block is ever sealed. Those deployments are
# upstream's; build-genesis.py refreshes them and re-applies the private
# network's chain id, validators, test accounts and fork schedules.
do_system_contracts() {
  need_bin python3 python3 "system-contracts"
  need_bin git git "system-contracts"
  local n
  for n in go1 go2 rust1 rust2 rust3; do
    if pid_is_ours "$(node_dir "$n")" >/dev/null; then die "stop the network first: $0 stop"; fi
  done
  python3 -I "$SCRIPT_DIR/build-genesis.py"
  log "done; run '$0 clean' before restarting so every client re-inits"
}

case "$ACTION" in
  start) start_mode "$MODE" ;;
  stop) do_stop ;;
  status) do_status ;;
  health) do_health ;;
  env) do_env ;;
  accounts) do_accounts ;;
  clean) do_clean ;;
  handover-to-rust) do_handover_to_rust ;;
  handover-to-go) do_handover_to_go ;;
  system-contracts) do_system_contracts ;;
  genesis-check) python3 -I "$SCRIPT_DIR/build-genesis.py" --check ;;
esac
