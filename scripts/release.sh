#!/usr/bin/env bash
# A09, A23, A25, hapi 9: ndërton paketën e release-it (vetëm "pages", pa RocksDB) dhe manifestin e saj.
#
#   scripts/release.sh [dosja-dalëse]          (parazgjedhja: dist/)
#
# - Ndërton plugin-in dhe CLI-të DY HERË, në dy dosje target të pastra, dhe kërkon hash-e identike
#   (artefakt i riprodhueshëm me të njëjtin target, kompilator, varësi dhe feature).
# - Kontrollon që kompilatori është ai i `rust-toolchain.toml` (Agave 4.3.2 kërkon rustc 1.97.1:
#   plugin-i ngarkohet si trait object) dhe që ndërfaqja Geyser në Cargo.lock = versioni i Agave.
# - Shkruan MANIFEST.json: commit, `rustc -vV`, `cargo -V`, target, hash-i i Cargo.lock, versionet e
#   formateve, hash-et e artefakteve, madhësitë e SDK-së (tarball npm + bundle i minifikuar).
# - Paketa përmban vetëm: libzilja_plugin.so, zilja-check, zilja-quorum, zilja-authorize, LICENSE,
#   README.md, docs/OPERIMI.md, docs/SPEC.md, MANIFEST.json. Asnjë vegël bench / skript shkatërrues (A22).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:-$ROOT/dist}
TARGET=x86_64-unknown-linux-gnu
AGAVE_VERSION=${AGAVE_VERSION:-v4.3.2}
cd "$ROOT"

want_rustc=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
got_rustc=$(rustc -V | awk '{print $2}')
OFFICIAL=true
EXTRA=""
if [ "$got_rustc" != "$want_rustc" ]; then
  # Vetëm për prova lokale: paketa etiketohet jo-zyrtare dhe s'duhet shpërndarë.
  [ "${ZILJA_UNOFFICIAL_TOOLCHAIN:-0}" = 1 ] || { echo "GABIM: rustc $got_rustc ≠ $want_rustc nga rust-toolchain.toml"; exit 1; }
  echo "⚠️  rustc $got_rustc ≠ $want_rustc: paketë JO-ZYRTARE (s'përputhet me ABI-në e Agave)"
  OFFICIAL=false
  EXTRA=--ignore-rust-version
fi
iface=$(awk '/^name = "agave-geyser-plugin-interface"/{getline; gsub(/version = |"/,""); print; exit}' Cargo.lock)
[ "v$iface" = "$AGAVE_VERSION" ] || { echo "GABIM: agave-geyser-plugin-interface $iface ≠ Agave $AGAVE_VERSION"; exit 1; }
if cargo tree -p zilja-plugin -e normal,build --target "$TARGET" | grep -iE "rocksdb|bindgen|clang-sys"; then
  echo "GABIM: release-i përmban motorin e vjetër RocksDB (A21)"; exit 1
fi

BINS=(libzilja_plugin.so zilja-check zilja-quorum zilja-authorize)
build() { # dosja target
  # shellcheck disable=SC2086
  CARGO_TARGET_DIR="$1" cargo build -q --locked --release --target "$TARGET" -p zilja-plugin -p zilja-cli $EXTRA
}
WORK=$(mktemp -d)
trap 'rm -rf -- "$WORK"' EXIT
build "$WORK/a"
build "$WORK/b"
for b in "${BINS[@]}"; do
  ha=$(sha256sum "$WORK/a/$TARGET/release/$b" | cut -d' ' -f1)
  hb=$(sha256sum "$WORK/b/$TARGET/release/$b" | cut -d' ' -f1)
  [ "$ha" = "$hb" ] || { echo "GABIM: $b s'është i riprodhueshëm ($ha ≠ $hb)"; exit 1; }
done

# SDK: tarball-i npm dhe bundle-t e minifikuara (A25: madhësia e artefaktit të emërtuar).
SDK_JSON=$(cd sdk/js && npm pack --dry-run --json 2>/dev/null)
echo 'export { verifyAccount } from "./src/verifier.js";' > sdk/js/.entry-verify.mjs
trap 'rm -rf -- "$WORK"; rm -f "$ROOT/sdk/js/.entry-verify.mjs"' EXIT
(cd sdk/js && npx -y esbuild@0.24 src/index.js --bundle --minify --format=esm --log-level=error) > "$WORK/full.js"
(cd sdk/js && npx -y esbuild@0.24 ./.entry-verify.mjs --bundle --minify --format=esm --log-level=error) > "$WORK/verify.js"
rm -f sdk/js/.entry-verify.mjs

