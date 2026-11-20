#!/usr/bin/env bash
# Të gjitha testet e PC-së me një komandë, sipas planit të vendosur (docs/PC-TESTI.md):
#
#   0 check      a jep PC-ja matje të sakta (ndalet nëse ka GABIM)
#   1 tests      cargo test --release --workspace
#   2 e2e        plugin-i brenda Agave-s (solana-test-validator), me RAM dhe me disk
#   3 testnet    matrix-i i plotë, 10 raste (size, cold, cache, seed) + klientët RPC
#   4 mainnet    i shkurtuar: size-full (1 024 slote), cold slot dhe cache 24 (512 slote),
#                pa profilin 100 000
#   5 publish    kopjon rezultatet te bench-results/ (për PR)
#
#   scripts/pc-run-all.sh                      # nis ose vazhdon nga faza ku mbeti
#   PHASES="3 4" scripts/pc-run-all.sh         # vetëm disa faza
#
# Gjendja: <rezultatet>/STATUS.md (faza, koha, OK/DËSHTOI), log-u i çdo faze te <rezultatet>/phaseN-*.log.
# Fazat e mbaruara kapërcehen (skedari phaseN.done); brenda matrix-it, edhe rastet e mbaruara.
# Rezultatet: si parazgjedhje <dosja mbi repo-n>/results (p.sh. ~/zilja/results).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TOOLS="${TOOLS:-$(dirname "$ROOT")/tools}"
R="${RESULTS:-$(dirname "$ROOT")/results}"
PHASES="${PHASES:-0 1 2 3 4 5}"
TESTNET_URL="${TESTNET_URL:-https://api.testnet.solana.com/snapshot.tar.bz2}"
MAINNET_URL="${MAINNET_URL:-https://api.mainnet-beta.solana.com/snapshot.tar.bz2}"
M_UPDATES="${M_UPDATES:-1000,5000,15000,30000,60000}"
MAINNET_MIN_FREE_GB="${MAINNET_MIN_FREE_GB:-700}"
# Vetëm për provën e skriptit (p.sh. snapshot i vogël): parametra shtesë për matrix-in.
T_EXTRA="${T_EXTRA:-}"
M_EXTRA="${M_EXTRA:-}"
mkdir -p "$R"
R="$(cd "$R" && pwd)"
MX="$ROOT/scripts/bench-matrix.sh"

# shellcheck disable=SC1091
[ -f "$TOOLS/env.sh" ] && . "$TOOLS/env.sh"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

exec 9>"$R/.lock"
flock -n 9 || { echo "Një tjetër pc-run-all po punon me $R (shiko STATUS.md)."; exit 1; }

NAMES=([0]=check [1]=tests [2]=e2e [3]=testnet [4]=mainnet [5]=publish)
status() { # faza gjendja shënimi
  local f="$R/status.tsv" n st t note
  touch "$f"
  grep -v "^$1	" "$f" | grep -E '^[0-5]	' > "$f.tmp" || true
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$(date '+%F %T')" "${3:-}" >> "$f.tmp"
  sort -n "$f.tmp" > "$f" && rm -f "$f.tmp"
  {
    echo "# Testet e PC-së — gjendja"
    echo
    echo "Përditësuar: $(date '+%F %T') · makina: $(hostname) · git: $(cd "$ROOT" && git rev-parse --short HEAD 2>/dev/null)"
    echo
    echo "| faza | gjendja | ora | shënim |"
    echo "|---|---|---|---|"
    while IFS=$'\t' read -r n st t note; do
      [[ $n =~ ^[0-5]$ ]] || continue
      echo "| $n ${NAMES[$n]} | $st | $t | $note |"
    done < "$f"
    echo
    echo "Log-et: \`phaseN-*.log\`. Rezultatet: \`testnet/RESULTS.md\`, \`mainnet/RESULTS.md\`."
  } > "$R/STATUS.md"
}
say() { echo "[$(date +%T)] $*" | tee -a "$R/run.log"; }

run_phase() { # n komanda...
  local n=$1; shift
  local log="$R/phase$n-${NAMES[$n]}.log"
  if [ -f "$R/phase$n.done" ]; then say "faza $n (${NAMES[$n]}): mbaruar më parë, e kapërcej"; return 0; fi
  say "faza $n (${NAMES[$n]}): fillon — log: $log"
  status "$n" "PO PUNON" ""
  local t0; t0=$(date +%s)
  if "$@" >> "$log" 2>&1; then
    local m=$(( ($(date +%s) - t0) / 60 ))
    touch "$R/phase$n.done"
    status "$n" "OK" "${m} min"
    say "faza $n (${NAMES[$n]}): OK për $m min"
  else
    local st=$?
    status "$n" "DËSHTOI" "kodi $st — $(tail -1 "$log" | cut -c1-150 | tr '|' '/')"
    say "faza $n (${NAMES[$n]}): DËSHTOI (kodi $st). Shiko $log. Pas rregullimit, e njëjta komandë vazhdon nga kjo fazë."
    exit "$st"
  fi
}

