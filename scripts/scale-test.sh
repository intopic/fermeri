#!/usr/bin/env bash
# Testi i shkallës së Ziles: 100 milionë → 500 milionë → 1 miliard llogari, në makinën tënde.
#
#   bash scale-test.sh
#
# Çfarë bën:
#   1. kontrollon makinën (Linux/WSL2, RAM, disk i lirë) dhe instalon atë që mungon;
#   2. shkarkon dhe ndërton Ziles (vetëm programin e matjes);
#   3. për çdo madhësi: ndërton pemën nga një "snapshot" me llogari sintetike, zbaton slote me
#      ndryshime si në mainnet, mat kohën, diskun, RAM-in dhe dëftesat;
#   4. shkruan raportin në ~/zilja-scale-results/ dhe FSHIN të dhënat e testit.
#
# Asgjë nuk lidhet me Solana-n ose me internetin gjatë matjes. Mund ta ndalosh kur të duash me
# Ctrl+C: të dhënat e testit fshihen edhe atëherë. Nise kur s'të duhet PC-ja (p.sh. natën):
# CPU-ja dhe disku punojnë në maksimum.
#
# Opsione (variabla mjedisi):
#   DATA_DIR=/rruga/në/nvme   ku shkruhen të dhënat e testit (parazgjedhja ~/zilja-scale-data)
#   SIZES="100000000 500000000 1000000000"   madhësitë
#   SLOTS=300                 slote për çdo madhësi
#   PER_SLOT=10000            ndryshime për slot (mainnet: dhjetëra mijëra llogari/slot)
#   COMPARE=1                 mat edhe RocksDB te madhësia e parë (0 = vetëm faqet)
#   KEEP_TOOLS=0              1 = mos e fshi kodin dhe Rust-in në fund
set -euo pipefail

