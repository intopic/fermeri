#!/usr/bin/env bash
# Teste për skriptet e PC-së, pa Solana dhe pa snapshot të vërtetë (CI, ~10 s):
#   H1  shkarkimi i snapshot-it rinis vetëm mbi të njëjtën URL; snapshot i ri → nga e para;
#       server pa Range → nga e para
#   M2  rastet e dështuara japin kodin 4; pc-run-all vazhdon me fazën tjetër dhe del me 4
#   M4  snapshot_resolve: ridrejtim, 404, 429, port i mbyllur
# Një server HTTP lokal luan rolin e RPC-së (`/snapshot.tar.bz2` → 303 → `/snapshot-*.tar.zst`).
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/snapshot-url.sh
. "$ROOT/scripts/snapshot-url.sh"
W=$(mktemp -d); trap 'kill "$SRV" 2>/dev/null; rm -rf "$W"' EXIT
mkdir -p "$W/srv"
head -c 2000000 /dev/urandom > "$W/srv/snapshot-100-aaa.tar.zst"
head -c 2000000 /dev/urandom > "$W/srv/snapshot-200-bbb.tar.zst"
echo snapshot-100-aaa.tar.zst > "$W/srv/target"
cat > "$W/srv.py" <<'PY'
import http.server, os, re, socket, sys
os.chdir(sys.argv[1])
class H(http.server.BaseHTTPRequestHandler):
    def reply(self, code, headers=()):
        self.send_response(code)
        for k, v in headers: self.send_header(k, v)
        self.send_header("Content-Length", "0"); self.end_headers()
    def do_GET(self):
        if self.path == "/snapshot.tar.bz2":
            return self.reply(303, [("Location", "/" + open("target").read().strip())])
        if self.path == "/limited": return self.reply(429)
        f = self.path.lstrip("/")
        if not os.path.isfile(f): return self.reply(404)
        d = open(f, "rb").read(); start = 0
        m = re.match(r"bytes=(\d+)-", self.headers.get("Range") or "")
        if m and os.path.exists("ranges"):
            start = int(m.group(1)); self.send_response(206)
            self.send_header("Content-Range", f"bytes {start}-{len(d)-1}/{len(d)}")
        else: self.send_response(200)
        self.send_header("Content-Length", str(len(d) - start)); self.end_headers()
        try: self.wfile.write(d[start:])
        except OSError: pass
    def log_message(self, *a): pass
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
open("port", "w").write(str(s.server_address[1]))
s.serve_forever()
PY
python3 "$W/srv.py" "$W/srv" & SRV=$!
for _ in $(seq 50); do [ -s "$W/srv/port" ] && break; sleep 0.1; done
U="http://127.0.0.1:$(cat "$W/srv/port")"
fail=0
check() { if eval "$2"; then echo "✅ $1"; else echo "❌ $1"; fail=1; fi; }

# M4: snapshot_resolve
out=$(snapshot_resolve "$U/snapshot.tar.bz2"); rc=$?
check "resolve: ridrejtimi → snapshot-100" '[ $rc = 0 ] && [ "$out" = "$U/snapshot-100-aaa.tar.zst" ]'
out=$(snapshot_resolve "$U/snapshot-100-aaa.tar.zst"); rc=$?
check "resolve: skedari vetë (200), pa e shkarkuar" '[ $rc = 0 ] && [ "$out" = "$U/snapshot-100-aaa.tar.zst" ]'
snapshot_resolve "$U/limited" >/dev/null; rc=$?
check "resolve: 429 → kodi 2" '[ $rc = 2 ]'
out=$(snapshot_resolve "$U/gone"); rc=$?
check "resolve: 404 → kodi 1" '[ $rc = 1 ] && [[ $out == *"HTTP 404"* ]]'
snapshot_resolve "http://127.0.0.1:1/x" >/dev/null; rc=$?
check "resolve: port i mbyllur → kodi 1" '[ $rc = 1 ]'

# H1: tri rinisje të shkarkimit.
mx() { BIN=/bin/false BENCH_GROUPS=size LIMITS=1 RPC_CLIENTS='' bash "$ROOT/scripts/bench-matrix.sh" "$U/snapshot.tar.bz2" "$1" > "$1.log" 2>&1; }
partial() { # dosja snapshot-i_i_pjesës
  rm -rf "$1"; mkdir -p "$1"
  head -c 700000 "$W/srv/$2" > "$1/snapshot.part"; echo "$U/$2" > "$1/snapshot.url"
}
touch "$W/srv/ranges"
partial "$W/a" snapshot-100-aaa.tar.zst; mx "$W/a"; st=$?
check "H1: e njëjta URL → vazhdon, skedari i plotë dhe i saktë" 'cmp -s "$W/a/snapshot-100-aaa.tar.zst" "$W/srv/snapshot-100-aaa.tar.zst" && grep -q "Resuming transfer" "$W/a.log"'
check "M2: rast i dështuar → kodi 4" '[ $st = 4 ]'
echo snapshot-200-bbb.tar.zst > "$W/srv/target"
partial "$W/b" snapshot-100-aaa.tar.zst; mx "$W/b"
check "H1: snapshot i ri në server → pjesa e vjetër fshihet, skedari i ri i saktë" 'cmp -s "$W/b/snapshot-200-bbb.tar.zst" "$W/srv/snapshot-200-bbb.tar.zst" && grep -q "ndryshoi" "$W/b.log" && [ ! -e "$W/b/snapshot-100-aaa.tar.zst" ]'
rm -f "$W/srv/ranges"
partial "$W/c" snapshot-200-bbb.tar.zst; mx "$W/c"
check "H1: server pa Range → nis nga e para, skedari i saktë" 'cmp -s "$W/c/snapshot-200-bbb.tar.zst" "$W/srv/snapshot-200-bbb.tar.zst" && grep -q "kodi 33" "$W/c.log"'

# M2: pc-run-all vazhdon pas fazës 3 të pjesshme dhe kryen tri nisjet e fazës 4.
RESULTS="$W/res" TOOLS="$W/tools" PHASES="3 4" TESTNET_URL="$U/snapshot.tar.bz2" MAINNET_URL="$U/snapshot.tar.bz2" \
  T_EXTRA="BIN=/bin/false RPC_CLIENTS= BENCH_GROUPS=size LIMITS=1" M_EXTRA="BIN=/bin/false RPC_CLIENTS=" \
  bash "$ROOT/scripts/pc-run-all.sh" > "$W/run.log" 2>&1; st=$?
check "M2: pc-run-all → kodi 4, faza 3 dhe 4 PJESËRISHT" '[ $st = 4 ] && [ "$(grep -c PJESËRISHT "$W/res/status.tsv")" = 2 ] && [ ! -e "$W/res/phase3.done" ]'
check "M2: faza 4 provon të tri nisjet" '[ "$(grep -c "DËSHTUAN:" "$W/res/phase4-mainnet.log")" = 3 ]'

[ $fail = 0 ] && echo "✅ skriptet e PC-së: të gjitha kalojnë" || { echo "❌ skriptet e PC-së: disa teste dështuan"; exit 1; }