p0() {
  "$MX" check "$R/testnet"
}
p1() {
  cd "$ROOT" && cargo test --release --workspace
}
p2() {
  local db bin="${ZILJA_AGAVE_BIN:-$HOME/.local/share/solana/install/active_release/bin}"
  [ -x "$bin/solana-test-validator" ] || { echo "s'gjendet solana-test-validator te $bin: nis scripts/pc-setup.sh"; return 2; }
  (cd "$ROOT" && cargo build --release -q -p zilja-plugin -p zilja-cli -p zilja-aggregator -p zilja-bench) || return 1
  for db in pages 0; do
    echo "=== e2e ZILJA_DB=$db ==="
    rm -rf "$R/e2e-$db"
    ZILJA_DB=$db WORK="$R/e2e-$db" bash "$ROOT/scripts/e2e-test-validator.sh" "$bin" || return 1
  done
}
p3() {
  # shellcheck disable=SC2086
  env NETWORK=testnet $T_EXTRA "$MX" "$TESTNET_URL" "$R/testnet"
}
p4() {
  local free; free=$(df -BG --output=avail "$R" | tail -1 | tr -dc 0-9)
  if [ "${free:-0}" -lt "$MAINNET_MIN_FREE_GB" ] && [ -z "$M_EXTRA" ]; then
    # L4: snapshot-i i Testnet-it fshihet vetëm kur faza 3 mbaroi (rezultatet mbeten). Përndryshe
    # ripërdoret nga një rinisje e fazës 3 dhe s'duhet shkarkuar sërish (~100 GB, me kufi 429).
    if [ -f "$R/phase3.done" ]; then
      echo "Pak vend: fshij snapshot-in e Testnet-it (faza 3 mbaroi)."
      rm -f "$R"/testnet/snapshot-*.tar.zst "$R"/testnet/snapshot.part
      free=$(df -BG --output=avail "$R" | tail -1 | tr -dc 0-9)
    else
      echo "Pak vend, por faza 3 s'ka mbaruar: snapshot-i i Testnet-it mbetet për rinisjen e saj."
    fi
  fi
  if [ "${free:-0}" -lt "$MAINNET_MIN_FREE_GB" ] && [ -z "$M_EXTRA" ]; then
    echo "Vetëm ${free} GB të lirë; Mainnet-i i plotë kërkon ≥ ${MAINNET_MIN_FREE_GB} GB (MAINNET_MIN_FREE_GB për ta ndryshuar)."
    return 3
  fi
  # Tri nisje në të njëjtën dosje: snapshot-i shkarkohet një herë, rastet e mbaruara kapërcehen.
  # shellcheck disable=SC2086
  env NETWORK=mainnet BENCH_GROUPS=size LIMITS=full SLOTS=1024 UPDATES="$M_UPDATES" $M_EXTRA "$MX" "$MAINNET_URL" "$R/mainnet" || return 1
  # shellcheck disable=SC2086
  env NETWORK=mainnet BENCH_GROUPS=cold COLD_MODES=slot COLD_LIMIT=full SLOTS=512 UPDATES="$M_UPDATES" $M_EXTRA "$MX" "$MAINNET_URL" "$R/mainnet" || return 1
  # shellcheck disable=SC2086
  env NETWORK=mainnet BENCH_GROUPS=cache CACHE_DEPTHS=24 COLD_LIMIT=full SLOTS=512 UPDATES="$M_UPDATES" $M_EXTRA "$MX" "$MAINNET_URL" "$R/mainnet" || return 1
}
p5() {
  local net
  for net in testnet mainnet; do
    [ -f "$R/$net/RESULTS.md" ] || { echo "$net: s'ka RESULTS.md, e kapërcej"; continue; }
    NETWORK=$net "$MX" publish "$R/$net" || return 1
  done
  echo "Rezultatet janë te $ROOT/bench-results/ (gati për PR)."
}

say "=== pc-run-all: fazat '$PHASES' → $R ==="
grep -qi microsoft /proc/version 2>/dev/null && say "WSL2: mbaje dritaren e Ubuntu-s hapur dhe PC-në pa gjumë (Windows: Cilësimet → Energjia → Gjumi: Kurrë)."
for n in $PHASES; do
  case "$n" in
    0) run_phase 0 p0 ;;
    1) run_phase 1 p1 ;;
    2) run_phase 2 p2 ;;
    3) run_phase 3 p3 ;;
    4) run_phase 4 p4 ;;
    5) rm -f "$R/phase5.done"; run_phase 5 p5 ;;
    *) echo "fazë e panjohur: $n (0–5)"; exit 2 ;;
  esac
done
say "=== gati: $(grep -c OK "$R/status.tsv") faza OK. Shiko $R/STATUS.md ==="
