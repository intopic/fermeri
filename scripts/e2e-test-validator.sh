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
# Sonda e auditimit (Faza 0): çfarë thërret validatori dhe kur.
export ZILJA_PROBE="$WORK/probe.log"
# F-01 me e2e: 0 = vetëm raporto (deri në Fazën 1), 1 = dështo.
STRICT_F01="${STRICT_F01:-0}"

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
# F-01: një transferim i finalizuar PAS snapshot-it të fundit, pastaj ndalim para snapshot-it tjetër.
# Pas rinisjes validatori ngarkon snapshot-in dhe e ripërsërit këtë slot nga ledger-i gjatë nisjes.
snap_slot() { ls "$LEDGER"/snapshot-*.tar* 2>/dev/null | sed -E 's/.*snapshot-([0-9]+)-.*/\1/' | sort -n | tail -1; }
for _ in $(seq 1 120); do [ -n "$(snap_slot)" ] && break; sleep 1; done
S0=$(snap_slot)
for _ in $(seq 1 180); do [ "$(snap_slot)" != "$S0" ] && break; sleep 1; done
solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/f01.json" >/dev/null
F01=$(solana-keygen pubkey "$WORK/f01.json")
F01_SIG=$(solana -u "$RPC" transfer --keypair "$WORK/payer.json" --allow-unfunded-recipient "$F01" 0.123 --commitment finalized --output json | python3 -c 'import json,sys; print(json.load(sys.stdin)["signature"])')
F01_SLOT=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$F01_SIG\",{\"commitment\":\"finalized\"}]}" "$RPC" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["slot"])')
# Prit që rrënja e ledger-it të kalojë qartë slotin e transferimit (që të ripërsëritet gjatë nisjes).
wait_rooted_past "$((F01_SLOT + 40))"
BEFORE=$(zroot)
echo "$(date +%s%3N) MARK stop" >> "$ZILJA_PROBE"
stop
S1=$(snap_slot)
echo "   llogaria e provës F-01: $F01, 0.123 SOL në slotin $F01_SLOT (i finalizuar); snapshot-i i fundit: $S1"
[ "$F01_SLOT" -gt "$S1" ] || echo "   ⚠️ snapshot-i e mbulon transferimin: hapi 4b nuk e provon F-01 këtë herë"
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
echo "== Hapi 4b: a e pa zilja ndryshimin që u ripërsërit gjatë nisjes? (F-01)"
FIN=$(solana -u "$RPC" slot --commitment finalized)
wait_rooted_past "$((FIN + 1))"
ZL=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountProof\",\"params\":[\"$F01\"]}" "$ZILJA" \
  | python3 -c 'import json,sys; a=json.load(sys.stdin)["result"]["account"]; print(a["lamports"] if a else 0)')
RL=$(solana -u "$RPC" balance "$F01" --lamports --commitment finalized | awk '{print $1}')
if [ "$ZL" = "$RL" ]; then
  echo "   ✅ zilja: $ZL lamports = RPC: $RL lamports"
elif [ "$STRICT_F01" = "1" ]; then
  echo "GABIM F-01: zilja $ZL lamports, RPC $RL lamports"; exit 1
else
  echo "   ❌ F-01 u riprodhua: zilja $ZL lamports, RPC $RL lamports (e pritur deri në Fazën 1)"
fi
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
if python3 - "$WORK/roots.log" <<'PY'
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
then :
elif [ "$STRICT_F01" = "1" ]; then
  echo "GABIM: root-i pas rinisjes ndryshon"; exit 1
else
  echo "   ❌ F-01 u riprodhua edhe këtu (e pritur deri në Fazën 1)"
fi
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
echo "== Sonda: pyetjet e hapura të auditimit"
python3 "$ROOT/scripts/probe-report.py" "$ZILJA_PROBE" || true
echo
echo "== GJITHÇKA KALOI ✅ (roots.log: $(wc -l < "$WORK/roots.log") rreshta)"
