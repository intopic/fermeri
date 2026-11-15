# Fermeri: Zilja e fshatit 🔔

**Zilja** është një vulë globale e gjendjes për Solana: një root Merkle mbi
*të gjitha* llogaritë, i përditësuar në çdo slot të rrënjosur. Me të, çdo banor
(kuletë, telefon, faqe web) verifikon llogarinë e vet me një dëftesë të vogël
(~1 KB), pa i besuar RPC-së.

> *Zilja is a global state commitment for Solana: a sparse Merkle root over all
> accounts, updated every rooted slot, built as a Geyser plugin (no validator
> changes). Light clients verify any account with a ~1 KB proof.*

## Analogjia

| Fshati | Solana | Në këtë repo |
|---|---|---|
| Shkruesi pranë plakut | Geyser plugin | `crates/zilja-plugin` |
| Vula e fshatit (zilja) | Root-i i pemës Merkle | `crates/zilja-tree` |
| Regjistri i radhës së sloteve | Ndjekja e bankave, fork-eve, Rooted | `crates/zilja-node` (`tracker`) |
| Dëftesa e hambarit | `getAccountProof` | `crates/zilja-node` (`rpc`) |
| Banori me telefon | Verifikuesi i lehtë | `crates/zilja-verifier`, `crates/zilja-wasm`, `web/` |
| Banori në terminal | CLI | `crates/zilja-cli` (`zilja-check`) |
| Pleqtë firmosin ziles | Kuorumi > 2/3 stake | `zilja-verifier` me `--features attest` |

## Nisja e shpejtë

```bash
# 1. Ndërto
cargo build --release -p zilja-plugin -p zilja-cli

# 2. Nis një validator me ziles
cat > zilja.json <<EOF
{ "libpath": "$PWD/target/release/libzilja_plugin.so",
  "bind": "127.0.0.1:7799",
  "roots_log": "$PWD/roots.log" }
EOF
solana-test-validator --geyser-plugin-config zilja.json

# 3. Verifiko një llogari si banor
./target/release/zilja-check http://127.0.0.1:7799 <PUBKEY> http://127.0.0.1:8899
```

Testi i plotë automatik (validator i vërtetë, transferime, rinisje nga snapshot):

```bash
scripts/e2e-test-validator.sh /rruga/te/solana-release/bin
```

## Dokumentet

- [`docs/SPEC.md`](docs/SPEC.md): formati i hash-eve, i dëftesës dhe i RPC-së
- [`docs/RAPORTI.md`](docs/RAPORTI.md): numrat e matur, faza pas faze
- [`docs/SIMD-DRAFT.md`](docs/SIMD-DRAFT.md): drafti i propozimit për protokollin

## Versioni i validatorit

Plugin-i ngarkohet si trait object, kështu që `agave-geyser-plugin-interface`
duhet të përputhet me versionin e validatorit. Tani: **Agave 4.3.2**
(`crates/zilja-plugin/Cargo.toml`).

Licenca: Apache-2.0
