#!/usr/bin/env bash
# Private BSC networks with a shared genesis: Go-only, Rust-only, or mixed.
#
#   ./privnet.sh go|rust|mixed   start the network (default action: start)
#   ./privnet.sh stop            stop all nodes started by this script
#   ./privnet.sh status          print block height / peer count per node
#   ./privnet.sh clean           stop everything and wipe chain data
#   ./privnet.sh handover-to-rust  stop the Go miner, restart rust1 as miner
#   ./privnet.sh handover-to-go    restart rust1 as follower, start Go miner
#
# Environment overrides:
#   GETH_BIN      geth (Go bsc) binary            [default: geth from PATH]
#   RETH_BIN      reth-bsc (Rust) binary          [default: reth-bsc from PATH]
#   PRIVNET_DIR   runtime data directory          [default: <script-dir>/data]
#
# Requirements: a geth binary built from bnb-chain/bsc, and (for rust/mixed
# modes) a reth-bsc binary built from a source tree that contains the
# private-network fixes described in docs/privnet.md (Status-head fix,
# system-contract decode fix, custom-chain defaults).
#
# Until upstream PR bnb-chain/reth-bsc#546 merges, build reth-bsc from our
# branch instead:
#   git clone https://github.com/akfork/reth-bsc && cd reth-bsc &&
#   git checkout fix/fix-privnet && cargo build --release
# After the merge, an upstream develop build works as well. Ports are fixed;
# see README.md.
set -euo pipefail

MODE="${1:-mixed}"
ACTION="start"
case "$MODE" in
  go|rust|mixed) ACTION="${2:-start}" ;;
  stop|status|clean|handover-to-rust|handover-to-go) ACTION="$MODE"; MODE="all" ;;
  *) echo "usage: $0 {go|rust|mixed} [start] | $0 {stop|status|clean|handover-to-rust|handover-to-go}" >&2; exit 2 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_DIR="${PRIVNET_DIR:-$SCRIPT_DIR/data}"
GETH_BIN="${GETH_BIN:-geth}"
RETH_BIN="${RETH_BIN:-reth-bsc}"
GENESIS="$SCRIPT_DIR/genesis.json"

# Fixed development validator key. NEVER use on mainnet/testnet.
VALIDATOR_KEY="1111111111111111111111111111111111111111111111111111111111111111"
VALIDATOR_ADDR="0x19e7e376e7c213b7e7e7e46cc70a5dd086daff2a"
PASSWORD="testpassword123"

# Ports: go1 30311/8545, go2 30312/8547, rust1 30313/8549, rust2 30314/8553.
GO1_P2P=30311 GO1_HTTP=8545
GO2_P2P=30312 GO2_HTTP=8547
RUST1_P2P=30313 RUST1_HTTP=8549 RUST1_AUTH=8555
RUST2_P2P=30314 RUST2_HTTP=8553 RUST2_AUTH=8556

log() { echo "[privnet] $*"; }
die() { echo "[privnet] ERROR: $*" >&2; exit 1; }
need_bin() { command -v "$1" >/dev/null 2>&1 || die "binary not found: $1 (set ${2} to override)"; }

rpc() { # rpc <port> <method> [params-json]
  local port="$1" method="$2" params="${3:-[]}"
  curl -s -m 10 -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}" \
    "http://127.0.0.1:$port" || true
}

wait_rpc() { # wait_rpc <port> <timeout-secs>
  local port="$1" timeout="${2:-60}" i=0
  while [ "$i" -lt "$timeout" ]; do
    if rpc "$port" eth_chainId | grep -q '"result"'; then return 0; fi
    sleep 2; i=$((i + 2))
  done
  die "RPC on port $port did not come up within ${timeout}s"
}

enode_from_log() { # enode_from_log <logfile>
  grep -o 'enode://[a-f0-9]*@[0-9.]*:[0-9]*' "$1" | head -1
}

rand_hex32() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

init_go_node() { # init_go_node <name>
  local name="$1"
  local dir="$DATA_DIR/$name"
  mkdir -p "$dir"
  if [ ! -f "$dir/keystore_imported" ]; then
    echo "$VALIDATOR_KEY" > "$dir/validator.key"
    echo "$PASSWORD" > "$dir/passfile"
    "$GETH_BIN" account import --datadir "$dir" --password "$dir/passfile" "$dir/validator.key" >/dev/null 2>&1
    touch "$dir/keystore_imported"
  fi
  if [ ! -d "$dir/geth" ]; then
    "$GETH_BIN" init --datadir "$dir" "$GENESIS" >/dev/null 2>&1
  fi
  if [ ! -f "$dir/nodekey.hex" ]; then rand_hex32 > "$dir/nodekey.hex"; fi
}

