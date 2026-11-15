#!/usr/bin/env bash
# Testi i plotë mbi një validator të vërtetë Agave (solana-test-validator).
#
#   1. Nis validatorin me plugin-in e Ziles.
#   2. Bën transferime të vërteta SOL.
#   3. Pret që slotet të rrënjosen dhe verifikon llogaritë me zilja-check.
#   4. Rinis validatorin nga ledger-i (nisje nga snapshot) dhe kontrollon që
#      pema rindërtohet me të gjitha llogaritë.
#
# Përdorimi: scripts/e2e-test-validator.sh [rruga/te/solana-release/bin]
# Me ZILJA_DB=1 pema mbahet në disk (RocksDB) dhe rinisja vazhdon nga disku, pa snapshot.
set -euo pipefail

BIN="${1:-$HOME/.local/share/solana/install/active_release/bin}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK"
LEDGER="$WORK/ledger"
LIB="$ROOT/target/release/libzilja_plugin.so"
CHECK="$ROOT/target/release/zilja-check"
RPC="http://127.0.0.1:8899"
ZILJA="http://127.0.0.1:7799"
export PATH="$BIN:$PATH"
export NO_PROXY="127.0.0.1,localhost" no_proxy="127.0.0.1,localhost"

[ -f "$LIB" ] || { echo "Mungon $LIB: cargo build --release -p zilja-plugin"; exit 2; }
[ -x "$CHECK" ] || { echo "Mungon $CHECK: cargo build --release -p zilja-cli"; exit 2; }

DBCONF=""
if [ "${ZILJA_DB:-0}" = "1" ]; then
  DBCONF=", \"db_path\": \"$WORK/zdb\", \"history_slots\": 256, \"cache_depth\": 4"
fi
cat > "$WORK/zilja.json" <<EOF
{ "libpath": "$LIB", "bind": "127.0.0.1:7799", "roots_log": "$WORK/roots.log",
  "attest_keypair": "$LEDGER/validator-keypair.json" $DBCONF }
EOF
cat "$WORK/zilja.json"

VPID=""
stop() { [ -n "$VPID" ] && kill "$VPID" 2>/dev/null && wait "$VPID" 2>/dev/null || true; VPID=""; }
trap stop EXIT

start() {
  solana-test-validator --ledger "$LEDGER" --geyser-plugin-config "$WORK/zilja.json" \
    --quiet --rpc-port 8899 "$@" > "$WORK/validator.out" 2>&1 &
  VPID=$!
  for _ in $(seq 1 120); do
    curl -s --noproxy '*' "$ZILJA/health" >/dev/null 2>&1 && solana -u "$RPC" cluster-version >/dev/null 2>&1 && return 0
    sleep 1
  done
  echo "Validatori nuk u nis"; tail -50 "$LEDGER/validator.log" || true; exit 2
}

