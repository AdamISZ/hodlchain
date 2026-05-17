#!/usr/bin/env bash
#
# End-to-end regtest demo of the hodlcoin POC.
#
# Spins up a fresh bitcoind in a temp datadir, creates a sequencer-funding
# wallet and a user wallet inside bitcoind, starts the hodl-sequencer and
# hodl-node, then runs two hodl-wallets (alice + bob) through:
#
#   keygen → mint-utxo (Alice) → mine ×1 → mint-message → mine ×2
#   → check alice's L2 balance → alice transfers to bob → mine ×2
#   → check both balances
#
# Cleans up bitcoind and both daemons on EXIT.
#
# Flags:
#   --keep-running   After the demo finishes, leave bitcoind +
#                    sequencer + node alive and pause waiting for
#                    the user to press enter. Useful for poking at
#                    http://127.0.0.1:28080/docs/ (Swagger UI).
#
# Requirements on $PATH: bitcoind, bitcoin-cli, curl, jq.

set -euo pipefail

KEEP_RUNNING=0
for arg in "$@"; do
    case "$arg" in
        --keep-running) KEEP_RUNNING=1 ;;
        -h|--help)
            sed -n '2,16p' "$0"
            exit 0
            ;;
        *)
            echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

# --- Layout ----------------------------------------------------------------

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA_DIR="${HODL_DEMO_DATA:-/tmp/hodl-regtest}"

# Non-default ports so we don't collide with a regtest bitcoind the user
# may already be running (the default regtest RPC port is 18443).
BTC_RPC=28443
BTC_P2P=28444
SEQ_PORT=28080
NODE_PORT=28081

WALLET_BIN="$ROOT/target/debug/hodl-wallet"
SEQ_BIN="$ROOT/target/debug/hodl-sequencer"
NODE_BIN="$ROOT/target/debug/hodl-node"

# bitcoind / bitcoin-cli. Override via env vars or set BITCOIND_PREFIX to a
# directory containing both binaries. Defaults try $PATH, then the common
# local install path.
BITCOIND_PREFIX="${BITCOIND_PREFIX:-$HOME/code/bitcoin-28.0/bin}"
find_btc_bin() {
    local name="$1"
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
    elif [ -x "$BITCOIND_PREFIX/$name" ]; then
        echo "$BITCOIND_PREFIX/$name"
    else
        echo ""
    fi
}
BITCOIND_BIN="${BITCOIND_BIN:-$(find_btc_bin bitcoind)}"
BITCOIN_CLI_BIN="${BITCOIN_CLI_BIN:-$(find_btc_bin bitcoin-cli)}"

# --- Tracing helpers -------------------------------------------------------

# Colors only if stdout is a tty.
if [ -t 1 ]; then
    C_HEAD=$'\033[1;36m'; C_OK=$'\033[1;32m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
    C_HEAD=''; C_OK=''; C_DIM=''; C_RST=''
fi
say()  { printf '%s==>%s %s\n'  "$C_HEAD" "$C_RST" "$*"; }
ok()   { printf '%s    %s%s\n' "$C_OK"   "$*" "$C_RST"; }
dim()  { printf '%s    %s%s\n' "$C_DIM"  "$*" "$C_RST"; }

require_path() {
    command -v "$1" >/dev/null 2>&1 || { echo "required binary missing on PATH: $1"; exit 1; }
}
require_path curl
require_path jq
require_path cargo
[ -x "$BITCOIND_BIN" ]     || { echo "bitcoind not found (looked at \$PATH and $BITCOIND_PREFIX); set BITCOIND_BIN or BITCOIND_PREFIX"; exit 1; }
[ -x "$BITCOIN_CLI_BIN" ]  || { echo "bitcoin-cli not found (looked at \$PATH and $BITCOIND_PREFIX); set BITCOIN_CLI_BIN or BITCOIND_PREFIX"; exit 1; }

# --- Cleanup trap ----------------------------------------------------------

PIDS=()
BITCOIND_RUNNING=0
cleanup() {
    local rc=$?
    set +e
    say "cleanup"
    for pid in "${PIDS[@]:-}"; do
        kill "$pid" 2>/dev/null
    done
    if [ "$BITCOIND_RUNNING" -eq 1 ]; then
        "$BITCOIN_CLI_BIN" -datadir="$DATA_DIR/bitcoin" -regtest stop 2>/dev/null
        for _ in {1..10}; do
            pgrep -f "bitcoind .*-datadir=$DATA_DIR/bitcoin" >/dev/null 2>&1 || break
            sleep 0.3
        done
    fi
    exit $rc
}
trap cleanup EXIT INT TERM