start_go_miner() { # start_go_miner <name> <p2p-port> <http-port>
  local name="$1" p2p="$2" http="$3"
  local dir="$DATA_DIR/$name"
  init_go_node "$name"
  log "starting Go miner $name (p2p $p2p, http $http)"
  "$GETH_BIN" --datadir "$dir" --networkid 1337 --port "$p2p" \
    --http --http.addr 127.0.0.1 --http.port "$http" \
    --http.api eth,net,web3,txpool,parlia,miner,admin \
    --nodekeyhex "$(cat "$dir/nodekey.hex")" --nodiscover \
    --mine --miner.etherbase "$VALIDATOR_ADDR" \
    --unlock "$VALIDATOR_ADDR" --password "$dir/passfile" \
    --allow-insecure-unlock --verbosity 3 --miner.recommit 1s \
    >"$dir/node.log" 2>&1 &
  echo $! > "$dir/pid"
  wait_rpc "$http"
}

start_go_follower() { # start_go_follower <name> <p2p-port> <http-port>
  local name="$1" p2p="$2" http="$3"
  local dir="$DATA_DIR/$name"
  init_go_node "$name"
  log "starting Go follower $name (p2p $p2p, http $http)"
  # NOTE: --syncmode full is required; the default snap mode fails on tiny
  # private chains with "missing trie node ... layer stale".
  "$GETH_BIN" --datadir "$dir" --networkid 1337 --port "$p2p" \
    --http --http.addr 127.0.0.1 --http.port "$http" \
    --http.api eth,net,web3,txpool,parlia,admin \
    --nodekeyhex "$(cat "$dir/nodekey.hex")" --nodiscover \
    --verbosity 3 --syncmode full \
    >"$dir/node.log" 2>&1 &
  echo $! > "$dir/pid"
  wait_rpc "$http"
}

go_add_peer() { # go_add_peer <datadir> <enode>
  "$GETH_BIN" attach --datadir "$1" --exec "admin.addPeer(\"$2\")" 2>/dev/null | tail -1
}

start_rust_node() { # start_rust_node <name> <p2p> <http> <authrpc> <trusted-enode> <miner:yes|no>
  local name="$1" p2p="$2" http="$3" auth="$4" trusted="$5" miner="$6"
  local dir
  dir="$DATA_DIR/$name"
  mkdir -p "$dir"
  local args=(node --chain "$GENESIS" --datadir "$dir" --port "$p2p" -d)
  [ -n "$trusted" ] && args+=(--trusted-peers "$trusted")
  args+=(--http --http.addr 127.0.0.1 --http.port "$http"
    --http.api eth,net,web3,admin,txpool,miner --authrpc.port "$auth")
  if [ "$miner" = "yes" ]; then
    # NOTE: mining must be enabled at startup; the miner_start RPC is a stub
    # and does not start the sealing loop.
    args+=(--mining.enabled --mining.private-key "0x$VALIDATOR_KEY" --mining.gas-limit 40000000)
  fi
  log "starting Rust node $name (p2p $p2p, http $http, miner=$miner)"
  "$RETH_BIN" "${args[@]}" >"$dir/node.out" 2>"$dir/node.log" &
  echo $! > "$dir/pid"
  wait_rpc "$http" 90
}