DATA_DIR=${DATA_DIR:-$HOME/zilja-scale-data}
SIZES=${SIZES:-"100000000 500000000 1000000000"}
SLOTS=${SLOTS:-300}
PER_SLOT=${PER_SLOT:-10000}
COMPARE=${COMPARE:-1}
KEEP_TOOLS=${KEEP_TOOLS:-0}
SRC_DIR=${SRC_DIR:-$HOME/zilja-scale-src}
OUT_DIR=${OUT_DIR:-$HOME/zilja-scale-results}
REPO=${REPO:-https://github.com/intopic/fermeri.git}
# Bajte disku për llogari gjatë testit (pema + staging + rezervë), për kontrollin paraprak.
BYTES_PER_ACCOUNT=520

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
die() { printf '\n\033[31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

cleanup() {
  rm -rf "$DATA_DIR/run" 2>/dev/null || true
}
trap 'cleanup; echo; echo "U ndal. Të dhënat e testit u fshinë; raportet e mbaruara janë në $OUT_DIR."' INT TERM
trap cleanup EXIT

# ------------------------------------------------------------------ 1. makina
say "1/4 Kontrolli i makinës"
[ "$(uname -s)" = "Linux" ] || die "Duhet Linux ose WSL2 (Windows: hap 'Ubuntu' nga WSL dhe nise aty)."
if grep -qi microsoft /proc/version 2>/dev/null; then
  echo "WSL2 u gjet."
  case "$DATA_DIR" in /mnt/*) die "Mos përdor /mnt/c (diski i Windows-it përmes WSL është shumë i ngadaltë). Lëre DATA_DIR në ~ ose në një disk Linux.";; esac
fi
CORES=$(nproc)
RAM_GB=$(awk '/MemTotal/ {printf "%d", $2/1024/1024}' /proc/meminfo)
CPU=$(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')
mkdir -p "$DATA_DIR" "$OUT_DIR"
FREE_GB=$(df -BG --output=avail "$DATA_DIR" | tail -1 | tr -dc 0-9)
DISK_DEV=$(df --output=source "$DATA_DIR" | tail -1)
echo "Procesori: $CPU ($CORES bërthama)"
echo "RAM:       $RAM_GB GB"
echo "Disku:     $FREE_GB GB të lirë në $DATA_DIR ($DISK_DEV)"
[ "$RAM_GB" -ge "${MIN_RAM_GB:-14}" ] || die "Duhen të paktën 16 GB RAM (ke $RAM_GB GB)."

# Madhësitë që futen në disk; të tjerat kapërcehen me shënim.
RUN_SIZES=""
for n in $SIZES; do
  need=$(( n * BYTES_PER_ACCOUNT / 1000000000 + 5 ))
  if [ "$need" -le "$FREE_GB" ]; then RUN_SIZES="$RUN_SIZES $n"; else echo "⚠ $n llogari kërkojnë ~$need GB; ke $FREE_GB GB: kapërcehet."; fi
done
[ -n "$RUN_SIZES" ] || die "Asnjë madhësi s'futet në disk. Liro hapësirë ose vendos DATA_DIR në një disk më të madh."
echo "Do të matet:$RUN_SIZES llogari (RocksDB për krahasim: $([ "$COMPARE" = 1 ] && echo po || echo jo))"

# ------------------------------------------------------------------ 2. mjetet
say "2/4 Mjetet dhe ndërtimi"
need_pkgs=""
for c in gcc clang pkg-config git curl python3; do command -v "$c" >/dev/null || need_pkgs="$need_pkgs $c"; done
if [ -n "$need_pkgs" ] || ! ls /usr/lib/llvm-*/lib/libclang*.so* >/dev/null 2>&1; then
  echo "Po instalohen: build-essential clang libclang-dev pkg-config git curl python3 (kërkon fjalëkalimin e sudo)"
  sudo apt-get update -q
  sudo apt-get install -y -q build-essential clang libclang-dev pkg-config git curl python3
fi
if ! command -v cargo >/dev/null; then
  echo "Po instalohet Rust (rustup)…"
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env" 2>/dev/null || true
if [ -d "$SRC_DIR/.git" ]; then
  git -C "$SRC_DIR" pull -q
else
  git clone -q "$REPO" "$SRC_DIR"
fi
(cd "$SRC_DIR" && cargo build -q --release -p zilja-bench --bin zilja-disk-bench)
BENCH="$SRC_DIR/target/release/zilja-disk-bench"
COMMIT=$(git -C "$SRC_DIR" rev-parse --short HEAD)
echo "Gati (kodi: $COMMIT)."

# ------------------------------------------------------------------ 3. matjet
STAMP=$(date +%Y%m%d-%H%M)
RES="$OUT_DIR/$STAMP"
mkdir -p "$RES"
{
  echo "{"
  echo "  \"cpu\": \"$CPU\", \"cores\": $CORES, \"ram_gb\": $RAM_GB,"
  echo "  \"disk\": \"$DISK_DEV\", \"disk_model\": \"$(lsblk -no MODEL "$(df --output=source "$DATA_DIR" | tail -1)" 2>/dev/null | head -1 | xargs)\","
  echo "  \"commit\": \"$COMMIT\", \"slots\": $SLOTS, \"per_slot\": $PER_SLOT, \"wsl\": $(grep -qi microsoft /proc/version && echo true || echo false)"
  echo "}"
} > "$RES/machine.json"

run_one() { # motori, llogari
  local eng=$1 n=$2
  local out="$RES/$eng-$n.json"
  say "3/4 Matja: $eng, $n llogari ($SLOTS slote × $PER_SLOT ndryshime)"
  local t0=$SECONDS
  rm -rf "$DATA_DIR/run"
  if ZILJA_ENGINE=$eng ZILJA_SYNC_EVERY=1 "$BENCH" "$n" "$SLOTS" "$PER_SLOT" "$DATA_DIR/run" > "$out.tmp" 2> "$RES/$eng-$n.log"; then
    mv "$out.tmp" "$out"
    python3 - "$out" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
print(f"  slot p50/p99: {r['apply_ms_p50']:.0f} / {r['apply_ms_p99']:.0f} ms   shkrime: {r['device_write_mb_per_slot']:.1f} MB/slot")
print(f"  dëftesa: {r['prove_ms_p50']*1000:.1f} µs, ~{r['proof_bytes_approx']:.0f} B   disk: {r['disk_bytes_per_account']:.0f} B/llogari   RAM: {r['process_rss_mb']:.0f} MB")
print(f"  ndërtimi nga snapshot-i: {r['snapshot_stage_seconds'] + r['snapshot_build_seconds']:.0f} s")
PY
  else
    echo "  ✗ dështoi; shih $RES/$eng-$n.log"
    rm -f "$out.tmp"
  fi
  rm -rf "$DATA_DIR/run"
  echo "  ($(( (SECONDS - t0) / 60 )) min)"
}

first=1
for n in $RUN_SIZES; do
  run_one pages "$n"
  if [ "$first" = 1 ] && [ "$COMPARE" = 1 ]; then run_one rocksdb "$n"; fi
  first=0
done

# ------------------------------------------------------------------ 4. raporti
say "4/4 Raporti"
python3 - "$RES" <<'PY'
import json, glob, os, sys
res = sys.argv[1]
m = json.load(open(os.path.join(res, "machine.json")))
rows = []
for f in sorted(glob.glob(os.path.join(res, "*-*.json")), key=lambda p: (int(p.rsplit("-", 1)[1][:-5]), p)):
    if f.endswith("machine.json"):
        continue
    rows.append(json.load(open(f)))
lines = [
    "# Zilja: testi i shkallës",
    "",
    f"Makina: {m['cpu']}, {m['cores']} bërthama, {m['ram_gb']} GB RAM, disk {m['disk_model'] or m['disk']}{' (WSL2)' if m['wsl'] else ''}.",
    f"Kodi: {m['commit']}. Ngarkesa: {m['slots']} slote × {m['per_slot']} ndryshime (90% llogari ekzistuese, 8% të reja, 2% fshirje), fsync në çdo slot.",
    "",
    "| Motori | Llogari | Slot p50 | Slot p99 | Shkrime/slot | Dëftesa p50 | Dëftesa | Disk/llogari | RAM | Ndërtimi |",
    "|---|---|---|---|---|---|---|---|---|---|",
]
for r in rows:
    lines.append(
        f"| {r['engine']} | {r['accounts_final']/1e6:,.{0 if r['accounts_final'] >= 1e7 else 1}f} M | {r['apply_ms_p50']:.0f} ms | {r['apply_ms_p99']:.0f} ms | "
        f"{r['device_write_mb_per_slot']:.1f} MB | {r['prove_ms_p50']*1000:.1f} µs | ~{r['proof_bytes_approx']:.0f} B | "
        f"{r['disk_bytes_per_account']:.0f} B | {r['process_rss_mb']:.0f} MB | {(r['snapshot_stage_seconds']+r['snapshot_build_seconds'])/60:.1f} min |"
    )
lines += ["", "Të gjitha dëftesat e kontrolluara u verifikuan: " + ", ".join(r["proofs_verified"] for r in rows) + "."]
txt = "\n".join(lines) + "\n"
open(os.path.join(res, "RAPORTI.md"), "w").write(txt)
print(txt)
PY

if [ "$KEEP_TOOLS" != 1 ]; then
  rm -rf "$SRC_DIR"
  echo "Kodi i Ziles u fshi ($SRC_DIR). Rust-i mbetet; për ta hequr: rustup self uninstall"
fi
rmdir "$DATA_DIR" 2>/dev/null || true
echo
echo "✅ Mbaroi. Dërgoje këtë dosje: $RES  (RAPORTI.md + skedarët .json)"
