# Raporti: Zilja e fshatit, faza pas faze

Data: 15 nëntor 2026 · Validatori: **Agave 4.3.2** (`solana-test-validator`) ·
Makina e matjeve: **2 bërthama, 7 GB RAM** (shumë më e vogël se një RPC node mainnet).

## Përmbledhja

Zilja funksionon si **Geyser plugin**, pa ndryshuar validatorin. Mbi një
validator të vërtetë Agave ajo:

1. ndërton vulën globale (root Merkle) mbi të gjitha llogaritë dhe e përditëson në çdo slot të rrënjosur;
2. shërben dëftesa prej ~0.3–1 KB për çdo llogari;
3. dëftesat verifikohen në Rust, në WASM (64 KB) dhe në JavaScript (5 KB) pa i besuar serverit;
4. root-i i rindërtuar nga snapshot-i është **identik** me root-in e ndërtuar slot pas sloti;
5. validatori e firmos root-in me çelësin e identitetit, dhe kuorumi > 2/3 kontrollohet mbi stake-un real.

Kjo provon pjesën që nuk varet nga madhësia. Mbetet të provohet madhësia
**mainnet**, në një server të vërtetë (shih "Çfarë mbetet").

## Objektivat kundrejt rezultateve

| Metrika | Objektivi | Rezultati | Statusi |
|---|---|---|---|
| Përditësimi i root-it për slot | < 50 ms | **18 ms** p50 / 26 ms p99 (1M llogari, 5 000 ndryshime/slot); 75 ms p50 (5M, 20 000/slot) | ✅ për 5k/slot · ⚠️ 20k/slot kërkon më shumë bërthama |
| Madhësia e dëftesës | ≤ 10 KB | **711 B** p50, 840 B max (1M); 807 B p50, 968 B max (5M) | ✅ |
| Verifikimi | < 50 ms | **3.5 µs** (Rust), **0.6 ms** (JS në Node) | ✅ |
| Madhësia e verifikuesit | < 500 KB | **64 KB** WASM, **5.3 KB** JS (2.2 KB gzip) | ✅ |
| Përgjigja e RPC-së | < 100 ms | **0.10 ms** p50, 0.22 ms p99 (1M llogari, HTTP lokal) | ✅ |
| Memoria shtesë | < 10% | RAM: 300–370 B/llogari · Disk: RAM pothuajse fiks, 200–230 B/llogari në disk | ⚠️ disku e zgjidh RAM-in, por është ende i ngadaltë (shih "Pema në disk") |
| CPU shtesë për validatorin | < 5% | E pamatur: kërkon një node mainnet | ⏳ |

## Faza 0: Përgatitja ✅ (në shkallë të vogël)

- Workspace Rust me 8 crate, CI në GitHub Actions me 3 punë: testet, WASM-i dhe testi i plotë mbi validatorin.
- Plugin-i ngarkohet nga `solana-test-validator` 4.3.2 përmes `--geyser-plugin-config`.
- ⏳ Node testnet i sinkronizuar për 24 orë: kërkon serverin.

## Faza 1: Zilja jashtë zinxhirit ✅

**Ndërtimi:** sparse Merkle tree kompakt, ku një nënpemë me një llogari të vetme
është vetë gjethja. Ndryshimet e çdo slot-i renditen dhe ndahen sipas bit-it në
çdo nivel, dhe degët e pavarura përpunohen paralelisht (rayon). Hash-et
rillogariten vetëm në rrugët që ndryshuan.

**Korrektësia:**
- Pema krahasohet me një implementim referencë të thjeshtë: 60 raunde ndryshimesh dhe fshirjesh të rastësishme, radhë të ndryshme, grupe të ndryshme. Root-i del gjithmonë i njëjtë.
- Ndjekja e fork-eve: bankat e braktisura, bankat Dead të zëvendësuara në të njëjtin slot, Rooted që kapërcen slote, Rooted i vjetër, ndryshime të vonuara.
- **Testi i plotë mbi validatorin:** pas rinisjes nga snapshot-i i slotit 100, root-i i rindërtuar nga e para (`d66fb37f…`) doli identik me root-in që Zilja kishte ndërtuar slot pas sloti, pra **0 mospërputhje**.