do_status() {
  printf '%-8s %-8s %-8s %s\n' NODE BLOCK PEERS ENODE
  for n in go1 go2 rust1 rust2; do
    [ -f "$DATA_DIR/$n/pid" ] || continue
    if ! kill -0 "$(cat "$DATA_DIR/$n/pid")" 2>/dev/null; then
      printf '%-8s %-8s %-8s %s\n' "$n" DEAD DEAD "-"; continue
    fi
    case "$n" in
      go1) http=$GO1_HTTP ;; go2) http=$GO2_HTTP ;;
      rust1) http=$RUST1_HTTP ;; rust2) http=$RUST2_HTTP ;;
    esac
    block=$(rpc "$http" eth_blockNumber | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','?'))" 2>/dev/null || echo "?")
    peers=$(rpc "$http" net_peerCount | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','?'))" 2>/dev/null || echo "?")
    printf '%-8s %-8s %-8s %s\n' "$n" "$block" "$peers" "-"
  done
}

do_stop() {
  for n in go1 go2 rust1 rust2; do
    if [ -f "$DATA_DIR/$n/pid" ]; then
      pid=$(cat "$DATA_DIR/$n/pid")
      if kill -0 "$pid" 2>/dev/null; then log "stopping $n (pid $pid)"; kill "$pid" 2>/dev/null || true; fi
      rm -f "$DATA_DIR/$n/pid"
    fi
  done
}

do_clean() {
  do_stop
  log "wiping chain data under $DATA_DIR"
  rm -rf "$DATA_DIR"/go1 "$DATA_DIR"/go2 "$DATA_DIR"/rust1 "$DATA_DIR"/rust2
}

start_mode() { # start_mode <go|rust|mixed>
  local mode="$1"
  mkdir -p "$DATA_DIR"
  case "$mode" in
    go) need_bin "$GETH_BIN" GETH_BIN ;;
    rust|mixed)
      need_bin "$GETH_BIN" GETH_BIN
      need_bin "$RETH_BIN" RETH_BIN
      ;;
  esac
  if [ "$mode" = "go" ] || [ "$mode" = "mixed" ]; then
    start_go_miner go1 "$GO1_P2P" "$GO1_HTTP"
    start_go_follower go2 "$GO2_P2P" "$GO2_HTTP"
    go_add_peer "$DATA_DIR/go1" "$(enode_from_log "$DATA_DIR/go2/node.log")"
  fi
  if [ "$mode" = "rust" ] || [ "$mode" = "mixed" ]; then
    miner="no"; trusted=""
    if [ "$mode" = "rust" ]; then miner="yes"; fi
    if [ "$mode" = "mixed" ]; then trusted="$(enode_from_log "$DATA_DIR/go1/node.log")"; fi
    # NOTE: -d (no discovery) is required; otherwise a custom chain falls back
    # to public bootnodes and gets banned by mainnet peers.
    start_rust_node rust1 "$RUST1_P2P" "$RUST1_HTTP" "$RUST1_AUTH" "$trusted" "$miner"
    start_rust_node rust2 "$RUST2_P2P" "$RUST2_HTTP" "$RUST2_AUTH" \
      "$(enode_from_log "$DATA_DIR/rust1/node.out")" "no"
    if [ "$mode" = "mixed" ]; then
      # Direct Go->Rust peering is optional (rust1 already dials go1); also
      # peer go1->rust1 for symmetry.
      go_add_peer "$DATA_DIR/go1" "$(enode_from_log "$DATA_DIR/rust1/node.out")"
    fi
  fi
  log "started ($mode). Waiting 20s for initial sync, then status:"
  sleep 20
  do_status
}

do_handover_to_rust() {
  need_bin "$GETH_BIN" GETH_BIN
  need_bin "$RETH_BIN" RETH_BIN
  log "stopping Go miner (miner_stop on :$GO1_HTTP)"
  rpc "$GO1_HTTP" miner_stop >/dev/null
  if [ -f "$DATA_DIR/rust1/pid" ]; then
    pid=$(cat "$DATA_DIR/rust1/pid")
    kill "$pid" 2>/dev/null || true
    sleep 4
    rm -f "$DATA_DIR/rust1/pid"
  fi
  # Restart rust1 on its existing DB, now as the block producer.
  start_rust_node rust1 "$RUST1_P2P" "$RUST1_HTTP" "$RUST1_AUTH" \
    "$(enode_from_log "$DATA_DIR/go1/node.log")" "yes"
  log "handover complete: rust1 is mining. Watch with: $0 status"
}

do_handover_to_go() {
  need_bin "$GETH_BIN" GETH_BIN
  need_bin "$RETH_BIN" RETH_BIN
  # Rust has no runtime miner-stop; restart rust1 as follower on its DB.
  if [ -f "$DATA_DIR/rust1/pid" ]; then
    pid=$(cat "$DATA_DIR/rust1/pid")
    kill "$pid" 2>/dev/null || true
    sleep 4
    rm -f "$DATA_DIR/rust1/pid"
  fi
  start_rust_node rust1 "$RUST1_P2P" "$RUST1_HTTP" "$RUST1_AUTH" \
    "$(enode_from_log "$DATA_DIR/go1/node.log")" "no"
  log "starting Go miner (miner_start on :$GO1_HTTP)"
  rpc "$GO1_HTTP" miner_start >/dev/null
  log "handover complete: go1 is mining. Watch with: $0 status"
}

case "$ACTION" in
  start) start_mode "$MODE" ;;
  stop) do_stop ;;
  status) do_status ;;
  clean) do_clean ;;
  handover-to-rust) do_handover_to_rust ;;
  handover-to-go) do_handover_to_go ;;
esac
