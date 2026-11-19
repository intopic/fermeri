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
# Me ZILJA_DB=1 pema mbahet në disk (RocksDB), me ZILJA_DB=pages në skedarin e faqeve; rinisja vazhdon nga disku, pa snapshot.
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
# F-01 me e2e: 1 = dështo (parazgjedhja pas Fazës 1), 0 = vetëm raporto.
STRICT_F01="${STRICT_F01:-1}"

[ -f "$LIB" ] || { echo "Mungon $LIB: cargo build --release -p zilja-plugin"; exit 2; }
[ -x "$CHECK" ] || { echo "Mungon $CHECK: cargo build --release -p zilja-cli"; exit 2; }

DBCONF=""
if [ "${ZILJA_DB:-0}" = "1" ]; then
  # A21: motori i vjetër RocksDB, vetëm me plugin-in e ndërtuar me --features legacy-rocksdb.
  DBCONF=", \"db_path\": \"$WORK/zdb\", \"tree_engine\": \"rocksdb\", \"history_slots\": 256, \"cache_depth\": 4"
elif [ "${ZILJA_DB:-0}" = "pages" ]; then
  DBCONF=", \"db_path\": \"$WORK/zdb\", \"tree_engine\": \"pages\", \"history_slots\": 256, \"cache_depth\": 4"
fi
# Pema e cekët (cache_depth 4) me qëllim, që edhe me pak llogari të punojnë faqet në disk.
export ZILJA_ALLOW_SMALL_CACHE_DEPTH=1
cat > "$WORK/zilja.json" <<EOF
{ "libpath": "$LIB", "bind": "127.0.0.1:7799", "metrics_bind": "127.0.0.1:7798", "roots_log": "$WORK/roots.log",
  "attest_keypair": "$LEDGER/validator-keypair.json", "genesis_path": "$LEDGER/genesis.bin" $DBCONF }
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

# Pret që validatori të MOS niset (plugin-i refuzon ngarkimin) dhe që logu të përmbajë "$1".
start_refused() {
  local want=$1; shift
  solana-test-validator --ledger "$LEDGER" --geyser-plugin-config "$WORK/zilja.json" \
    --quiet --rpc-port 8899 "$@" > "$WORK/validator.out" 2>&1 &
  VPID=$!
  for _ in $(seq 1 120); do kill -0 "$VPID" 2>/dev/null || break; sleep 1; done
  if kill -0 "$VPID" 2>/dev/null; then echo "GABIM: validatori u nis, plugin-i duhej të refuzonte ($want)"; exit 1; fi
  VPID=""
  cat "$WORK/validator.out" "$LEDGER/validator.log" 2>/dev/null | grep -q "$want" || { echo "GABIM: mungon mesazhi '$want'"; tail -20 "$WORK/validator.out"; exit 1; }
}