metric() { curl -s --noproxy '*' "$ZILJA/metrics" | awk -v m="zilja_$1" '$1==m {print $2}'; }
zroot() { curl -s --noproxy '*' -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot"}' "$ZILJA"; }

wait_rooted_past() {
  local target=$1
  for _ in $(seq 1 120); do
    local s; s=$(metric last_rooted_slot)
    [ "${s:-0}" -ge "$target" ] && return 0
    sleep 1
  done
  echo "Zilja nuk arriti slotin $target"; exit 1
}

echo "== Hapi 1: nis validatorin me Ziles (punë në $WORK)"
start --reset
solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/payer.json" >/dev/null
solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/fermer.json" >/dev/null
PAYER=$(solana-keygen pubkey "$WORK/payer.json")
FERMER=$(solana-keygen pubkey "$WORK/fermer.json")
NOBODY=$(solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/nobody.json" >/dev/null; solana-keygen pubkey "$WORK/nobody.json")

echo "== Hapi 2: transferime të vërteta"
solana -u "$RPC" airdrop 10 "$PAYER" --commitment finalized >/dev/null
solana -u "$RPC" transfer --keypair "$WORK/payer.json" --allow-unfunded-recipient "$FERMER" 1.5 \
  --commitment finalized >/dev/null
solana -u "$RPC" transfer --keypair "$WORK/payer.json" "$FERMER" 0.25 --commitment finalized >/dev/null
SLOT=$(solana -u "$RPC" slot --commitment finalized)
echo "   transferimi u finalizua në slotin ~$SLOT; pres që Zilja ta rrënjosë"
wait_rooted_past "$SLOT"

echo "== Hapi 3: banori verifikon"
"$CHECK" "$ZILJA" "$FERMER" "$RPC"
"$CHECK" "$ZILJA" "$PAYER" "$RPC"
"$CHECK" "$ZILJA" "$NOBODY"
echo "-- i njëjti kontroll me verifikuesin JavaScript (kodi i faqes demo):"
node "$ROOT/web/check.mjs" "$ZILJA" "$FERMER" "$RPC"
node "$ROOT/web/check.mjs" "$ZILJA" "$NOBODY"
echo "-- SDK zilja-verify (paketa npm):"
node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" --rpc "$RPC"
node "$ROOT/sdk/js/examples/verify.mjs" "$NOBODY" --zilja "$ZILJA"
if node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" --min-trust agreement >/dev/null; then
  echo "GABIM: me një zile të vetme, minTrust=agreement s'duhet të kalojë"; exit 1
fi
echo "   ✅ SDK: minTrust=agreement me një zile refuzohet siç duhet"
echo "-- root i rremë duhet të refuzohet:"
if "$CHECK" "$ZILJA" "$FERMER" --root "$(printf 'ab%.0s' $(seq 1 32))"; then
  echo "GABIM: root i rremë u pranua"; exit 1
fi
if node "$ROOT/web/check.mjs" "$ZILJA" "$FERMER" --root "$(printf 'ab%.0s' $(seq 1 32))"; then
  echo "GABIM: root i rremë u pranua nga JS"; exit 1
fi
EXPECTED=$((1750000000))
GOT=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountProof\",\"params\":[\"$FERMER\"]}" "$ZILJA" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["account"]["lamports"])')
[ "$GOT" = "$EXPECTED" ] || { echo "GABIM: lamports $GOT != $EXPECTED"; exit 1; }
echo "   ✅ balanca e fermerit në zile: $GOT lamports (1.75 SOL)"
echo "-- metrikat pas fazës së parë:"
curl -s --noproxy '*' "$ZILJA/metrics" | grep -v '^#'
zroot; echo

echo "== Hapi 4: rinis nga ledger-i (nisje nga snapshot)"
# Prit që të krijohet të paktën një snapshot.
for _ in $(seq 1 120); do ls "$LEDGER"/snapshot-*.tar* >/dev/null 2>&1 && break; sleep 1; done
BEFORE=$(zroot)
stop
sleep 2
start
if [ "${ZILJA_DB:-0}" = "1" ]; then
  AFTER=$(zroot)
  echo "   para ndalimit: $BEFORE"
  echo "   pas rinisjes:  $AFTER"
  [ "$(metric resumed)" = "1" ] || { echo "GABIM: zilja nuk vazhdoi nga disku"; exit 1; }
  [ "$(metric startup_accounts)" = "0" ] || { echo "GABIM: zilja rindërtoi nga snapshot-i"; exit 1; }
  python3 -c 'import json,sys; a=json.loads(sys.argv[1])["result"]; b=json.loads(sys.argv[2])["result"]; sys.exit(0 if a["root"]==b["root"] or b["slot"]>a["slot"] else 1)' "$BEFORE" "$AFTER" \
    || { echo "GABIM: gjendja pas rinisjes ndryshon nga ajo para ndalimit"; exit 1; }
  echo "   ✅ vazhdoi nga disku pa snapshot"
else
  STARTUP=$(metric startup_accounts)
  echo "   pema fillestare: $STARTUP llogari nga snapshot-i, në $(metric startup_build_ms) ms"
  [ "${STARTUP:-0}" -gt 0 ] || { echo "GABIM: nuk erdhën llogari nga snapshot-i"; exit 1; }
fi
SLOT=$(solana -u "$RPC" slot --commitment finalized)
wait_rooted_past "$((SLOT + 1))"
"$CHECK" "$ZILJA" "$FERMER" "$RPC"
# Llogari që nuk preket më: duhet të jetë aty pas rinisjes.
"$CHECK" "$ZILJA" "11111111111111111111111111111111" "$RPC"
curl -s --noproxy '*' "$ZILJA/metrics" | grep -v '^#'
if [ "${ZILJA_DB:-0}" = "1" ]; then
  echo "== Hapi 5 (disk): dëftesë historike dhe asnjë ndalim"
  [ "$(metric broken)" = "0" ] || { echo "GABIM: zilja ndaloi"; exit 1; }
  [ "$(metric resume_verified)" = "1" ] || { echo "GABIM: blockhash-i pas rinisjes nuk u kontrollua"; exit 1; }
  echo "   ✅ gjurma e zinxhirit (SlotHashes) e slotit të ruajtur u gjet pas rinisjes"
  OLD_SLOT=$(metric last_rooted_slot)
  solana -u "$RPC" transfer --keypair "$WORK/payer.json" "$FERMER" 0.5 --commitment finalized >/dev/null
  SLOT=$(solana -u "$RPC" slot --commitment finalized)
  wait_rooted_past "$SLOT"
  "$CHECK" "$ZILJA" "$FERMER" --slot "$OLD_SLOT" | tee "$WORK/hist.txt"
  grep -q "1750000000 lamports" "$WORK/hist.txt" || { echo "GABIM: dëftesa historike s'tregon 1.75 SOL"; exit 1; }
  "$CHECK" "$ZILJA" "$FERMER" "$RPC" | tee "$WORK/now.txt"
  grep -q "2250000000 lamports" "$WORK/now.txt" || { echo "GABIM: balanca e re duhet 2.25 SOL"; exit 1; }
  echo "   ✅ sloti $OLD_SLOT: 1.75 SOL · tani: 2.25 SOL, të dyja të verifikuara"
else
echo "== Hapi 5: vazhdimësia: root-i i rindërtuar nga snapshot-i = root-i i ndërtuar slot pas sloti"
python3 - "$WORK/roots.log" <<'PY'
import sys
seen, same, diff = {}, 0, 0
for line in open(sys.argv[1]):
    slot, root = line.split()
    if slot in seen:
        if seen[slot] == root: same += 1
        else: diff += 1; print(f"   ❌ slot {slot}: {seen[slot]} != {root}")
    seen[slot] = root
print(f"   slote të llogaritura dy herë: {same} identike, {diff} të ndryshme")
sys.exit(0 if same > 0 and diff == 0 else 1)
PY
fi
echo "== Hapi 6: pleqtë firmosin ziles (kuorumi > 2/3 i stake-ut nga getVoteAccounts)"
"$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA"
echo "-- SDK me kuorum (minTrust=quorum):"
node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" --rpc "$RPC" --quorum --min-trust quorum
if [ "${ZILJA_DB:-0}" = "1" ]; then
  echo "== Hapi 7 (disk): zinxhir krejt i ri (--reset) me pemën e vjetër në disk → zilja duhet të ndalojë"
  stop
  sleep 2
  start --reset
  for _ in $(seq 1 240); do [ "$(metric broken)" = "1" ] && break; sleep 1; done
  [ "$(metric broken)" = "1" ] || { echo "GABIM: zilja nuk e kuptoi që zinxhiri ndryshoi"; exit 1; }
  R=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot"}' "$ZILJA")
  echo "   $R"
  echo "$R" | grep -q "zilja ndaloi" || { echo "GABIM: RPC-ja duhet të refuzojë"; exit 1; }
  echo "   ✅ zilja ndaloi dhe nuk jep vula të gabuara; disku u fshi për rindërtim"
fi
echo
echo "== GJITHÇKA KALOI ✅ (roots.log: $(wc -l < "$WORK/roots.log") rreshta)"