COMMIT=$(git rev-parse HEAD)
NAME="zilja-${COMMIT:0:12}-$TARGET"
PKG="$OUT/$NAME"
rm -rf -- "$PKG" "$PKG.tar.gz"
mkdir -p "$PKG/docs"
for b in "${BINS[@]}"; do cp "$WORK/a/$TARGET/release/$b" "$PKG/"; done
cp LICENSE README.md "$PKG/"
cp docs/OPERIMI.md docs/SPEC.md "$PKG/docs/"

python3 - "$PKG" "$COMMIT" "$TARGET" "$AGAVE_VERSION" "$iface" "$WORK" "$SDK_JSON" "$OFFICIAL" <<'PY'
import hashlib, json, os, re, subprocess, sys
pkg, commit, target, agave, iface, work, sdk, official = sys.argv[1:9]
sha = lambda p: hashlib.sha256(open(p, "rb").read()).hexdigest()
grab = lambda path, pat: re.search(pat, open(path).read()).group(1)
npm = json.loads(sdk)[0]
full, verify = open(f"{work}/full.js", "rb").read(), open(f"{work}/verify.js", "rb").read()
import gzip
m = {
    "official": official == "true",
    "commit": commit,
    "target": target,
    "rustc": subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout.strip().splitlines(),
    "cargo": subprocess.run(["cargo", "-V"], capture_output=True, text=True).stdout.strip(),
    "cargo_lock_sha256": sha("Cargo.lock"),
    "agave": {"version": agave, "geyser_plugin_interface": iface},
    "features": "parazgjedhja: motori i vetëm pages (RocksDB u hoq, R6)",
    "formats": {
        "pages": {"name": grab("crates/zilja-store/src/pagefile.rs", r'FORMAT_NAME: &str = "([^"]+)"'),
                  "format_version": int(grab("crates/zilja-store/src/pagefile.rs", r"pub const FORMAT_VERSION: u64 = (\d+)"))},
        "proof_version": int(grab("crates/zilja-verifier/src/lib.rs", r"pub const PROOF_VERSION: u8 = (\d+)")),
        "attest_log": grab("crates/zilja-node/src/attester.rs", r'const HEADER: &str = "# ([^"]+)"'),
        "attest_message": "zilja/attest/v2",
    },
    # G29: çdo skedar i paketës, edhe në nëndosje (docs/), me rrugë relative; vetë MANIFEST.json jo.
    "artifacts": {f: {"sha256": sha(os.path.join(pkg, f)), "bytes": os.path.getsize(os.path.join(pkg, f))}
                  for f in sorted(os.path.relpath(os.path.join(d, n), pkg) for d, _, ns in os.walk(pkg) for n in ns)
                  if f != "MANIFEST.json"},
    "reproducible": "dy ndërtime në dosje target të ndara dhanë hash-e identike",
    "sdk_js": {
        "npm_tarball_bytes": npm["size"], "npm_unpacked_bytes": npm["unpackedSize"],
        "npm_files": [f["path"] for f in npm["files"]],
        "bundle_full_min_bytes": len(full), "bundle_full_min_gzip_bytes": len(gzip.compress(full, 9)),
        "bundle_verify_only_min_bytes": len(verify), "bundle_verify_only_min_gzip_bytes": len(gzip.compress(verify, 9)),
        "measured_with": "npm pack --dry-run; esbuild@0.24 --bundle --minify --format=esm (src/index.js / vetëm verifyAccount)",
    },
}
assert "docs/SPEC.md" in m["artifacts"] and "docs/OPERIMI.md" in m["artifacts"], "G29: docs/ mungon nga MANIFEST"
assert "LICENSE" in m["artifacts"] and "LICENSE" in m["sdk_js"]["npm_files"], "A23: LICENSE mungon"
json.dump(m, open(os.path.join(pkg, "MANIFEST.json"), "w"), indent=1, ensure_ascii=False)
print(json.dumps({k: m[k] for k in ("commit", "agave", "formats", "sdk_js")}, indent=1, ensure_ascii=False))
PY
tar -C "$OUT" -czf "$PKG.tar.gz" "$NAME"
echo "✅ $PKG.tar.gz"
sha256sum "$PKG.tar.gz"