METRICS="http://127.0.0.1:7798"
health() { curl -s --noproxy '*' -o /dev/null -w '%{http_code}' "$ZILJA/health"; }
admin_rpc() { python3 - "$LEDGER/admin.rpc" "$@" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.settimeout(60)
s.sendall((json.dumps({"jsonrpc": "2.0", "id": 1, "method": sys.argv[2], "params": sys.argv[3:]}) + "\n").encode())
buf = b""
while True:
    try:
        buf += s.recv(65536); print(json.dumps(json.loads(buf))); break
    except json.JSONDecodeError:
        continue
PY
}
metric() { curl -s --noproxy '*' "$METRICS/metrics" | awk -v m="zilja_$1" '$1==m {print $2}'; }
# A08: thread-et e pool-it Rayon të Ziles në procesin e validatorit.
rayon_threads() { cat /proc/"$VPID"/task/*/comm 2>/dev/null | grep -c '^zilja-rayon' || true; }
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

if [ "${ZILJA_DB:-0}" = "pages" ]; then
  echo "== Hapi 0 (A21): konfigurimi i vjetër (db_path pa tree_engine = RocksDB i dikurshëm) refuzohet"
  cp "$WORK/zilja.json" "$WORK/zilja.pages.json"
  sed -i 's/, "tree_engine": "pages"//' "$WORK/zilja.json"
  start_refused "tree_engine" --reset
  cp "$WORK/zilja.pages.json" "$WORK/zilja.json"
  [ ! -e "$WORK/zdb" ] || { echo "GABIM: konfigurimi i refuzuar krijoi dosjen e pemës"; exit 1; }
  echo "   ✅ refuzohet pa krijuar asgjë"
fi
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

# A01: Agave duhet ta ketë krijuar njoftuesin e entry-ve (pa të UpdateParent s'vjen kurrë).
ENTRIES=$(metric entries_seen_total); BATCHES=$(metric batches_total)
echo "   A01: entry të marra: ${ENTRIES:-0} në ${BATCHES:-0} slote të rrënjosur (UpdateParent: $(metric update_parent_seen_total))"
[ "${ENTRIES:-0}" -gt 0 ] || { echo "GABIM: Agave s'po dërgon njoftime entry: UpdateParent do të humbiste (A01)"; exit 1; }
echo "== Hapi 3: banori verifikon"
"$CHECK" "$ZILJA" "$FERMER" "$RPC" --min-trust single
"$CHECK" "$ZILJA" "$PAYER" "$RPC" --min-trust single
# L-02: pema e nisur nga genesis s'mbulon çdo llogari, ndaj nuk jep dëftesa "nuk ekziston".
if "$CHECK" "$ZILJA" "$NOBODY" --min-trust single 2>/dev/null; then echo "GABIM: dëftesë mospërfshirjeje nga pemë jo e plotë"; exit 1; fi
echo "   ✅ pema jo e plotë refuzon dëftesat 'nuk ekziston' (L-02)"
echo "-- i njëjti kontroll me verifikuesin JavaScript (kodi i faqes demo):"
node "$ROOT/web/check.mjs" "$ZILJA" "$FERMER" "$RPC" --min-trust single
if node "$ROOT/web/check.mjs" "$ZILJA" "$NOBODY" --min-trust single 2>/dev/null; then echo "GABIM: JS pranoi mospërfshirje nga pemë jo e plotë"; exit 1; fi
echo "-- SDK zilja-verify (paketa npm):"
node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" --rpc "$RPC" --min-trust single
if node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" >/dev/null; then
  echo "GABIM: me një zile të vetme, parazgjedhja (minTrust=agreement) s'duhet të kalojë"; exit 1
fi
echo "   ✅ SDK: parazgjedhja minTrust=agreement me një zile refuzohet siç duhet (L-01)"
# P-11: e njëjta parazgjedhje (agreement) në CLI dhe në faqe: një zile e vetme s'mjafton.
if "$CHECK" "$ZILJA" "$FERMER" "$RPC" >/dev/null; then echo "GABIM: zilja-check pranoi një zile të vetme me parazgjedhjen"; exit 1; fi
if node "$ROOT/web/check.mjs" "$ZILJA" "$FERMER" "$RPC" >/dev/null; then echo "GABIM: faqja pranoi një zile të vetme me parazgjedhjen"; exit 1; fi
# Dy zile (këtu e njëjta zile me dy adresa: 127.0.0.1 dhe localhost) me të njëjtin root → agreement.
ZILJA2="${ZILJA/127.0.0.1/localhost}"
"$CHECK" "$ZILJA" "$FERMER" "$RPC" --also "$ZILJA2"
node "$ROOT/web/check.mjs" "$ZILJA" "$FERMER" "$RPC" --also "$ZILJA2"
echo "   ✅ CLI = faqja = SDK: një zile refuzohet, dy zile që pajtohen kalojnë"
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
curl -s --noproxy '*' "$METRICS/metrics" | grep -v '^#'
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
# v2: epoka aktuale për autorizimin e çelësit të veçantë (validatori ndalet më poshtë).
EPOCH=$(solana -u "$RPC" epoch-info --output json | python3 -c 'import json,sys; print(json.load(sys.stdin)["epoch"])')
echo "$(date +%s%3N) MARK stop" >> "$ZILJA_PROBE"
stop
S1=$(snap_slot)
echo "   llogaria e provës F-01: $F01, 0.123 SOL në slotin $F01_SLOT (i finalizuar); snapshot-i i fundit: $S1"
[ "$F01_SLOT" -gt "$S1" ] || echo "   ⚠️ snapshot-i e mbulon transferimin: hapi 4b nuk e provon F-01 këtë herë"
# F-10: nga kjo nisje firmos një çelës i veçantë, i autorizuar një herë nga identiteti.
GENESIS=$(solana -u "$RPC" genesis-hash 2>/dev/null || true)
[ -n "$GENESIS" ] || GENESIS=$(python3 -c 'import hashlib,sys
d=hashlib.sha256(open(sys.argv[1],"rb").read()).digest();A="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";n=int.from_bytes(d,"big");s=""
while n: n,r=divmod(n,58); s=A[r]+s
print(s)' "$LEDGER/genesis.bin")
solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/attest-signer.json" >/dev/null
# v2: autorizimi vlen për 15 epoka nga epoka aktuale (marrë para ndalimit).
"$ROOT/target/release/zilja-authorize" "$LEDGER/validator-keypair.json" "$WORK/attest-signer.json" "$GENESIS" "$EPOCH" > "$WORK/zilja-authorization.json"
echo "[]" > "$WORK/revocations.json"
python3 - "$WORK/zilja.json" "$WORK" <<'PY'
import json, sys
p, w = sys.argv[1], sys.argv[2]
c = json.load(open(p))
c["attest_keypair"] = f"{w}/attest-signer.json"
c["attest_authorization"] = f"{w}/zilja-authorization.json"
c["attest_revocations"] = f"{w}/revocations.json"
# P-05: çelës tjetër → log tjetër firmash (log-u i vjetër është i lidhur me çelësin e vjetër).
c["attest_state"] = f"{w}/attest-dedicated.log"
json.dump(c, open(p, "w"), indent=1)
PY
sleep 2
start
if [ "${ZILJA_DB:-0}" != "0" ]; then
  # P-08: pas hapjes nga disku zilja s'shërben derisa zinxhiri ta konfirmojë (faza recovered → serving).
  for _ in $(seq 1 120); do [ "$(metric phase)" = "2" ] && break; sleep 1; done
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
  # Tani pema është e plotë: dëftesat "nuk ekziston" lejohen dhe verifikohen kudo.
  "$CHECK" "$ZILJA" "$NOBODY" --min-trust single
  node "$ROOT/web/check.mjs" "$ZILJA" "$NOBODY" --min-trust single
  node "$ROOT/sdk/js/examples/verify.mjs" "$NOBODY" --zilja "$ZILJA" --min-trust single
  echo "   ✅ pema e plotë jep dëftesa 'nuk ekziston' të verifikuara"
fi
SLOT=$(solana -u "$RPC" slot --commitment finalized)
wait_rooted_past "$((SLOT + 1))"
"$CHECK" "$ZILJA" "$FERMER" "$RPC" --min-trust single
# Llogari që nuk preket më: duhet të jetë aty pas rinisjes.
"$CHECK" "$ZILJA" "11111111111111111111111111111111" "$RPC" --min-trust single
curl -s --noproxy '*' "$METRICS/metrics" | grep -v '^#'
echo "== Hapi 4b: a e pa zilja ndryshimin që u ripërsërit gjatë nisjes? (F-01)"
FIN=$(solana -u "$RPC" slot --commitment finalized)
wait_rooted_past "$((FIN + 1))"
ZL=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountProof\",\"params\":[\"$F01\"]}" "$ZILJA" \
  | python3 -c 'import json,sys; a=json.load(sys.stdin).get("result",{}).get("account"); print(a["lamports"] if a else 0)')
RL=$(solana -u "$RPC" balance "$F01" --lamports --commitment finalized | awk '{print $1}')
if [ "$ZL" = "$RL" ]; then
  echo "   ✅ zilja: $ZL lamports = RPC: $RL lamports"
elif [ "$STRICT_F01" = "1" ]; then
  echo "GABIM F-01: zilja $ZL lamports, RPC $RL lamports"; exit 1
else
  echo "   ❌ F-01 u riprodhua: zilja $ZL lamports, RPC $RL lamports (e pritur deri në Fazën 1)"
fi
if [ "${ZILJA_DB:-0}" != "0" ]; then
echo "== Hapi 5 (disk): dëftesë historike dhe asnjë ndalim"
  [ "$(metric broken)" = "0" ] || { echo "GABIM: zilja ndaloi"; exit 1; }
  [ "$(metric resume_verified)" = "1" ] || { echo "GABIM: blockhash-i pas rinisjes nuk u kontrollua"; exit 1; }
  echo "   ✅ gjurma e zinxhirit (SlotHashes) e slotit të ruajtur u gjet pas rinisjes"
  OLD_SLOT=$(metric last_rooted_slot)
  solana -u "$RPC" transfer --keypair "$WORK/payer.json" "$FERMER" 0.5 --commitment finalized >/dev/null
  SLOT=$(solana -u "$RPC" slot --commitment finalized)
  wait_rooted_past "$SLOT"
  "$CHECK" "$ZILJA" "$FERMER" --min-trust single --slot "$OLD_SLOT" | tee "$WORK/hist.txt"
  grep -q "1750000000 lamports" "$WORK/hist.txt" || { echo "GABIM: dëftesa historike s'tregon 1.75 SOL"; exit 1; }
  "$CHECK" "$ZILJA" "$FERMER" "$RPC" --min-trust single | tee "$WORK/now.txt"
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
ATT=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaAttestation"}' "$ZILJA")
if [ "$(metric complete)" = "1" ]; then
  echo "$ATT" | python3 -c 'import json,sys; r=json.load(sys.stdin)["result"]; g=sys.argv[1]
assert r["genesis"]==g, (r["genesis"], g); assert r["signer"] and r["authorization"], r
print("   firma v2: genesis", g[:8]+"…, çelës i veçantë", r["signer"][:8]+"…")' "$GENESIS" || { echo "GABIM: firma v2"; exit 1; }
  "$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA" --genesis "$GENESIS"
  if "$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA" --genesis 11111111111111111111111111111111 >/dev/null; then
    echo "GABIM: kuorumi u pranua për një genesis tjetër"; exit 1
  fi
  echo "   ✅ firmat s'vlejnë për një zinxhir tjetër (F-10)"
  # Autorizimi v2 me afat: një autorizim që s'e mbulon epokën aktuale s'numërohet, dhe plugin-i e
  # rilexon skedarin pa rinisje (kthehet sërish te i vlefshmi).
  EPOCH=$(solana -u "$RPC" epoch-info --output json | python3 -c 'import json,sys; print(json.load(sys.stdin)["epoch"])')
  cp "$WORK/zilja-authorization.json" "$WORK/zilja-authorization.ok.json"
  "$ROOT/target/release/zilja-authorize" "$LEDGER/validator-keypair.json" "$WORK/attest-signer.json" "$GENESIS" "$(( EPOCH + 20 ))" 2>/dev/null > "$WORK/auth.tmp"
  mv "$WORK/auth.tmp" "$WORK/zilja-authorization.json"
  auth_from() { curl -s --noproxy '*' -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaAttestation"}' "$ZILJA" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["authorization"]["fromEpoch"])'; }
  for _ in $(seq 1 30); do [ "$(auth_from)" = "$(( EPOCH + 20 ))" ] && break; sleep 1; done
  [ "$(auth_from)" = "$(( EPOCH + 20 ))" ] || { echo "GABIM: autorizimi i ri s'u rilexua pa rinisje"; exit 1; }
  if "$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA" --genesis "$GENESIS" >/dev/null; then
    echo "GABIM: kuorumi u pranua me autorizim që s'e mbulon epokën aktuale"; exit 1
  fi
  OK_FROM=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fromEpoch"])' "$WORK/zilja-authorization.ok.json")
  mv "$WORK/zilja-authorization.ok.json" "$WORK/zilja-authorization.json"
  for _ in $(seq 1 30); do [ "$(auth_from)" = "$OK_FROM" ] && break; sleep 1; done
  echo "   autorizimi pas kthimit: fromEpoch $(auth_from) (pritej $OK_FROM)"
  "$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA" --genesis "$GENESIS" > "$WORK/quorum-after.txt" || { cat "$WORK/quorum-after.txt"; echo "GABIM: kuorumi s'u kthye pas rinovimit"; exit 1; }
  echo "   ✅ autorizimi jashtë periudhës s'numërohet; rinovimi lexohet pa rinisje (v2)"
  echo "-- SDK me kuorum (minTrust=quorum):"
  node "$ROOT/sdk/js/examples/verify.mjs" "$FERMER" --zilja "$ZILJA" --rpc "$RPC" --quorum --min-trust quorum
  # R5-3: aggregator-i i pabesuar: firmat me një thirrje; SDK-ja i verifikon vetë.
  if [ -x "$ROOT/target/release/zilja-aggregator" ]; then
    AGG="http://127.0.0.1:17800"
    "$ROOT/target/release/zilja-aggregator" --bind 127.0.0.1:17800 --zilja "$ZILJA" > "$WORK/aggregator.log" 2>&1 &
    AGG_PID=$!
    for _ in $(seq 1 30); do curl -s --noproxy '*' "$AGG/health" | grep -q ok && break; sleep 0.5; done
    node --input-type=module -e '
import { ZiljaClient } from "'"$ROOT"'/sdk/js/src/index.js";
const [z, ag, rpc, g, pk] = process.argv.slice(1);
const slot = (await (await fetch(z, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getZiljaRoot" }) })).json()).result.slot;
const b = (await (await fetch(ag, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getZiljaQuorumBundle", params: [slot] }) })).json()).result;
if (!b || b.answered !== 1 || b.attestations.length !== 1) { console.error("GABIM: bundle", JSON.stringify(b)); process.exit(1); }
const r = await new ZiljaClient({ endpoints: [z], aggregators: [ag], solanaRpc: rpc, quorum: true, minTrust: "quorum", genesisHash: g }).verifyAccount(pk);
if (!r.ok || r.trust !== "quorum") { console.error("GABIM: kuorum me aggregator", r.trust, r.errors); process.exit(1); }
console.log("   ✅ aggregator: bundle me", b.attestations.length, "firmë @ slot", slot, "→ kuorum i verifikuar nga SDK-ja (R5-3)");
' "$ZILJA" "$AGG" "$RPC" "$GENESIS" "$FERMER" || { kill $AGG_PID; exit 1; }
    kill $AGG_PID
  fi
  [ "$(metric attest_refused_total)" = "0" ] || { echo "GABIM: firma të refuzuara pas rinisjes (root tjetër për të njëjtin slot?)"; exit 1; }
  # R5-1: revokimi i firmosur nga identiteti: zilja e shërben menjëherë (pa rinisje), ndalon së
  # firmosuri me atë çelës, dhe kuorumi s'numërohet më, edhe brenda periudhës së autorizimit.
  SIGNER=$(solana-keygen pubkey "$WORK/attest-signer.json")
  "$ROOT/target/release/zilja-authorize" --revoke "$LEDGER/validator-keypair.json" "$SIGNER" "$GENESIS" 2>/dev/null > "$WORK/rev.tmp"
  python3 -c 'import json,sys; json.dump([json.load(open(sys.argv[1]))], open(sys.argv[2],"w"))' "$WORK/rev.tmp" "$WORK/revocations.json"
  nrev() { curl -s --noproxy '*' -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaRevocations"}' "$ZILJA" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["result"]))'; }
  for _ in $(seq 1 30); do [ "$(nrev)" = "1" ] && break; sleep 1; done
  [ "$(nrev)" = "1" ] || { echo "GABIM: revokimi s'u shërbye pa rinisje"; exit 1; }
  if "$ROOT/target/release/zilja-quorum" "$RPC" "$ZILJA" --genesis "$GENESIS" > "$WORK/quorum-revoked.txt"; then
    cat "$WORK/quorum-revoked.txt"; echo "GABIM: kuorumi u pranua me çelës të revokuar"; exit 1
  fi
  for _ in $(seq 1 30); do [ "$(metric attest_refused_total)" != "0" ] && break; sleep 1; done
  [ "$(metric attest_refused_total)" != "0" ] || { echo "GABIM: zilja vazhdoi të firmosë me çelës të revokuar"; exit 1; }
  echo "   ✅ revokimi: shërbehet pa rinisje, kuorumi s'numërohet, zilja ndalon së firmosuri (R5-1)"
  # Procedura (OPERIMI.md): çelës i ri + autorizim i ri + log i ri. Me çelësin e revokuar plugin-i
  # s'ngarkohet më (hapat e mëtejshëm e ringarkojnë).
  solana-keygen new --force --no-bip39-passphrase -s -o "$WORK/attest-signer2.json" >/dev/null
  "$ROOT/target/release/zilja-authorize" "$LEDGER/validator-keypair.json" "$WORK/attest-signer2.json" "$GENESIS" "$EPOCH" 2>/dev/null > "$WORK/zilja-authorization2.json"
  python3 - "$WORK/zilja.json" "$WORK" <<'PY'
import json, sys
c = json.load(open(sys.argv[1])); w = sys.argv[2]
c["attest_keypair"] = f"{w}/attest-signer2.json"; c["attest_authorization"] = f"{w}/zilja-authorization2.json"
c["attest_state"] = f"{w}/attest-dedicated2.log"
json.dump(c, open(sys.argv[1], "w"), indent=1)
PY
else
  echo "$ATT" | grep -q "s'firmos" || { echo "GABIM: pema jo e plotë s'duhet të firmosë: $ATT"; exit 1; }
  echo "   ✅ pema jo e plotë (nisur nga genesis) nuk firmos (F-10)"
fi
if [ "${ZILJA_DB:-0}" != "0" ]; then
  echo "== Hapi 7 (disk): zinxhir krejt i ri (--reset) me pemën e vjetër në disk → zilja duhet të ndalojë"
  stop
  # Autorizimi i çelësit lidhet me genesis-in; zinxhiri i ri ka genesis tjetër, ndaj kthehu te
  # identiteti, me log të ri firmash (P-05: log-u është i lidhur me genesis-in dhe çelësin).
  python3 - "$WORK/zilja.json" "$LEDGER" "$WORK" <<'PY'
import json, sys
c = json.load(open(sys.argv[1])); c.pop("attest_authorization", None); c["attest_keypair"] = sys.argv[2] + "/validator-keypair.json"
c["attest_state"] = sys.argv[3] + "/attest-reset.log"
json.dump(c, open(sys.argv[1], "w"), indent=1)
PY
  sleep 2
  if [ "${ZILJA_DB:-0}" = "pages" ]; then
    # P-05: pema me faqe është e lidhur me genesis-in (MANIFEST): s'hapet fare në rrjetin e ri.
    start_refused "genesis" --reset
    echo "   ✅ pema e genesis-it të vjetër u refuzua në ngarkim; skedarët u ruajtën"
    [ -f "$WORK/zdb/MANIFEST" ] && [ -s "$WORK/zdb/pages.dat" ] || { echo "GABIM: pema e vjetër u prek"; exit 1; }
    echo "== Hapi 8 (pages): procedura: zhvendos dosjen, rinis → pema ndërtohet për rrjetin e ri"
    mv "$WORK/zdb" "$WORK/zdb.genesis-vjeter"
    start
    wait_rooted_past "$(( $(solana -u "$RPC" slot --commitment finalized) + 1 ))"
    [ "$(metric broken)" = "0" ] && [ "$(health)" = "200" ] || { echo "GABIM: pas rindërtimit"; exit 1; }
    grep -q "$(solana -u "$RPC" genesis-hash)" "$WORK/zdb/MANIFEST" || { echo "GABIM: MANIFEST pa genesis-in e ri"; exit 1; }
    echo "   ✅ u ndërtua për genesis-in e ri"
    echo "== Hapi 9 (pages): ringarkimi i plugin-it me admin RPC → LOCK-u lirohet; pema s'mund të vazhdojë"
    RAYON_BEFORE=$(rayon_threads)
    admin_rpc reloadPlugin zilja "$WORK/zilja.json"
    kill -0 "$VPID" || { echo "GABIM: validatori ra"; exit 1; }
    RAYON_AFTER=$(rayon_threads)
    echo "   A08: thread-e zilja-rayon para/pas ringarkimit: $RAYON_BEFORE / $RAYON_AFTER"
    [ "$RAYON_BEFORE" -gt 0 ] && [ "$RAYON_AFTER" = "$RAYON_BEFORE" ] || { echo "GABIM: thread-et Rayon të instancës së vjetër mbetën pas unload (A08)"; exit 1; }
    # Instanca e re hapi të njëjtën dosje: lock-u i së vjetrës u lirua (P-01).
    for _ in $(seq 1 60); do [ "$(metric resumed)" = "1" ] && break; sleep 1; done
    [ "$(metric resumed)" = "1" ] || { echo "GABIM: instanca e re s'e hapi pemën (LOCK?)"; tail -20 "$WORK/validator.out"; exit 1; }
    # Bankat në fluturim gjatë ringarkimit i mori instanca e vjetër: zinxhiri s'lidhet → ndalim (fail-closed).
    for _ in $(seq 1 60); do [ "$(metric broken)" = "1" ] && break; sleep 1; done
    [ "$(metric broken)" = "1" ] && [ "$(health)" = "503" ] || { echo "GABIM: pas ringarkimit zilja duhej të ndalonte, jo të shërbente"; exit 1; }
    echo "   ✅ ringarkimi: LOCK-u u lirua, instanca e re ndaloi fail-closed (bankat në fluturim humbën)"
    echo "== Hapi 10 (pages): rinisja e validatorit e rindërton pemën e shënuar"
    stop
    sleep 2
    start
    [ "$(metric resumed)" = "0" ] || { echo "GABIM: pema e shënuar s'duhej të vazhdonte"; exit 1; }
    wait_rooted_past "$(( $(solana -u "$RPC" slot --commitment finalized) + 3 ))"
    [ "$(metric broken)" = "0" ] && [ "$(health)" = "200" ] || { echo "GABIM: pas rindërtimit"; exit 1; }
    echo "   ✅ u rindërtua dhe punon"
  else
  start --reset
  for _ in $(seq 1 240); do [ "$(metric broken)" = "1" ] && break; sleep 1; done
  [ "$(metric broken)" = "1" ] || { echo "GABIM: zilja nuk e kuptoi që zinxhiri ndryshoi"; exit 1; }
  R=$(curl -s --noproxy '*' -X POST -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot"}' "$ZILJA")
  echo "   $R"
  echo "$R" | grep -q "zilja ndaloi" || { echo "GABIM: RPC-ja duhet të refuzojë"; exit 1; }
  [ "$(health)" = "503" ] || { echo "GABIM: /health duhet 503 pas ndalimit (F-12)"; exit 1; }
  [ "$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' "$ZILJA/metrics")" = "404" ] || { echo "GABIM: /metrics s'duhet në portin publik"; exit 1; }
  echo "   ✅ zilja ndaloi, nuk jep vula të gabuara, /health = 503; disku u shënua për rindërtim (F-13)"
  echo "== Hapi 8 (disk): rinisja tjetër e rindërton pemën (pa fshirje me dorë)"
  stop
  sleep 2
  start
  [ "$(metric resumed)" = "0" ] || { echo "GABIM: pema e shënuar s'duhej të vazhdonte"; exit 1; }
  wait_rooted_past "$(( $(solana -u "$RPC" slot --commitment finalized) + 1 ))"
  [ "$(metric broken)" = "0" ] || { echo "GABIM: zilja ndaloi pas rindërtimit"; exit 1; }
  [ "$(health)" = "200" ] || { echo "GABIM: /health duhet 200"; exit 1; }
  echo "   ✅ u rindërtua vetë dhe punon"
  fi
else
  echo "== Hapi 7 (RAM): ringarkimi i plugin-it me admin RPC (F-07, A02)"
  RAYON_BEFORE=$(rayon_threads)
  admin_rpc reloadPlugin zilja "$WORK/zilja.json"
  grep -q on_unload "$ZILJA_PROBE" || { echo "GABIM: on_unload nuk u thirr"; exit 1; }
  RAYON_AFTER=$(rayon_threads)
  echo "   A08: thread-e zilja-rayon para/pas ringarkimit: $RAYON_BEFORE / $RAYON_AFTER"
  [ "$RAYON_BEFORE" -gt 0 ] && [ "$RAYON_AFTER" = "$RAYON_BEFORE" ] || { echo "GABIM: thread-et Rayon të instancës së vjetër mbetën pas unload (A08)"; exit 1; }
  kill -0 "$VPID" || { echo "GABIM: validatori ra"; exit 1; }
  # A02: pas ringarkimit asgjë s'provon që një bankë u vëzhgua e plotë: zilja ndalon, s'aplikon gjysmake.
  for _ in $(seq 1 120); do [ "$(metric broken)" = "1" ] && break; sleep 1; done
  [ "$(metric broken)" = "1" ] && [ "$(health)" = "503" ] || { echo "GABIM: pas ringarkimit zilja duhej të ndalonte, jo të aplikonte banka të pjesshme (A02)"; exit 1; }
  echo "   ✅ ringarkim pa rrëzim; instanca e re ndaloi fail-closed (A02)"
  echo "== Hapi 7b (RAM): rinisja e validatorit jep bazën"
  stop
  sleep 2
  start
  wait_rooted_past "$(( $(solana -u "$RPC" slot --commitment finalized) + 3 ))"
  [ "$(metric broken)" = "0" ] && [ "$(health)" = "200" ] || { echo "GABIM: pas rinisjes"; exit 1; }
  echo "   ✅ pas rinisjes punon"
fi
echo "== Sonda: pyetjet e hapura të auditimit"
python3 "$ROOT/scripts/probe-report.py" "$ZILJA_PROBE" || true
echo
echo "== GJITHÇKA KALOI ✅ (roots.log: $(wc -l < "$WORK/roots.log") rreshta)"