# --- Build -----------------------------------------------------------------

say "building hodlcoin binaries"
(cd "$ROOT" && cargo build --workspace 2>&1) | tail -1
ok "binaries: $WALLET_BIN, $SEQ_BIN, $NODE_BIN"

# --- bitcoind --------------------------------------------------------------

say "fresh data dir: $DATA_DIR"
rm -rf "$DATA_DIR"
mkdir -p "$DATA_DIR/bitcoin"

cat > "$DATA_DIR/bitcoin/bitcoin.conf" <<EOF
fallbackfee=0.00001
# txindex=1 lets `getrawtransaction <txid>` succeed without a wallet
# context or a blockhash hint — required for the node's Esplora-
# compatible /tx/:txid endpoint that light wallets walk through.
txindex=1
[regtest]
rpcport=$BTC_RPC
EOF

say "starting bitcoind in regtest mode ($BITCOIND_BIN)"
"$BITCOIND_BIN" -datadir="$DATA_DIR/bitcoin" -regtest \
    -rpcport="$BTC_RPC" -rpcbind=127.0.0.1 -rpcallowip=127.0.0.1 \
    -listen=0 -daemon >/dev/null
BITCOIND_RUNNING=1

COOKIE="$DATA_DIR/bitcoin/regtest/.cookie"
for _ in {1..30}; do
    if [ -f "$COOKIE" ] && "$BITCOIN_CLI_BIN" -datadir="$DATA_DIR/bitcoin" -regtest getblockcount >/dev/null 2>&1; then
        ok "bitcoind RPC ready"
        break
    fi
    sleep 0.3
done

btc()      { "$BITCOIN_CLI_BIN" -datadir="$DATA_DIR/bitcoin" -regtest "$@"; }
btc_user() { "$BITCOIN_CLI_BIN" -datadir="$DATA_DIR/bitcoin" -regtest -rpcwallet=user "$@"; }
btc_seq()  { "$BITCOIN_CLI_BIN" -datadir="$DATA_DIR/bitcoin" -regtest -rpcwallet=sequencer "$@"; }

# --- Wallets in bitcoind ---------------------------------------------------

say "creating bitcoind wallets"
btc createwallet user      >/dev/null
btc createwallet sequencer >/dev/null

USER_ADDR=$(btc_user getnewaddress "" bech32m)
btc generatetoaddress 101 "$USER_ADDR" >/dev/null
ok "mined 101 blocks; user has $(btc_user getbalance) BTC"

# Fund the sequencer wallet so it can pay fees for OP_RETURN posts.
SEQ_FUND=$(btc_seq getnewaddress "" bech32m)
btc_user sendtoaddress "$SEQ_FUND" 1.0 >/dev/null
btc generatetoaddress 1 "$USER_ADDR" >/dev/null
ok "sequencer has $(btc_seq getbalance) BTC for OP_RETURN fees"

L1_GENESIS=$(btc getblockcount)
ok "L2 will anchor at L1 height $L1_GENESIS"

# --- Configs ---------------------------------------------------------------

mkdir -p "$DATA_DIR/seq" "$DATA_DIR/node" "$DATA_DIR/wallet"

cat > "$DATA_DIR/seq/config.json" <<EOF
{
  "network": "regtest",
  "bitcoind": {
    "url": "http://127.0.0.1:$BTC_RPC/wallet/sequencer",
    "auth": { "kind": "cookie", "path": "$COOKIE" }
  },
  "l1_genesis_height": $L1_GENESIS,
  "listen": "127.0.0.1:$SEQ_PORT",
  "db_path": "$DATA_DIR/seq/hodl-sequencer.db",
  "poll_ms": 500
}
EOF

cat > "$DATA_DIR/node/config.json" <<EOF
{
  "network": "regtest",
  "bitcoind": {
    "url": "http://127.0.0.1:$BTC_RPC",
    "auth": { "kind": "cookie", "path": "$COOKIE" }
  },
  "sequencer_url": "http://127.0.0.1:$SEQ_PORT",
  "l1_genesis_height": $L1_GENESIS,
  "listen": "127.0.0.1:$NODE_PORT",
  "db_path": "$DATA_DIR/node/hodl-node.db",
  "poll_ms": 500
}
EOF

