#!/usr/bin/env bash
# Seria e testeve mbi një snapshot të vërtetë, në PC-në tënde, me një komandë.
#
#   scripts/bench-matrix.sh check [dosja]                         # 1. a është PC-ja gati (rreth 1 min)
#   scripts/bench-matrix.sh <snapshot.tar.zst | URL> [dosja]     # 2. nis të gjitha rastet
#   scripts/bench-matrix.sh publish <dosja>                       # 3. kopjon rezultatet në bench-results/ për GitHub
#   Kodi i daljes: 0 = të gjitha rastet mbaruan; 4 = disa raste dështuan (të tjerat mbaruan; një nisje e re
#   provon vetëm ato); 1 = gabim që ndal gjithçka.
#
# Snapshot-i shkarkohet NJË herë (nëse jepet URL) dhe përdoret për çdo rast. Çdo rast
# ndërton pemën nga e para në një DB të re, pastaj mat slotet dhe dëftesat.
# Rastet që kanë mbaruar (kanë shënuesin ok, G15) kapërcehen: komanda mund të niset sërish
# pas një ndërprerjeje dhe vazhdon aty ku mbeti.
#
# Rastet (grupet). "Ngrohtë" = pages.dat në cache-in e sistemit (siç del vetë pas ndërtimit);
# "ftohtë" = bench-i e nxjerr nga cache-i (--cold), që leximet të shkojnë vërtet te NVMe.
#   size  — sa llogari, ngrohtë: një rast për çdo vlerë në LIMITS ("full" = snapshot-i i plotë)
#   cold  — leximet nga NVMe me COLD_LIMIT llogari: `profile` (cache bosh në fillim të çdo profili,
#           pastaj ngrohet si në prodhim) dhe `slot` (cache bosh para çdo sloti: rasti më i keq)
#   cache — cache_depth 16/20/24 (RAM ↔ lexime), ftohtë (profile): pa lexime nga disku s'ka dallim
#   seed  — qëndrueshmëria: e njëjta matje me farë tjetër (ngrohtë, CASE_LIMIT)
#
# Ndryshoret (me vlerat e parazgjedhura):
#   BENCH_GROUPS="size cold cache seed"
#   LIMITS="1000000 10000000 50000000 full"
#   CASE_LIMIT=10000000           (për seed)
#   COLD_LIMIT=full               (për cold dhe cache)
#   COLD_MODES="profile slot"
#   CACHE_DEPTHS="16 24"          (20 = parazgjedhja, matet te cold; vetëm shumëfisha të 4)
#   SEEDS="2 3"                   (1 = parazgjedhja, matet te size)
#   UPDATES=1000,5000,15000,30000,60000,100000
#   SLOTS=1024                    (≥ 4 checkpoint-e për profil: meta.ckpt shkruhet çdo 256 slote)
#   SAMPLE=500000                 (ngrohtë) / COLD_SAMPLE=5000000 (ftohtë: mostër e madhe ngrohet më pak)
#   PROOFS=20000
#   RPC_CLIENTS=1,8,32,128        (R9: dëftesa nga shumë klientë njëkohësisht mbi HTTP, në çdo rast;
#   RPC_SECONDS=30                 bosh = pa këtë fazë). Gjatë saj zilja zbaton një slot çdo
#   RPC_SLOT_MS=200                RPC_SLOT_MS me RPC_SLOT_UPDATES ndryshime, si në Mainnet, dhe
#   RPC_SLOT_UPDATES=5000          mat edhe një klient të madh nga NJË IP (kufiri i prodhimit:
#   RPC_ONE_IP=1                   2 lidhje njëkohësisht). 1 në 100 përgjigje verifikohet plotësisht.
#   RPC_ROOT_EVERY=4               Pema përditësohet çdo K slote (grupimi, SPEC §3); 1 = çdo slot.
#   BATCH=                        grupimi i sloteve, K:U (U = llogari unike për grup), p.sh. sipas
#                                 matjes së Mainnet-it: BATCH=1:1502,2:2340,4:3827,8:6134 (bosh = pa këtë fazë)
#   BIN=target/release/zilja-snapshot-bench (ndërtohet nëse mungon)
#
# Çdo rast ndërton pemën nga e para. Me Mainnet të plotë kjo është disa orë për rast: zgjidh grupet,
# p.sh. vetëm Mainnet i plotë, ngrohtë dhe ftohtë:
#   BENCH_GROUPS="size cold" LIMITS=full scripts/bench-matrix.sh https://api.mainnet-beta.solana.com/snapshot.tar.bz2 ~/zilja-mainnet
#
# Në fund: <dosja>/RESULTS.md (tabelat e krahasimit) dhe grafikët e çdo rasti.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/snapshot-url.sh
. "$ROOT/scripts/snapshot-url.sh"
BIN="${BIN:-$ROOT/target/release/zilja-snapshot-bench}"
usage() { sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }

