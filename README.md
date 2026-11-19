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
| Tavolina dhe rafti | Pema në disk (RocksDB) | `crates/zilja-store` |
| Regjistri i radhës së sloteve | Ndjekja e bankave, fork-eve, Rooted | `crates/zilja-node` (`tracker`) |
| Dëftesa e hambarit | `getAccountProof` | `crates/zilja-node` (`rpc`) |
| Banori me telefon | Verifikuesi i lehtë | `crates/zilja-verifier`, `crates/zilja-wasm`, `web/` |
| Kuleta që e tregon "✅" | SDK npm `zilja-verify` | `sdk/js` |
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
# Parazgjedhja kërkon ≥ 2 zile që pajtohen (--also); me një zile të vetme: --min-trust single.
./target/release/zilja-check http://127.0.0.1:7799 <PUBKEY> http://127.0.0.1:8899 --min-trust single
node web/check.mjs http://127.0.0.1:7799 <PUBKEY> http://127.0.0.1:8899 --min-trust single   # i njëjti vendim në JS
#   --also <ZILJA_URL>  zile e dytë · --quorum  firmat e validatorëve · --max-lag <n>|none · --min-slot <n>
#   Kodi i daljes: 0 vetëm kur kalon çdo kontroll (dëftesa, besimi, freskia, të dhënat).
# ose hap web/index.html në shfletues

# Pema në disk në vend të RAM-it: shto te zilja.json
#   "db_path": "/var/lib/zilja/db", "history_slots": 32, "cache_depth": 20
./target/release/zilja-check http://127.0.0.1:7799 <PUBKEY> --min-trust single --slot <SLOT>   # dëftesë historike

# 4. A e kanë firmosur pleqtë? Shto te zilja.json:
#   "attest_keypair": "<çelësi>", "attest_authorization": "<nga zilja-authorize>",
#   "genesis_path": "<ledger>/genesis.bin"          (shih docs/OPERIMI.md)
./target/release/zilja-quorum http://127.0.0.1:8899 http://127.0.0.1:7799
```

Metrikat janë në `http://127.0.0.1:7798/metrics`, një port privat. RPC-ja publike duhet
të qëndrojë pas një proxy, shih [`docs/OPERIMI.md`](docs/OPERIMI.md).

Testi i plotë automatik (validator i vërtetë, transferime, rinisje nga snapshot, ringarkim i
plugin-it, zinxhir i gabuar, rindërtim):

```bash
scripts/e2e-test-validator.sh /rruga/te/solana-release/bin
```

## Statusi

| Faza | Statusi |
|---|---|
| 0. Përgatitja | ✅ workspace, CI, plugin mbi `solana-test-validator` · ⏳ node testnet 24 orë |
| 1. Zilja jashtë zinxhirit | ✅ pema + plugin; root-i nga snapshot-i = root-i slot pas sloti |
| 2. Dëftesat | ✅ ~0.7–1 KB, RPC 0.1 ms me 1M llogari |
| 3. Verifikuesi | ✅ Rust, WASM 64 KB, JS 5 KB, faqja demo · ⏳ aplikacion telefoni |
| 4. Firma e pleqve | ✅ firma jashtë protokollit + kuorum 2/3 mbi stake-un real · ⏳ fork Agave |
| 5. Stresi dhe sulmet | ✅ fork-e, banka Dead, 50 vektorë sulmi · ⏳ ledger replay mainnet |
| 6. Korrja | ✅ raporti dhe drafti SIMD |
| SDK për kuletat | ✅ `zilja-verify` (npm, pa varësi): dëftesa + krahasim zilesh + kuorum + krahasim të dhënash · ⏳ e pa publikuar në npm |
| Light client me checkpoint | ⏳ e shtyrë (dega lokale `pika-3-light-client`) |
| Pema në disk | ✅ RocksDB me faqe 4-nivelëshe, atomike, rinisje pa snapshot, 32 slote histori, mbrojtje nga zinxhiri i gabuar · ⚠️ ~315 ms/slot me 10M llogari në 2 bërthama |

| Auditimi (15 nëntor 2026) | ✅ v0.2.0: të gjitha gjetjet (2 kritike, 3 të larta, 8 mesatare, 9 të ulëta, 6 info) të rregulluara dhe të testuara: [`docs/AUDIT-FIXES.md`](docs/AUDIT-FIXES.md) · ⏳ ri-auditimi |

Numrat e plotë janë te [`docs/RAPORTI.md`](docs/RAPORTI.md).

## Për kuletat: `zilja-verify`

```js
import { ZiljaClient } from "zilja-verify";
const zilja = new ZiljaClient({ endpoints: ["https://zilja-1.example", "https://zilja-2.example"], minTrust: "agreement" });
const r = await zilja.verifyAccount(pubkey);   // r.ok, r.verdict, r.account.lamports, r.trust
```

Pa varësi, 8.7 KB e paketuar, me tipe TypeScript. Shih [`sdk/js/README.md`](sdk/js/README.md).

## Dokumentet

- [`docs/SPEC.md`](docs/SPEC.md): formati v2 i hash-eve, i dëftesës, i firmave dhe i RPC-së
- [`docs/OPERIMI.md`](docs/OPERIMI.md): portet, proxy, ndalimi dhe rindërtimi, firmosja
- [`docs/AUDIT-FIXES.md`](docs/AUDIT-FIXES.md): çdo gjetje e auditimit → rregullimi → testi
- [`docs/RAPORTI.md`](docs/RAPORTI.md): numrat e matur, faza pas faze
- [`docs/SIMD-DRAFT.md`](docs/SIMD-DRAFT.md): drafti i propozimit për protokollin

## Versioni i validatorit

Plugin-i ngarkohet si trait object, kështu që `agave-geyser-plugin-interface`
duhet të përputhet me versionin e validatorit. Tani: **Agave 4.3.2**
(`crates/zilja-plugin/Cargo.toml`).

Licenca: Apache-2.0