**Matjet** (`zilja-bench`, 2 thread-e):

| | 1M llogari, 5k/slot | 5M llogari, 20k/slot |
|---|---|---|
| Ndërtimi fillestar | 0.95 s | 4.98 s |
| Aplikimi p50 / p99 / max | 18.3 / 26.1 / 26.7 ms | 75.0 / 99.3 / 108.2 ms |
| Ndryshime në sekondë | 270 600 | 268 500 |
| Memoria | ~350 MB (367 B/llogari) | ~1.47 GB (307 B/llogari) |

Shpejtësia rritet me numrin e bërthamave (degët janë të pavarura). Në një server
mainnet me dhjetëra bërthama pritet të jetë disa herë më e lartë, por kjo
**duhet matur atje**.

## Faza 2: Dëftesat ✅

- `getAccountProof` dhe `getZiljaRoot` mbi JSON-RPC (`docs/SPEC.md`).
- Dëftesat e mospërfshirjes ("ky hambar nuk ekziston") funksionojnë.
- 10 000 llogari të rastësishme: 100% e dëftesave verifikohen, dhe asnjë kalon 10 KB.
- RPC me 1M llogari: 0.10 ms p50 mbi HTTP. *Shënim:* disa klientë HTTP shtojnë rreth 40 ms nga Nagle/delayed-ACK; kjo nuk vjen nga serveri.

## Faza 3: Verifikuesi ✅ (web) · ⏳ (aplikacion telefoni)

- `zilja-verifier` (Rust), `zilja-wasm` (64 KB) dhe `web/zilja-verifier.js` (5 KB, pa varësi).
- Të tre kalojnë **të njëjtët 110 vektorë**: 40 përfshirje, 20 mospërfshirje dhe 50 sulme (lamports të rreme, owner i rremë, vëlla i ndryshuar, "s'ekziston" i rremë, root i gabuar).
- Faqja demo `web/index.html` e bën verifikimin në shfletues dhe mund ta krahasojë root-in me një zile të dytë ose me një root të njohur.
- ⏳ Aplikacioni Android/iOS (UniFFI): logjika është gati, mungon vetëm lidhja.

## Faza 4: Firma e pleqve ✅ (jashtë protokollit) · ⏳ (në protokoll)

- Versioni jashtë protokollit: çdo operator që nis Ziles me `attest_keypair` firmos `"zilja/attest/v1" ‖ slot ‖ root`.
- `zilja-quorum` merr stake-un nga `getVoteAccounts`, kërkon firmat për të njëjtin slot nga disa zile dhe kontrollon kuorumin > 2/3.
- **Mbi validatorin e vërtetë:** identiteti i validatorit e firmosi root-in e slotit 101, me kuorum 100% të stake-ut.
- Teste: firmat e dyfishta, validatorët jashtë tabelës, slot ose root i gabuar, 60% < 2/3.
- ⏳ Fork-u i Agave me root-in në bank hash dhe me cluster me shumë validatorë: kërkon serverin dhe miratimin (SIMD).
- ⏳ Alpenglow/BLS: varet nga data kur del në testnet.

## Faza 5: Stresi dhe sulmet ✅ (simulim) · ⏳ (trafik mainnet)

`zilja-stream-bench` dërgon ngjarje si validatori: snapshot, banka, fork-e
(10% e sloteve), banka Dead (gjysma e fork-eve) dhe Rooted me vonesë 32 slote
që herë-herë kapërcen slote.

