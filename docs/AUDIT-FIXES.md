# Rregullimet pas auditimit (v0.2.0)

Auditimi u bë mbi commit-in `f59be06` (15 nëntor 2026). Rregullimet janë bërë në gjashtë faza,
ku secila u bashkua te `main` vetëm me CI jeshile, përfshirë testin e plotë mbi
`solana-test-validator` në RAM dhe në disk. Diff-i i plotë: `git diff f59be06..v0.2.0`.

| Faza | Commit | Përmbajtja |
|---|---|---|
| 0 | `ddadabb` | Testet e audituesit në repo, testi model i tracker-it, e2e me rinisje, sonda |
| 1 | `df5d59a` | Vula e saktë pas rinisjes |
| 2 | `687f5d1` | SDK dhe formati në rrjet |
| 3 | `6189a82` | Ndalim i pastër kur diçka prishet |
| 4 | `713696a` | Formati v2 |
| 5 | `90e050f` | Thjeshtime, shpejtësi, fuzz |
| 6 | (ky) | Dokumentet, versioni |

## Gjetjet

| ID | Rregullimi | Testi që e vërteton |
|---|---|---|
| **F-01** | Prindi i çdo banke nga `SlotHashes` (vjen edhe për slotet e ripërsëritura). Zinxhiri duhet të mbarojë saktësisht te sloti i fundit i rrënjosur, përndryshe `Broken`. | `audit_poc::f01_*` (2); `tracker_model::model_startup_ledger_replay`, `model_realistic_restart` (10 000 histori); `tracker::startup_replay_without_statuses_uses_slot_hashes_parents`; e2e hapi 4b (transferim i ripërsëritur, RPC = zilja) dhe hapi 5 (`roots.log`) me `STRICT_F01=1` |
| **F-02** | Në snapshot fiton versioni me slotin më të madh, në RAM dhe në disk. Varret mbahen deri në `finish_snapshot`. | `audit_poc::f02_*`; `disk::snapshot_newest_version_wins_on_disk_and_in_ram`; `tracker_model::model_snapshot_duplicate_versions` |
| **F-03** | SDK: u64 i rreptë në [0, 2^64), `executable` boolean, hex/base58 të rreptë; kthen vlerat e kontrolluara. WASM merr numrat si tekst. | `sdk/js/test/audit.test.mjs`; `wire.test.mjs` (36 vektorë); `zilja-wasm::passes_wire_vectors`, `strict_inputs` |
| **F-04** | Çdo fëmijë që prindi e shënon jo-bosh duhet të lexohet dhe të përputhet me hash-in e prindit, në zbatim dhe në dëftesa; përndryshe gabim dhe ndalim. | `store audit_poc::f04_*`; `disk::injected_read_faults_never_produce_a_wrong_root` |
| **F-05** | Pas ndalimit hidhen bankat dhe hartat, dhe plugin-i ndalon së dërguari. Kanali ka kufi 1M; kur mbushet pas nisjes, zilja ndalon. | `audit_poc::f05_*` |
| **F-06** | Server HTTP i ri: trupi ≤ 64 KB, 5 s timeout, 8 punëtorë, `/health` nga atomike, `/metrics` në port privat, CORS i fikur si parazgjedhje. | `audit_poc::f06_*`; e2e: `/metrics` = 404 në portin publik |
| **F-07** | `on_unload` i ndal serverët dhe i pret thread-et. | e2e hapi 7 (RAM): `reloadPlugin` me admin RPC, portet rimerren dhe validatori vazhdon |
| **F-08** | `lamports`/`dataLen` dalin si tekst. Një numër JSON pranohet vetëm ≤ 2^53−1 (`zilja_verifier::wire`). | `wire.test.mjs`; `rpc::end_to_end_service_and_jsonrpc` |
| **F-09** | Pema mbi `tree_key = SHA256("zilja/key/v2" ‖ pubkey)`, dëftesa v2. Stack 64 MB për punëtorin. | `store audit_poc::f09_deep_twin_keys_amplify_disk_work`, tani me assert: 1× punë, thellësi 16 në vend të 256 |
| **F-10** | Firmat v2 me `genesis_hash`; çelës i veçantë i autorizuar nga identiteti (`zilja-authorize`). Firmoset vetëm pema e plotë pas konfirmimit të zinxhirit, kurrë dy root-e për një slot (skedar me fsync). | `attest::*` (genesis tjetër, autorizim); `attester::never_signs_two_roots_for_one_slot_even_after_restart`; e2e hapi 6 (çelës i veçantë, `--genesis` tjetër refuzohet, `attest_refused_total = 0` pas rinisjes, pema jo e plotë s'firmos) |
| **F-11** | Gjurma = `(slot, bank_hash)` e vetë slotit të rrënjosur, nga SlotHashes i një fëmije. Asnjë slot s'aplikohet pas rinisjes pa e konfirmuar zinxhirin. | `tracker::resume_unverified_never_applies`, `fingerprint_from_slot_hashes_roundtrip`; e2e hapi 7 (disk, `--reset`) |
| **F-12** | `StateTree` kthen `Result`, pa `expect`; `setup_logger`; `/health` 503 kur s'është gati ose ka ndaluar; `zilja_last_root_age_seconds`. | `disk::injected_read_faults_*`; e2e: `/health` = 503 pas ndalimit |
| **F-13** | Asnjë fshirje automatike: shënimi `invalid` rindërton në nisjen tjetër. Versioni i panjohur refuzohet. `snapshot_on_resume`. | `disk_service::resume_history_and_wrong_chain`, `unknown_state_version_is_refused_not_wiped`; e2e hapi 8 (rindërtim vetë) |
| L-01 | `minTrust` i paracaktuar `agreement`; faqja demo s'thotë "e verifikuar" kur root-i vjen nga i njëjti server. | `client.test.mjs` 5; e2e |
| L-02 | Pema jo e plotë s'jep dëftesa mospërfshirjeje (`-32006`); SDK s'i pranon. | `client.test.mjs` 6c; e2e hapi 3 |
| L-03 | Hex/base58 të rreptë; `dataMatches` krahason bajte. | `audit.test.mjs` |
| L-04 | Parametrat e gabuar → `-32602`; SDK kontrollon pubkey/slot e përgjigjes; adresat e dyfishta s'numërohen. | `rpc::end_to_end_*`; `client.test.mjs` 6, 6b |
| L-05 | Dëftesa historike etiketohet me slotin e rrënjosur që u përdor; `getZiljaRoot [slot]`. | `rpc::end_to_end_*` |
| L-06 | `notify_entry_update_parent` → `BankCleared`. | (Alpenglow s'është aktiv në 4.3.2 test-validator) |
| L-07 | SDK kthen `data.bytes` vetëm kur të dhënat përputhen me vulën. | `index.d.ts`, `client.js` |
| L-08 | `cache_depth` 12–24, kufij për `history_slots`/`block_cache_mb`. | `Config::validate` |
| L-09 | CLI: 30 s timeout, përgjigje ≤ 64 MB. | — |
| I-01 | Fjalët e sakta për ruajtjen + `sync_every_slots`. | — |
| I-02 | Firmat e fundit rikthehen pas rinisjes; `roots.log` rrotullohet mbi 64 MB. | `attester::*` |
| I-03 | `counter` (me `_total`) / `gauge`. | — |
| I-04 | SPEC v2: rrugët e vektorëve, `rent_epoch`, mospërfshirja vetëm për pemë të plota. | — |
| I-05 | Faqja demo plotëson nga URL-ja vetëm pubkey-in. | — |
| I-06 | `rust-toolchain.toml` 1.97.1. | CI |

## Propozimet (Simpler / Safer / Faster)

| ID | Vendimi |
|---|---|
| Simpler-1, 2 | ✅ Prindi nga SlotHashes; një gjurmë e vetme, e tipizuar. Hiqen `Event::Block`, blockhash-et dhe formati `"bank:"`. |
| Simpler-3 | ❌ **Nuk u mor, pas matjes.** Faqet nga thellësia 0 do të shtonin ~3000 shkrime faqesh + ~3000 arkivime për slot mbi ~7300 ops të sotme: ~+25% kohë zbatimi. Thjeshtimi s'e vlen sot. |
| Simpler-4 (V7) | ❌ **Nuk u mor, pas matjes** (`store/tests/seek_bench.rs`). `seek_for_prev` mbi çelësa me version është 1.5× më i ngadaltë se `get` (1M çelësa: 8.3 µs kundrejt 5.4 µs). Zbatimi bën ~3700 lexime për slot, dhe historia sot paguan `seek` vetëm për dëftesat historike. |
| Simpler-5 | ✅ `getZiljaRoot [slot]` |
| Simpler-6 | ✅ Hiqen `start`/`start_with`, `web/zilja-verifier.js` dhe leximi i `ZILJA_PROFILE` për çdo slot. |
| Simpler-7 | ✅ drop/create CF në rindërtim |
| Safer-1 | ✅ `halt()` i vetëm |
| Safer-2 | ✅ testi model |
| Safer-3 | ✅ `zilja-fuzz` + `fuzz-diff.mjs`. Gjeti dhe u rregulluan: bit-e mbushëse në bitmap-in e dëftesës, vëlla "jo-bosh" me vlerë bosh, bit-i i 16-të i faqes, `kind`/`bool` të panjohur në disk. Pas rregullimit: 1 orë pa gjetje. |
| Safer-4 | ✅ `wire-vectors.json` nga përgjigje të vërteta |
| Safer-5 | ✅ e2e me ndryshim llogarie mes snapshot-it dhe ndalimit |
| Safer-6 | ✅ lexime që dështojnë me qëllim; vrasje gjatë ndërtimit nga snapshot-i |
| Faster-1 | ✅ (disk) `prepare_slot` nën kyçin e leximit, `commit_slot` i shkurtër. RAM: zbatimi (~11 ms) mbetet nën kyçin e shkrimit. |
| Faster-2 | ✅ RAM: 14.5 → 11.5 ms p50 (2M llogari, 5000/slot) |
| Faster-3 | ✅ zilet pyeten paralelisht |
| Faster-4 | ✅ asnjë punë pas ndalimit. Slotet pa ndryshime s'ekzistojnë në praktikë, sepse çdo slot shkruan sysvar-ët. |

## Ndryshimet që thyejnë përputhshmërinë

- Formati v2: `tree_key`, dëftesa `version = 2`, firmat `zilja/attest/v2`. Root-et e v1 nuk krahasohen me v2.
- Gjendja në disk `v4`: një disk i v0.1 refuzohet, dhe operatori e fshin me dorë.
- `lamports`/`dataLen` në JSON janë tekst.
- `minTrust` i paracaktuar `agreement`.
- `/metrics` në `metrics_bind` (127.0.0.1:7798).
- Konfigurimi: `attest_keypair` kërkon `genesis_hash` ose `genesis_path`.

## Pyetjet e hapura, me përgjigje

- **F-01/F-02 në një validator të gjallë:** po. E2e e riprodhoi F-01 (0.123 SOL humbën pas rinisjes) para Fazës 1 dhe s'e riprodhon më.
- **`on_unload` në ndalim normal (SIGTERM):** nuk thirret. Gjendja në disk është e sigurt ndaj ndalimit pa paralajmërim.
- **End-of-startup në nisje nga genesis:** nuk vjen; tracker-i e trajton.
- **Statuse për slotet e ripërsëritura:** asnjë (73 banka në matje), vetëm shkrime llogarish dhe SlotHashes.