# ---- publish: kopjon vetëm skedarët e vegjël (pa results.csv/DB) në repo ----
if [ "${1:-}" = "publish" ]; then
  SRC="${2:-}"; [ -d "$SRC" ] && [ -f "$SRC/RESULTS.md" ] || { echo "s'ka $SRC/RESULTS.md: nis fillimisht testet" >&2; exit 1; }
  NET=${NETWORK:-$(cat "$SRC/network" 2>/dev/null || echo snapshot)}
  DEST="$ROOT/bench-results/$(date +%F)-$(hostname -s)-$NET"
  mkdir -p "$DEST"
  cp "$SRC/RESULTS.md" "$SRC/machine.txt" "$DEST/" 2>/dev/null
  for d in "$SRC"/*/; do
    n=$(basename "$d"); [ -f "$d/summary.json" ] || continue
    mkdir -p "$DEST/$n"
    cp "$d/summary.json" "$DEST/$n/"
    [ -f "$d/batch.csv" ] && cp "$d/batch.csv" "$DEST/$n/"
    cp "$d"/*.png "$DEST/$n/" 2>/dev/null || true
    gzip -c "$d/timeline.csv" > "$DEST/$n/timeline.csv.gz" 2>/dev/null || true
  done
  du -sh "$DEST"
  cat <<EOF
gati: $DEST
Për ta çuar në GitHub:
  cd $ROOT && git checkout -b bench/$(basename "$DEST") && git add bench-results && \\
  git commit -m "Rezultatet e bench-it: $(basename "$DEST")" && git push -u origin HEAD
dhe hap PR-në në GitHub.
EOF
  exit 0
fi

# ---- check: a është PC-ja gati për matje të sakta ----
if [ "${1:-}" = "check" ]; then
  D="${2:-$HOME/zilja-bench-$(date +%F)}"; mkdir -p "$D"; D="$(cd "$D" && pwd)"
  ERR=0; WARN=0
  ok()   { echo "  OK      $*"; }
  warn() { echo "  KUJDES  $*"; WARN=$((WARN+1)); }
  bad()  { echo "  GABIM   $*"; ERR=$((ERR+1)); }
  echo "Kontrolli i PC-së për bench-in ($D):"
  [ "$(uname -s)" = Linux ] && ok "Linux $(uname -r)" || bad "duhet Linux (ose WSL2 në Windows)"
  WSL=0; grep -qi microsoft /proc/version 2>/dev/null && WSL=1
  if [ $WSL = 1 ]; then
    ok "WSL2"
    case "$D" in /mnt/[a-z]/*|/mnt/[a-z]) bad "$D është në diskun e Windows-it (/mnt/c…): shumë i ngadaltë dhe matjet s'vlejnë; përdor një dosje brenda ~/" ;; esac
    warn "WSL2: Windows mund të mbajë cache edhe sipër diskut virtual; shiko 'cache %' te rastet cold (duhet të jetë i ulët)"
  fi
  fs=$(df -T "$D" 2>/dev/null | awk 'NR==2{print $2}')
  case "$fs" in ext4|xfs|btrfs) ok "sistemi i skedarëve: $fs" ;; 9p|drvfs|v9fs|fuseblk|ntfs*|vfat|exfat) bad "sistemi i skedarëve $fs: s'vlen për matje" ;; *) warn "sistemi i skedarëve: ${fs:-i panjohur}" ;; esac
  avail=$(df -BG --output=avail "$D" | tail -1 | tr -dc 0-9)
  echo "  INFO    disk i lirë: ${avail} GB (Testnet: shiko madhësinë e snapshot-it ×2 + DB; Mainnet i plotë: ≥ 700 GB – 1 TB)"
  [ "${avail:-0}" -ge 100 ] || warn "më pak se 100 GB të lirë: mjafton vetëm për raste me --limit"
  dev=$(df --output=source "$D" | tail -1); blk=$(lsblk -no PKNAME "$dev" 2>/dev/null | head -1); blk=${blk:-$(basename "$dev")}
  rota=$(cat "/sys/block/$blk/queue/rotational" 2>/dev/null)
  model=$(cat "/sys/block/$blk/device/model" 2>/dev/null | xargs)
  [ "$rota" = 1 ] && warn "disku $blk raportohet si rrotullues (HDD). Nëse është vërtet HDD, numrat s'vlejnë për NVMe; në makina virtuale ky raport shpesh s'është i saktë" || ok "disku: $blk ${model:-} $( [ $WSL = 1 ] && echo '(në WSL2 modeli i vërtetë s’duket: shiko në Windows)')"
  mt=$(awk '/MemTotal/{printf "%.0f",$2/1048576}' /proc/meminfo); ma=$(awk '/MemAvailable/{printf "%.0f",$2/1048576}' /proc/meminfo)
  ok "RAM: ${mt} GB gjithsej, ${ma} GB i lirë$( [ $WSL = 1 ] && echo ' (WSL2 merr si parazgjedhje 50% të RAM-it të PC-së; ndryshohet te %UserProfile%\.wslconfig → memory=)')"
  sw=$(awk '/SwapTotal/{printf "%.0f",$2/1048576}' /proc/meminfo); [ "${sw:-0}" -gt 0 ] && warn "swap ${sw} GB: nëse përdoret gjatë matjes, numrat prishen (RAM maksimal te tabela tregon)" || ok "pa swap"
  la=$(cut -d' ' -f1 /proc/loadavg); n=$(nproc)
  ok "CPU: $n fije, $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)"
  awk -v l="$la" 'BEGIN{exit !(l>1.0)}' && warn "ngarkesa tani $la: mbyll programet e tjera (shfletues, lojëra, antivirus, Windows Update) gjatë matjes" || ok "ngarkesa tani: $la"
  pr=$(cat /proc/sys/net/ipv4/ip_local_port_range 2>/dev/null | tr '\t' ' ')
  [ -n "$pr" ] && echo "  INFO    porta të përkohshme për klientët e testit RPC: $pr (nëse tabela tregon 'porta' > 0, klientët e makinës mbaruan portat: sudo sysctl -w net.ipv4.ip_local_port_range=\"1024 65535\")"
  gov=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null)
  [ -n "$gov" ] && { [ "$gov" = performance ] && ok "CPU governor: performance" || warn "CPU governor: $gov (për numra të qëndrueshëm: performance; në laptop: në prizë, modaliteti 'Best performance')"; }
  for t in cargo curl python3; do command -v $t >/dev/null && ok "$t" || bad "mungon $t"; done
  python3 -c 'import matplotlib' 2>/dev/null && ok "matplotlib (grafikët)" || warn "mungon matplotlib: pip install matplotlib (pa të s'ka grafikë, tabela del)"
  # Cache-i ftohtë: shkruan 256 MB, i nxjerr nga cache-i dhe i lexon sërish; pajisja duhet të lexojë ~256 MB.
  python3 - "$D" <<'PY' && ok "--cold punon: leximi shkon vërtet në disk" || { echo "  GABIM   --cold s'ka efekt këtu: leximet vijnë nga cache-i; rastet cold s'do të jenë të sakta"; ERR=$((ERR+1)); }
import os, sys
p = os.path.join(sys.argv[1], ".cold-check"); n = 256 << 20
with open(p, "wb") as f:
    for _ in range(n >> 20): f.write(os.urandom(1 << 20))
    f.flush(); os.fsync(f.fileno())
def rb(): return int([l for l in open("/proc/self/io") if l.startswith("read_bytes")][0].split()[1])
fd = os.open(p, os.O_RDONLY)
os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
r0 = rb()
while os.read(fd, 1 << 20): pass
got = rb() - r0
os.close(fd); os.remove(p)
print(f"  INFO    pas heqjes nga cache-i u lexuan {got / 1e6:.0f} MB nga pajisja (prisnim ~{n / 1e6:.0f})")
sys.exit(0 if got > n * 0.8 else 1)
PY
  if [ -x "$BIN" ] || (cd "$ROOT" && cargo build --release -q -p zilja-bench --bin zilja-snapshot-bench); then
    ok "zilja-snapshot-bench u ndërtua"
    (cd "$ROOT" && cargo test --release -q -p zilja-bench --bin zilja-snapshot-bench 2>&1 | tail -2 | grep -q 'test result: ok') \
      && ok "testet e bench-it (snapshot sintetik: root = pema në RAM)" || bad "testet e bench-it dështuan"
  else bad "ndërtimi dështoi"; fi
  echo; echo "Gjithsej: $ERR gabime, $WARN kujdes."
  [ $ERR -eq 0 ] && echo "Gati. Hapi tjetër: scripts/bench-matrix.sh <URL ose snapshot> $D"
  exit $((ERR > 0))
fi

SNAP="${1:-}"; [ -n "$SNAP" ] || usage
OUT="${2:-$HOME/zilja-bench-$(date +%F)}"
BENCH_GROUPS="${BENCH_GROUPS:-size cold cache seed}"
LIMITS="${LIMITS:-1000000 10000000 50000000 full}"
CASE_LIMIT="${CASE_LIMIT:-10000000}"
COLD_LIMIT="${COLD_LIMIT:-full}"
COLD_MODES="${COLD_MODES:-profile slot}"
CACHE_DEPTHS="${CACHE_DEPTHS:-16 24}"
SEEDS="${SEEDS:-2 3}"
UPDATES="${UPDATES:-1000,5000,15000,30000,60000,100000}"
SLOTS="${SLOTS:-1024}"
SAMPLE="${SAMPLE:-500000}"
COLD_SAMPLE="${COLD_SAMPLE:-5000000}"
PROOFS="${PROOFS:-20000}"
RPC_CLIENTS="${RPC_CLIENTS-1,8,32,128}"
RPC_SECONDS="${RPC_SECONDS:-30}"
RPC_SLOT_MS="${RPC_SLOT_MS:-200}"
RPC_SLOT_UPDATES="${RPC_SLOT_UPDATES:-5000}"
RPC_ONE_IP="${RPC_ONE_IP:-1}"
RPC_ROOT_EVERY="${RPC_ROOT_EVERY:-4}"
BATCH="${BATCH:-}"
BATCH_ARGS=()
[ -n "$BATCH" ] && BATCH_ARGS=(--batch "$BATCH")
RPC_ARGS=()
if [ -n "$RPC_CLIENTS" ]; then
  RPC_ARGS=(--rpc-clients "$RPC_CLIENTS" --rpc-seconds "$RPC_SECONDS" --rpc-slot-ms "$RPC_SLOT_MS" --rpc-slot-updates "$RPC_SLOT_UPDATES" --rpc-root-every "$RPC_ROOT_EVERY")
  [ "$RPC_ONE_IP" = 1 ] && RPC_ARGS+=(--rpc-one-ip)
fi
# Listat pranojnë edhe presje (p.sh. LIMITS=1000000,full), që të kalojnë lehtë nëpër `env`.
BENCH_GROUPS="${BENCH_GROUPS//,/ }"; LIMITS="${LIMITS//,/ }"; COLD_MODES="${COLD_MODES//,/ }"
CACHE_DEPTHS="${CACHE_DEPTHS//,/ }"; SEEDS="${SEEDS//,/ }"
for d in $CACHE_DEPTHS; do [ $((d % 4)) -eq 0 ] || { echo "CACHE_DEPTHS: $d s'është shumëfish i 4" >&2; exit 1; }; done
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
LOG="$OUT/matrix.log"
# G04: dy nisje në të njëjtën dosje do të shkruanin të njëjtat raste njëkohësisht.
exec 8>"$OUT/.matrix.lock"
flock -n 8 || { echo "Një tjetër bench-matrix po punon me $OUT." >&2; exit 1; }
say() { echo "[$(date +%T)] $*" | tee -a "$LOG"; }

if [ ! -x "$BIN" ]; then
  say "po ndërtoj zilja-snapshot-bench (release)…"
  (cd "$ROOT" && cargo build --release -p zilja-bench --bin zilja-snapshot-bench) || exit 1
fi

# ---- snapshot-i: një herë ----
case "$SNAP" in
  http://*|https://*)
    have=$(ls "$OUT"/snapshot-*.tar.zst 2>/dev/null | head -1)
    if [ -n "$have" ]; then
      say "snapshot-i ekziston: $have"
      SNAP="$have"
    else
      # H1: URL-ja përfundimtare zgjidhet para shkarkimit dhe ruhet te snapshot.url. Vazhdimi
      # (`-C -`) bëhet vetëm mbi të njëjtën URL; nëse serveri ka nxjerrë snapshot tjetër, pjesa e
      # vjetër fshihet, që të mos ngjiten dy snapshot-e në një skedar.
      part="$OUT/snapshot.part"; url_file="$OUT/snapshot.url"; got=0
      for attempt in 1 2 3; do
        eff=$(snapshot_resolve_retry "$SNAP"); rc=$?
        if [ $rc -ne 0 ]; then
          say "URL-ja e snapshot-it s'u zgjidh: $eff$([ $rc -eq 2 ] && echo ' (HTTP 429: shumë kërkesa; provo më vonë ose një URL tjetër)')"
          exit 1
        fi
        if [ -f "$part" ] && [ "$(cat "$url_file" 2>/dev/null)" != "$eff" ]; then
          say "snapshot-i në server ndryshoi ($(cat "$url_file" 2>/dev/null || echo '?') → $eff): pjesa e vjetër fshihet, nis nga e para"
          rm -f "$part"
        fi
        echo "$eff" > "$url_file"
        say "po shkarkoj $eff (një herë)…"
        curl -fL --retry 5 --retry-delay 30 -C - -o "$part" "$eff"; st=$?
        [ $st -eq 0 ] && { got=1; break; }
        # 33: serveri s'pranon vazhdim (Range); pjesa s'vlen më, prova tjetër nis nga e para.
        [ $st -eq 33 ] && rm -f "$part"
        say "shkarkimi u ndërpre (kodi $st, prova $attempt/3); e vazhdoj nëse URL-ja s'ka ndryshuar"
      done
      [ $got = 1 ] || { say "shkarkimi dështoi"; exit 1; }
      name=$(basename "${eff%%\?*}")
      case "$name" in snapshot-*.tar.zst) ;; *) name="snapshot.tar.zst" ;; esac
      mv "$part" "$OUT/$name"
      rm -f "$url_file"
      SNAP="$OUT/$name"
      say "u shkarkua: $SNAP ($(du -h "$SNAP" | cut -f1))"
    fi ;;
  *) [ -f "$SNAP" ] || { echo "s'gjendet: $SNAP" >&2; exit 1; } ;;
esac
case "$SNAP" in *mainnet*) echo mainnet > "$OUT/network" ;; *testnet*) echo testnet > "$OUT/network" ;; esac
[ -f "$OUT/network" ] || { [ -n "${NETWORK:-}" ] && echo "$NETWORK" > "$OUT/network"; }

# ---- makina: që rezultatet të krahasohen drejt ----
# Matrix-i mund të niset disa herë në të njëjtën dosje me parametra të ndryshëm (rastet e mbaruara
# kapërcehen, snapshot-i ripërdoret): çdo nisje shtohet te runs.log; parametrat e çdo rasti janë te
# <rasti>/summary.json.
echo "$(date -Is) BENCH_GROUPS='$BENCH_GROUPS' LIMITS='$LIMITS' CASE_LIMIT=$CASE_LIMIT COLD_LIMIT=$COLD_LIMIT COLD_MODES='$COLD_MODES' CACHE_DEPTHS='$CACHE_DEPTHS' SEEDS='$SEEDS' UPDATES=$UPDATES SLOTS=$SLOTS SAMPLE=$SAMPLE COLD_SAMPLE=$COLD_SAMPLE PROOFS=$PROOFS RPC_CLIENTS=$RPC_CLIENTS RPC_SECONDS=$RPC_SECONDS RPC_SLOT_MS=$RPC_SLOT_MS RPC_SLOT_UPDATES=$RPC_SLOT_UPDATES RPC_ONE_IP=$RPC_ONE_IP RPC_ROOT_EVERY=$RPC_ROOT_EVERY BATCH=$BATCH" >> "$OUT/runs.log"
{
  echo "data: $(date -Is)"
  echo "git: $(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null) $(cd "$ROOT" && git status --porcelain 2>/dev/null | grep -q . && echo '(me ndryshime lokale)')"
  echo "snapshot: $(basename "$SNAP") ($(du -h "$SNAP" | cut -f1))"
  echo "nisjet e matrix-it (runs.log):"; sed 's/^/  /' "$OUT/runs.log"
  echo; uname -a
  echo; lscpu 2>/dev/null | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|MHz|L3' || true
  echo; free -g 2>/dev/null || true
  echo; df -h "$OUT" 2>/dev/null || true
  echo; lsblk -d -o NAME,MODEL,ROTA,SIZE,TRAN 2>/dev/null || true
  grep -qi microsoft /proc/version 2>/dev/null && echo "WSL2: po"
} > "$OUT/machine.txt"

# ---- lista e rasteve: emri|argumentet ----
lim() { [ "$1" = full ] && echo "" || echo "--limit $1"; }
CASES=()
for g in $BENCH_GROUPS; do
  case "$g" in
    size)  for n in $LIMITS; do CASES+=("size-$n|$(lim "$n") --cache-depth 20 --seed 1 --sample $SAMPLE --cold none"); done ;;
    cold)  for m in $COLD_MODES; do CASES+=("cold-$m-$COLD_LIMIT|$(lim "$COLD_LIMIT") --cache-depth 20 --seed 1 --sample $COLD_SAMPLE --cold $m"); done ;;
    cache) for d in $CACHE_DEPTHS; do CASES+=("cache$d-cold-$COLD_LIMIT|$(lim "$COLD_LIMIT") --cache-depth $d --seed 1 --sample $COLD_SAMPLE --cold profile"); done ;;
    seed)  for s in $SEEDS; do CASES+=("seed$s-$CASE_LIMIT|$(lim "$CASE_LIMIT") --cache-depth 20 --seed $s --sample $SAMPLE --cold none"); done ;;
    *) echo "grup i panjohur: $g (size, cold, cache, seed)" >&2; exit 1 ;;
  esac
done
say "${#CASES[@]} raste në $OUT"

FAILED=()
for c in "${CASES[@]}"; do
  name="${c%%|*}"; args="${c#*|}"
  dir="$OUT/$name"
  # G15: i mbaruar = shënuesi `ok` (shkruhet vetëm kur bench-i del me 0), ose një summary.json i vjetër
  # pa rpc_error (nisjet para shënuesit). Një summary.json me rpc_error është rast i dështuar: riniset.
  if [ -f "$dir/ok" ] || { [ -f "$dir/summary.json" ] && ! grep -q '"rpc_error"' "$dir/summary.json"; }; then
    say "$name: ka mbaruar më parë, e kapërcej"; continue
  fi
  mkdir -p "$dir"
  rm -f "$dir/summary.json"
  # G04: një dosje e re për çdo rast (mktemp), që kjo nisje e krijoi vetë; fshihet vetëm ajo.
  # Kurrë një rrugë fikse si "$OUT/db", që mund të jetë e dikujt tjetër.
  db=$(mktemp -d "$OUT/.db-$name.XXXXXX") || { say "$name: s'u krijua dosja e DB-së"; exit 1; }
  say "$name: fillon ($args)  [disk i lirë: $(df -h --output=avail "$OUT" | tail -1 | tr -d ' ')]"
  t0=$(date +%s)
  # shellcheck disable=SC2086
  "$BIN" "$SNAP" "$db" --out "$dir/results.csv" $args \
      --updates "$UPDATES" --slots "$SLOTS" --proofs "$PROOFS" "${BATCH_ARGS[@]}" "${RPC_ARGS[@]}" \
      > "$dir/stdout.json" 2> "$dir/bench.log"
  st=$?
  du -sh "$db" 2>/dev/null | cut -f1 > "$dir/db_size" || true
  rm -rf "$db"
  if [ $st -ne 0 ]; then
    say "$name: DËSHTOI (kodi $st): $(tail -1 "$dir/bench.log")"
    FAILED+=("$name"); continue
  fi
  touch "$dir/ok"
  say "$name: gati për $(( ($(date +%s) - t0) / 60 )) min — $(grep '^pema:' "$dir/bench.log" | cut -c1-120)"
  grep '^KUJDES' "$dir/bench.log" | while read -r w; do say "$name: $w"; done
  python3 "$ROOT/scripts/plot-bench.py" "$dir" >> "$dir/bench.log" 2>&1 || say "$name: grafikët s'u bënë (pip install matplotlib)"
done

python3 "$ROOT/scripts/bench-table.py" "$OUT" > "$OUT/RESULTS.md" && say "tabela: $OUT/RESULTS.md"
if [ ${#FAILED[@]} -gt 0 ]; then
  say "DËSHTUAN: ${FAILED[*]} (shiko <rasti>/bench.log)"
  # M2: kodi 4 = disa raste dështuan, të tjerat mbaruan (≠ 1, gabim që ndal gjithçka). Një nisje
  # e re në të njëjtën dosje provon vetëm rastet e dështuara.
  exit 4
fi
say "të gjitha rastet mbaruan. Për GitHub: scripts/bench-matrix.sh publish $OUT"