| | 100k llogari | 1M llogari |
|---|---|---|
| Slote × ndryshime | 200 × 2 000 | 300 × 5 000 |
| Fork-e / banka Dead | 14 / 8 | 34 / 19 |
| Ngjarje në sekondë | 436 000 | 391 000 |
| Root = modeli kanonik | ✅ | ✅ |

Sulmet e testuara: dëftesë e rreme, root i vjetër ose i gabuar, slot nga fork i
braktisur, llogari e fshirë, dëftesë e cunguar ose me bajte të tepërta, firma të
falsifikuara ose të dyfishta.

⏳ Ripërsëritja e ledger-it mainnet (ledger replay) kërkon serverin.

## Faza 6: Korrja ✅

Kodi është në këtë repo, me CI jeshile. Drafti i propozimit është te `docs/SIMD-DRAFT.md`.

## Pema në disk (RocksDB) ✅ e saktë · ⚠️ afër kufirit në makinë të vogël

**Si funksionon:** nivelet e sipërme të pemës (`cache_depth`, parazgjedhja 20)
jetojnë vetëm në RAM. Poshtë tyre, pema ruhet me **faqe**: çdo faqe mban 4 nivele
(deri në 15 nyje) dhe lexohet ose shkruhet si një hyrje e vetme. Çdo slot shkruhet
me **një `WriteBatch` atomik**. Kur hapet disku, nivelet e sipërme rillogariten nga
rrënjët e faqeve dhe krahasohen me root-in e ruajtur. Faqet që duhen merren rresht
pas rreshti, me kërkesa të grupuara (multi-get) në paralel.

**Kujtesa e sloteve: 32 slote (~13 s).** Fillimisht propozova 256, por matjet
treguan që historia e niveleve të sipërme kushton ~45 mijë hyrje për slot në RAM.
32 slote mjaftojnë për të krahasuar disa zile në të njëjtin slot dhe për vonesat e
RPC-së. Numri ndryshohet me `history_slots`. Për prova më të vjetra, banori ruan
vetë dëftesën, vulën dhe firmat.

**Rinisja:** nëse disku ka gjendje, zilja i thotë validatorit të mos dërgojë
snapshot-in dhe vazhdon menjëherë. Gjurma e gishtit të zinxhirit është hash-i i
bankës së slotit të fundit, marrë nga sysvar-i **SlotHashes**. Pas rinisjes, zilja
e kërkon atë hash në SlotHashes-in e parë që vjen. Nëse nuk e gjen (zinxhir tjetër
ose boshllëk > 512 slote), ndalon, nuk jep vula të gabuara dhe e fshin diskun që
nisja tjetër ta rindërtojë.

**Korrektësia:**
- Pema në disk = pema në RAM pas çdo slot, me cache 0, 4 dhe 20 nivele, me fshirje dhe me slote të kapërcyera. Edhe dëftesat janë bajt për bajt të njëjta.
- Dëftesat historike në çdo slot brenda kujtesës përputhen me kopjet e pemës në RAM të atyre sloteve.
- **Testi i "rrymës që ikën":** 12 herë SIGKILL në momente të rastësishme gjatë shkrimit. Çdo herë disku mbeti i plotë (root-i = root-i i rillogaritur nga gjethet) dhe asnjë slot i konfirmuar nuk humbi.
- **Mbi validatorin e vërtetë** (`ZILJA_DB=1 scripts/e2e-test-validator.sh`):
  - pas rinisjes, zilja vazhdoi nga disku pa snapshot, me të njëjtin root;
  - gjurma SlotHashes u konfirmua;
  - dëftesa historike tregoi 1.75 SOL në slotin para një transferimi, kurse tani llogaria ka 2.25 SOL, dhe të dyja u verifikuan;
  - me `--reset` (zinxhir i ri) dhe disk të vjetër, zilja ndaloi vetë.

**Matjet** (2 bërthama, 7 GB RAM, disk virtual; 5 000 ndryshime për slot; kujtesa 32 slote):