# --- Start sequencer + node ------------------------------------------------

say "starting hodl-sequencer"
"$SEQ_BIN" run --config "$DATA_DIR/seq/config.json" \
    >"$DATA_DIR/seq/log" 2>&1 &
PIDS+=($!)

for _ in {1..40}; do
    if curl -sf "http://127.0.0.1:$SEQ_PORT/head" >/dev/null; then break; fi
    sleep 0.25
done
HEAD_SEQ=$(curl -s "http://127.0.0.1:$SEQ_PORT/head")
ok "sequencer up — $(echo "$HEAD_SEQ" | jq -c '{height,l1_height}')"

say "starting hodl-node"
"$NODE_BIN" run --config "$DATA_DIR/node/config.json" \
    >"$DATA_DIR/node/log" 2>&1 &
PIDS+=($!)

for _ in {1..40}; do
    if curl -sf "http://127.0.0.1:$NODE_PORT/head" >/dev/null; then break; fi
    sleep 0.25
done
HEAD_NODE=$(curl -s "http://127.0.0.1:$NODE_PORT/head")
ok "node up      — $(echo "$HEAD_NODE" | jq -c '{height,l1_height}')"

# --- hodl-wallets ----------------------------------------------------------

ALICE_WALLET="$DATA_DIR/wallet/alice.json"
BOB_WALLET="$DATA_DIR/wallet/bob.json"

say "keygen Alice & Bob"
"$WALLET_BIN" --wallet "$ALICE_WALLET" keygen \
    --network regtest \
    --bitcoind-url "http://127.0.0.1:$BTC_RPC/wallet/user" \
    --bitcoind-cookie "$COOKIE" \
    --sequencer-url "http://127.0.0.1:$SEQ_PORT" \
    --node-url "http://127.0.0.1:$NODE_PORT" \
    --esplora-url "http://127.0.0.1:$NODE_PORT" \
    | sed 's/^/    /'
"$WALLET_BIN" --wallet "$BOB_WALLET" keygen \
    --network regtest \
    --bitcoind-url "http://127.0.0.1:$BTC_RPC/wallet/user" \
    --bitcoind-cookie "$COOKIE" \
    --sequencer-url "http://127.0.0.1:$SEQ_PORT" \
    --node-url "http://127.0.0.1:$NODE_PORT" \
    | sed 's/^/    /'

ALICE_ADDR=$("$WALLET_BIN" --wallet "$ALICE_WALLET" address)
BOB_ADDR=$("$WALLET_BIN" --wallet "$BOB_WALLET" address)
dim "alice L2 address: $ALICE_ADDR"
dim "bob   L2 address: $BOB_ADDR"

# --- Step 1: Alice creates a CSV-locked mint UTXO --------------------------

say "alice creates a CSV-locked mint UTXO (T=10000 blocks ≈ 70 days, V=0.1 BTC)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" mint-utxo \
    --lock-blocks 10000 --value-btc 0.1 | sed 's/^/    /'

btc generatetoaddress 1 "$USER_ADDR" >/dev/null
ok "mined 1 block — funding UTXO confirmed"

OUTPOINT=$("$WALLET_BIN" --wallet "$ALICE_WALLET" list-mints | head -1 | awk '{print $1}')
dim "outpoint: $OUTPOINT"

# --- Step 2: Alice submits the mint message --------------------------------

say "alice submits mint message"
"$WALLET_BIN" --wallet "$ALICE_WALLET" mint-message \
    --outpoint "$OUTPOINT" | sed 's/^/    /'

# --- Step 3: drive L1 forward so sequencer produces + attestation lands ----

# Block N → sequencer produces L2 block 1 + broadcasts OP_RETURN.
# Block N+1 → OP_RETURN tx confirms; node sees it.
say "mining 2 blocks to land the attestation"
btc generatetoaddress 1 "$USER_ADDR" >/dev/null
sleep 1.5
btc generatetoaddress 1 "$USER_ADDR" >/dev/null
sleep 2.0

# Help the node catch up if it's still ticking.
for _ in {1..20}; do
    NODE_HEAD=$(curl -s "http://127.0.0.1:$NODE_PORT/head" | jq -r '.height')
    if [ "$NODE_HEAD" -ge 1 ]; then break; fi
    sleep 0.25
done

