#!/usr/bin/env bash
# Përgatit një PC të ri për testet (docs/PC-TESTI.md). Pa sudo: instalon Rust-in (~/.cargo),
# Agave-n dhe Node.js 22 te <dosja e repo-s>/../tools (si në CI: Agave nga GitHub releases) dhe
# ndërton binarët; shkruan tools/env.sh që e përdor scripts/pc-run-all.sh. Veglat e sistemit (kompilatori C, git, …) kërkojnë
# sudo një herë: nëse mungojnë, skripti tregon komandën e saktë dhe ndalon.
#
#   scripts/pc-setup.sh
#
# Mund të niset sa herë të duash: kapërcen atë që është gati.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGAVE_VERSION="${AGAVE_VERSION:-v4.3.2}"
TOOLS="${TOOLS:-$(dirname "$ROOT")/tools}"
mkdir -p "$TOOLS"
say() { echo "[$(date +%T)] $*"; }

[ "$(uname -s)" = Linux ] || { echo "Duhet Linux ose WSL2 (Ubuntu)."; exit 1; }
case "$ROOT" in /mnt/[a-z]/*) echo "Kodi është te disku i Windows-it ($ROOT). Vendose brenda Linux-it, p.sh. ~/zilja."; exit 1 ;; esac

# 1. Veglat e sistemit (një herë me sudo, nga ti).
miss=()
for t in cc make pkg-config git curl python3 tar bzip2 xz; do command -v "$t" >/dev/null || miss+=("$t"); done
python3 -c 'import matplotlib' 2>/dev/null || miss+=("matplotlib")
if [ ${#miss[@]} -gt 0 ]; then
  echo "Mungojnë: ${miss[*]}"
  echo "Nise një herë këtë komandë (kërkon fjalëkalimin tënd), pastaj rinise këtë skript:"
  echo "  sudo apt update && sudo apt install -y build-essential pkg-config git curl python3-matplotlib bzip2 xz-utils"
  exit 1
fi
say "veglat e sistemit: gati"

# 2. Rust (te ~/.cargo, pa sudo). Versioni i saktë merret vetë nga rust-toolchain.toml.
if ! command -v cargo >/dev/null; then
  [ -x "$HOME/.cargo/bin/cargo" ] || { say "po instaloj Rust…"; curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null || exit 1; }
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi
(cd "$ROOT" && rustc --version) || exit 1
say "Rust: gati"

# 3. Agave (si në CI: arkivi zyrtar nga GitHub releases), për testin me validator të vërtetë.
AG="$TOOLS/agave-$AGAVE_VERSION/solana-release/bin"
if ! "$AG/solana-test-validator" --version 2>/dev/null | grep -q "${AGAVE_VERSION#v}"; then
  say "po shkarkoj Agave $AGAVE_VERSION…"
  mkdir -p "$TOOLS/agave-$AGAVE_VERSION"
  curl -fsSL --retry 3 -o "$TOOLS/agave-$AGAVE_VERSION/agave.tar.bz2" \
    "https://github.com/anza-xyz/agave/releases/download/$AGAVE_VERSION/solana-release-x86_64-unknown-linux-gnu.tar.bz2" \
    || { echo "shkarkimi i Agave-s dështoi"; exit 1; }
  tar xjf "$TOOLS/agave-$AGAVE_VERSION/agave.tar.bz2" -C "$TOOLS/agave-$AGAVE_VERSION" && rm "$TOOLS/agave-$AGAVE_VERSION/agave.tar.bz2"
fi
"$AG/solana-test-validator" --version || exit 1
say "Agave: gati ($AG)"

# 3b. Node.js 22 (SDK-ja kërkon >= 22; Ubuntu jep më të vjetër): arkivi zyrtar, pa sudo.
NODE_DIR="$TOOLS/node22"
sys_node=$(command -v node || true)
if [ -n "$sys_node" ] && "$sys_node" --version 2>/dev/null | grep -Eq '^v(2[2-9]|[3-9][0-9])'; then
  NODE_DIR="$(dirname "$(dirname "$(readlink -f "$sys_node")")")"   # Node i sistemit mjafton
elif ! "$NODE_DIR/bin/node" --version 2>/dev/null | grep -q '^v2[2-9]'; then
  say "po shkarkoj Node.js 22…"
  arch=$(uname -m); case "$arch" in x86_64) na=x64 ;; aarch64) na=arm64 ;; *) echo "arkitekturë e panjohur: $arch"; exit 1 ;; esac
  f=$(curl -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt | awk -v a="linux-$na.tar.xz" '$2 ~ a {print $2; exit}')
  [ -n "$f" ] || { echo "s'u gjet arkivi i Node.js 22"; exit 1; }
  rm -rf "$NODE_DIR" && mkdir -p "$NODE_DIR"
  curl -fsSL "https://nodejs.org/dist/latest-v22.x/$f" | tar xJ -C "$NODE_DIR" --strip-components=1 || { echo "shkarkimi i Node.js dështoi"; exit 1; }
fi
say "Node.js: $("$NODE_DIR/bin/node" --version)"

cat > "$TOOLS/env.sh" <<ENV
# Krijuar nga scripts/pc-setup.sh
export PATH="$AG:$NODE_DIR/bin:\$HOME/.cargo/bin:\$PATH"
export ZILJA_AGAVE_BIN="$AG"
ENV

# 4. Binarët e testeve.
say "po ndërtoj plugin-in, CLI-në dhe bench-et (release, disa minuta herën e parë)…"
(cd "$ROOT" && cargo build --release -q -p zilja-plugin -p zilja-cli -p zilja-bench -p zilja-aggregator) || exit 1
say "gati. Hapi tjetër: scripts/pc-run-all.sh (shiko docs/PC-TESTI.md)"