| | RAM | Disk, cache 20 | Disk, cache 22 |
|---|---|---|---|
| **1M llogari:** përditësimi p50 / p99 | 18 / 26 ms | 164 / 230 ms | – |
| **10M llogari:** përditësimi p50 / p99 | **22 / 32 ms** | 406 / 575 ms | 299 / 574 ms |
| 10M: RAM i procesit | 2.9 GB (vetëm pema) | 1.7 GB* | 2.5 GB* |
| 10M: disku | – | 2.2 GB (229 B/llogari) | 2.0 GB (200 B/llogari) |
| 10M: ndërtimi nga snapshot-i | 11 s | 88 s | 95 s |
| 10M: dëftesë e tanishme / historike | – | 0.08 / 0.04 ms | 0.06 / 0.03 ms |

\* Përfshin cache-në e RocksDB (256 MB) dhe listën e çelësave të vetë testit (~320 MB).

**Faqet** (10M llogari, cache 20, 5 000 ndryshime për slot, e njëjta makinë):

| | Nyje një nga një | Faqe me 4 nivele |
|---|---|---|
| Lexime nga disku për slot | 27 300 | **9 000** (3× më pak) |
| Koha e leximit | ~250 ms | ~100 ms |
| Operacione shkrimi për slot | 55 000 | **17 600** |
| Përditësimi p50 / p99 | 406 / 575 ms | **~315 / 420–620 ms** |
| Disku | 229 B/llogari | 235 B/llogari |
| 1M llogari, p50 | 164 ms | 146 ms |

Provova edhe cache 16 dhe 12: RAM-i bie (65 mijë dhe 4 mijë nyje), por leximet
rriten dhe përditësimi ngadalësohet (389 dhe 466 ms). Thellësia 20 mbetet më e mira.
Koha që mbetet (~100 ms lexim, ~55 ms pemë, ~60 ms përgatitje faqesh, ~30–70 ms
shkrim) është kryesisht CPU. Në këtë makinë me 2 bërthama, kompaktimi i RocksDB në
sfond konkurron për të njëjtat bërthama, prandaj p99 ka zhurmë.

**Përfundimi i ndershëm:**
- Disku është **i saktë dhe i sigurt**. Me faqet, në këtë makinë është ~14 herë më i ngadaltë se RAM-i (315 ms kundrejt 22 ms me 10M llogari). Me p50 qëndron brenda 400 ms, por p99 ndonjëherë e kalon.
- Para faqeve, kufiri ishin ~5 lexime të rastësishme për çdo ndryshim. Tani janë ~1.8, dhe pjesa më e madhe e kohës është CPU, që shpejtohet me më shumë bërthama.
- Me 10M llogari, disku kursen pak RAM. Kursimi i madh del në shkallë mainnet: pema në RAM rritet ~300 B për llogari (~30 GB për 100M), kurse me diskun RAM-i rritet pak (cache-ja është ~2^`cache_depth` nyje).
- **Hapi tjetër:** matja në një server të vërtetë (NVMe, 16+ bërthama, cache më e madhe e RocksDB) me 100M+ llogari. Kjo s'mund të bëhet në këtë makinë.
- **Deri atëherë:** në një server me RAM të mjaftueshëm, mënyra RAM mbetet më e mira. Mënyra disk vlen kur RAM-i është i kufizuar dhe ngarkesa më e vogël.

## SDK për kuletat (`sdk/js`, paketa `zilja-verify`) ✅

- Një thirrje e vetme, `ZiljaClient.verifyAccount(pubkey)`, që:
  - verifikon dëftesën;
  - krahason root-in mes disa zileve në të njëjtin slot;
  - sipas dëshirës, kontrollon kuorumin e validatorëve (> 2/3 e stake-ut);
  - krahason `lamports` dhe hash-in e të dhënave me RPC-në e zakonshme.
