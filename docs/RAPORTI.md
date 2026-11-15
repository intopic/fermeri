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
| Memoria shtesë | < 10% | **300–370 bajt për llogari** në RAM | ❌ për mainnet vetëm në RAM, duhet pemë në disk |
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

## Çfarë mbetet (kërkon një server të vërtetë)

1. **Node mainnet ose testnet me Ziles për 72 orë:** CPU shtesë, numri real i ndryshimeve për slot, koha e nisjes nga snapshot-i i plotë.
2. **Pemë në disk:** me ~300–370 bajt për llogari, çdo 100 milionë llogari kërkojnë ~30–37 GB RAM. Për mainnet, nyjet e poshtme duhet të ruhen në disk (p.sh. RocksDB) dhe në RAM vetëm nivelet e sipërme.
3. **Fork-u i Agave** me root-in në bank hash, dhe matja me cluster me shumë validatorë.
4. **Aplikacioni i telefonit** dhe matja në një Android mesatar.
5. **Rishikimi** nga një ekip validatorësh (Anza ose Firedancer).

## Si t'i përsëritësh matjet

```bash
cargo run -p zilja-bench --release -- 1000000 200 5000
cargo run -p zilja-bench --release --bin zilja-rpc-bench -- 1000000 5000
cargo run -p zilja-bench --release --bin zilja-stream-bench -- 1000000 300 5000
scripts/e2e-test-validator.sh /rruga/te/solana-release/bin
```
