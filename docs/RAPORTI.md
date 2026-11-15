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

## Pema në disk (RocksDB) ✅ e saktë · ⚠️ ende e ngadaltë në makinë të vogël

**Si funksionon:** nivelet e sipërme të pemës (`cache_depth`, parazgjedhja 20)
jetojnë vetëm në RAM. Në disk shkruhen gjethet (me meta-n e llogarisë brenda) dhe
nyjet poshtë cache-së. Çdo slot shkruhet me **një `WriteBatch` atomik**. Kur hapet
disku, nivelet e sipërme rillogariten nga shtresa e poshtme dhe krahasohen me
root-in e ruajtur. Nyjet e lexuara nga disku merren nivel pas niveli, me kërkesa të
grupuara (multi-get) në paralel.

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

**Përfundimi i ndershëm:**
- Disku është **i saktë dhe i sigurt**, por në këtë makinë është 13–18 herë më i ngadaltë se RAM-i. Me 10M llogari kalon kufirin e 400 ms të një sloti.
- Kufiri vjen nga ~5 lexime të rastësishme nga disku për çdo ndryshim, me ~10 µs secili në një disk virtual me 2 bërthama.
- Me 10M llogari, disku kursen pak RAM. Kursimi i madh del në shkallë mainnet: pema në RAM rritet ~300 B për llogari (~30 GB për 100M), kurse me diskun RAM-i rritet pak (cache-ja është ~2^`cache_depth` nyje).
- **Hapi tjetër:** "faqet", pra 4 nivele të pemës në një hyrje të vetme në disk. Kështu leximet për ndryshim bien nga ~5 në ~1–2, dhe pastaj matet në një server me NVMe dhe shumë bërthama.
- **Deri atëherë:** në një server me RAM të mjaftueshëm, mënyra RAM mbetet më e mira. Mënyra disk vlen kur RAM-i është i kufizuar dhe ngarkesa më e vogël.

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