dim "sequencer head: $(curl -s http://127.0.0.1:$SEQ_PORT/head | jq -c '{height,l1_height,state_root}')"
dim "node      head: $(curl -s http://127.0.0.1:$NODE_PORT/head | jq -c '{height,l1_height,state_root}')"

# --- Step 4: read alice's L2 balance --------------------------------------

say "alice's L2 balance (via node)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" balance | sed 's/^/    /'

# --- Step 5: Alice transfers to Bob ---------------------------------------

# Compute a transfer amount that won't exceed Alice's balance.
ALICE_BAL=$(curl -s "http://127.0.0.1:$NODE_PORT/balance/$ALICE_ADDR" | jq -r '.balance')
if [ "$ALICE_BAL" -eq 0 ]; then
    echo "alice balance is 0 — sequencer/node didn't pick the mint up; check $DATA_DIR/seq/log and $DATA_DIR/node/log"
    exit 1
fi
TRANSFER=$(( ALICE_BAL / 4 ))
say "alice transfers $TRANSFER atoms to bob"
"$WALLET_BIN" --wallet "$ALICE_WALLET" transfer \
    --to "$BOB_ADDR" --amount "$TRANSFER" | sed 's/^/    /'

say "mining 2 blocks to land the transfer"
btc generatetoaddress 1 "$USER_ADDR" >/dev/null
sleep 1.5
btc generatetoaddress 1 "$USER_ADDR" >/dev/null
sleep 2.0

for _ in {1..20}; do
    BOB_BAL=$(curl -s "http://127.0.0.1:$NODE_PORT/balance/$BOB_ADDR" | jq -r '.balance')
    if [ "$BOB_BAL" -gt 0 ]; then break; fi
    sleep 0.25
done

# --- Step 6: final balances -----------------------------------------------

say "final balances (via node)"
echo "    alice: $("$WALLET_BIN" --wallet "$ALICE_WALLET" balance | grep balance)"
echo "    bob:   $("$WALLET_BIN" --wallet "$BOB_WALLET"   balance | grep balance)"
say "node head"
curl -s "http://127.0.0.1:$NODE_PORT/head" | jq . | sed 's/^/    /'

# Cryptographically verify Alice's balance via the inclusion proof.
say "verifying alice's balance against the node's state_root (Phase 2)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" verify-balance | sed 's/^/    /'

# Also verify a non-existent third-party address — should come back as
# an empty-leaf proof that still verifies.
THIRD=0000000000000000000000000000000000000000000000000000000000000001
say "verifying a non-existent address (expect: empty leaf, balance=0)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" verify-balance --addr "$THIRD" | sed 's/^/    /'

# Phase 3: light-client mode. Walk the L1 attestation chain via the
# Esplora-compatible endpoints on hodl-node, derive the current
# state_root from L1, then verify alice's inclusion proof against THAT
# state_root. No bitcoind RPC used by the wallet for this step.
say "light-client head (derive state_root from L1 attestation chain via Esplora)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" light-head | sed 's/^/    /'

say "light-client balance for alice (L1-walk + proof verify, end-to-end)"
"$WALLET_BIN" --wallet "$ALICE_WALLET" light-balance | sed 's/^/    /'

say "OpenAPI specs"
echo -n "    sequencer /openapi.json: "
curl -sf "http://127.0.0.1:$SEQ_PORT/openapi.json" \
    | jq -c '{title: .info.title, paths: (.paths | keys), schemas: (.components.schemas | keys | length)}'
echo -n "    node      /openapi.json: "
curl -sf "http://127.0.0.1:$NODE_PORT/openapi.json" \
    | jq -c '{title: .info.title, paths: (.paths | keys), schemas: (.components.schemas | keys | length)}'

dim "    Swagger UI available while daemons run:"
dim "        sequencer: http://127.0.0.1:$SEQ_PORT/docs/"
dim "        node:      http://127.0.0.1:$NODE_PORT/docs/"

ok "demo complete"

if [ "$KEEP_RUNNING" -eq 1 ]; then
    echo
    say "--keep-running set: leaving daemons + bitcoind alive"
    dim "  press enter to tear down (or Ctrl-C — trap will still fire)..."
    # `|| true` so set -e doesn't trip if the user hits EOF instead of enter.
    read -r _ || true
fi
echo
dim "logs:"
dim "  sequencer: $DATA_DIR/seq/log"
dim "  node:      $DATA_DIR/node/log"