- Tre nivele besimi, `single` → `agreement` → `quorum`, me `minTrust` për të kërkuar një nivel minimal. Dy zile që japin root të ndryshëm japin `invalid`.
- Pa varësi (vetëm WebCrypto, edhe Ed25519), tipe TypeScript të kontrolluara me `strict`. *(A25: shifra e dikurshme "8.7 KB" s'e përshkruan paketën aktuale; në `5b079ed` tarball-i npm është 21.0 kB dhe bundle-i i minifikuar 6.3 KB gzip. Madhësia e çdo release-i jepet te `MANIFEST.json`, me komandën dhe commit-in.)*
- Teste: 110 vektorë dëftesash dhe 8 vektorë firmash, të njëjtë me Rust bajt për bajt, plus 12 teste të klientit me zile të simuluara. Testi mbi validatorin e vërtetë: `single`, `quorum` (100% e stake-ut), dhe `minTrust=agreement` me një zile, që refuzohet siç duhet.
- CI kontrollon që vektorët në repo janë pikërisht ata që gjeneron Rust.
- **Kufizim:** me `quorum`, tabela e stake-ut i besohet `solanaRpc`-së. Light client-i me checkpoint është shtyrë për më vonë.

## Pas auditimit (v0.2.0) ✅

Auditimi i `f59be06` gjeti 2 gjetje kritike, 3 të larta, 8 mesatare, 9 të ulëta dhe 6
informative. Të gjitha u rregulluan në gjashtë faza, secila me CI jeshile përfshirë e2e mbi
validatorin. Hollësitë janë te [`AUDIT-FIXES.md`](AUDIT-FIXES.md). Numrat që ndryshuan:

| Matja | Para | Pas |
|---|---|---|
| Rinisje me slote të ripërsëritura (e2e) | 0.123 SOL humbën (F-01) | zilja = RPC |
| Testi model i tracker-it (10 000 histori) | 7–25% gabim | 0 |
| Çelësa "binjakë" (500 ndryshime) | 545 ms, 31 179 lexime, thellësi 256 | 13 ms, 1 122 lexime, thellësi 16 |
| RAM, 2M llogari, 5000 ndryshime/slot | 14.5 ms p50 | 11.5 ms p50 |
| Disk, 2M llogari, 3000 ndryshime/slot (2 vCPU) | 91.8 ms p50 | 93.8 ms p50 (zhurmë); dëftesat s'presin më zbatimin |
| Fuzz (dëftesa, disk, RPC, JS = Rust) | — | 1 orë pa gjetje, pas 4 rregullimeve të kodimit kanonik |

## Çfarë mbetet (kërkon një server të vërtetë)

1. **Node mainnet ose testnet me Ziles për 72 orë:** CPU shtesë, numri real i ndryshimeve për slot, koha e nisjes nga snapshot-i i plotë.
2. **Pema në disk më e shpejtë:** faqet me 4 nivele dhe matja në NVMe (shih seksionin më lart).
3. **Fork-u i Agave** me root-in në bank hash, dhe matja me cluster me shumë validatorë.
4. **Aplikacioni i telefonit** dhe matja në një Android mesatar.
5. **Rishikimi** nga një ekip validatorësh (Anza ose Firedancer).

## Si t'i përsëritësh matjet

```bash
cargo run -p zilja-bench --release -- 1000000 200 5000
cargo run -p zilja-bench --release --bin zilja-rpc-bench -- 1000000 5000
cargo run -p zilja-bench --release --bin zilja-stream-bench -- 1000000 300 5000
cargo run -p zilja-bench --release --bin zilja-disk-bench -- 10000000 100 5000 /tmp/zdb 32
cargo run -p zilja-bench --release --bin zilja-crash-test -- 12
scripts/e2e-test-validator.sh /rruga/te/solana-release/bin
ZILJA_DB=1 scripts/e2e-test-validator.sh /rruga/te/solana-release/bin
```
