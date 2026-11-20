# shellcheck shell=bash
# H1/M4: URL-ja e saktë e snapshot-it, pa e shkarkuar.
#
# `/snapshot.tar.bz2` i RPC-së ridrejton (30x) te snapshot-i më i ri, p.sh.
# `/snapshot-<slot>-<hash>.tar.zst`, që ndryshon çdo disa orë. Vazhdimi i një shkarkimi
# (`curl -C -`) mbi URL-në e ridrejtuar mund të ngjisë dy snapshot-e të ndryshme në një skedar;
# ndaj shkarkimi bëhet gjithmonë mbi URL-në përfundimtare që jep kjo funksion.
#
#   snapshot_resolve <url>   → shkruan URL-në përfundimtare; kthen 0 (200), 2 (429, kufi
#                              shkarkimesh) ose 1 (gabim tjetër, me kodin HTTP pas URL-së)
#
# Ndiqen deri në 8 ridrejtime një nga një, pa trupin e përgjigjes: `--max-filesize` e ndal curl-in
# para trupit kur përgjigjja 200 është vetë skedari i madh (kodi 63), por `-w` jep sërish kodin HTTP.
snapshot_resolve() {
  local u=$1 i out code next
  for i in 1 2 3 4 5 6 7 8; do
    out=$(curl -sS -o /dev/null --max-filesize 65536 --connect-timeout 20 --max-time 60 \
      -w '%{http_code} %{redirect_url}' "$u" 2>/dev/null)
    code=${out%% *}
    next=${out#"$code"}; next=${next# }
    case "$code" in
      301|302|303|307|308)
        [ -n "$next" ] || { echo "$u (HTTP $code pa Location)"; return 1; }
        u=$next ;;
      200) echo "$u"; return 0 ;;
      429) echo "$u"; return 2 ;;
      *) echo "$u (HTTP ${code:-000})"; return 1 ;;
    esac
  done
  echo "$u (shumë ridrejtime)"
  return 1
}

# Si `snapshot_resolve`, por provon sërish deri në `tries` herë kur serveri kthen 429.
snapshot_resolve_retry() { # url [tries=5] [pritja s=30]
  local u=$1 tries=${2:-5} wait=${3:-30} n out rc
  for ((n = 1; n <= tries; n++)); do
    out=$(snapshot_resolve "$u"); rc=$?
    [ $rc -eq 2 ] || { echo "$out"; return $rc; }
    [ $n -lt "$tries" ] && sleep "$wait"
  done
  echo "$out"
  return 2
}
