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

## Auditimi i dytë: gjetjet A01–A26 (19 nëntor 2026)

Auditimi u bë mbi `46b7a60` kundër `anza-xyz/agave@a203300` (`v4.3.2`). Çdo gjetje ka PR-në e vet:
- testi i PR-së dështon mbi kodin e mëparshëm dhe kalon me rregullimin;
- bashkimi te `main` u bë vetëm me CI jeshile, përfshi e2e mbi Agave 4.3.2 (RAM, RocksDB i vjetër, pages).

| ID | Rëndësia | Rregullimi | PR | Prova kryesore |
|---|---|---|---|---|
| A01 | E lartë | `entry_notifications_enabled = true`: UpdateParent arrin te tracker-i | #9 | porta e Agave e kopjuar; e2e: ~98 entry/slot nga Agave real |
| A02 | E lartë | ringarkimi / `loadPlugin` në mes të punës s'aplikon banka; genesis vetëm deri te sloti 0 | #16 | `reload_never_applies_banks`, `admin_load_mid_run_is_not_a_genesis_boot`; e2e RAM |
| A03 | E lartë | ditari i firmave v2: firma origjinale, vërtetim strikt, `floor`, migrim pa besim | #14 | `tampered_or_incomplete_records_are_refused`, `v1_log_migrates_without_trusting_history` |
| A04 | E lartë | kornizë e plotë me CRC të gabuar = dëmtim | #10 | `complete_last_frame_with_bad_crc_is_refused` |
| A05 | E lartë | pa checkpoint: fshirje vetëm me `BUILDING` ose dosje bosh | #10 | `missing_checkpoint_with_existing_pages_is_refused` |
| A06 | E lartë | `INVALID` i palexueshëm refuzon hapjen | #10 | `unreadable_invalid_marker_is_refused` |
| A07 | E lartë | ndalimi i serializuar me commit/firmë | #11 | `stop_from_rpc_freezes_queued_work` (80/80) |
| A08 | E lartë | pool Rayon i instancës, `join` në unload | #12, #18 | `unload_leaves_no_threads_behind`; e2e: `zilja-rayon` 2/2 |
| A09 | E lartë | `scripts/release.sh`: dy ndërtime identike, `MANIFEST.json` me `rustc -vV` | #25 | CI "Release"; hash-e identike |
| A10 | E lartë | kuorumi: pamje koherente e epokës, të dy kufijtë, etiketë "pamje e RPC-së" | #17 | `quorum_counts_only_inside_the_stake_tables_epoch` |
| A11 | E lartë | numra të saktë: JSON pa humbje në JS, u64 strikt, mbledhje e kontrolluar | #17 | `attest.test.mjs` A11, `quorum_rejects_inexact_stake_numbers` |
| A12 | E lartë | slot mbi referencën → `ahead`, jo `fresh` | #20 | vektorët + `slot_ahead_of_finalized_reference_is_not_fresh` |
| A13 | E lartë | krahasimi i kërkuar që s'u krye → `data_unavailable` | #20 | `requested_data_check_that_did_not_run_fails` |
| A14 | E lartë | referencë `{root, slot, complete}`; agreement kërkon plotësi për mungesë | #20 | vektorët `anchor_*`, `absence_agreeing_source_incomplete` |
| A15 | Mesatare | identiteti numërohet pas firmës së vlefshme | #17 | `invalid_signature_does_not_shadow_valid_one` (Rust, JS) |
| A16 | Mesatare | `attestMessage` me `toU64` | #17 | `attest.test.mjs` A16 |
| A17 | Mesatare | afat monoton për kërkesë; kufi madhësie në SDK | #23 | `slow_drip_client_is_cut_at_the_request_deadline` |
| A18 | Mesatare | buxhet për bankat/ndryshimet në pritje | #21 | `pending_state_has_a_memory_budget`; 212 B, 218 ns për ndryshim |
| A19 | Mesatare | `-32008` mbi majë; `accounts` vetëm për slotin aktual; numri i parametrave | #26 | `history_contract_matches_the_spec` |
| A20 | E ulët | hex vetëm me shkronja të vogla kudo; rregullat e bitmap-it në SPEC | #19 | `wire-vectors.json` `*_uppercase` |
| A21 | E lartë (lëshimi) | release vetëm `pages`, pa RocksDB/libclang; konfigurimet e vjetra refuzohen | #22 | CI "Release"; `cargo tree` 0 rreshta; e2e hapi 0 |
| A22 | Mesatare | bench dhe skripti fshijnë vetëm atë që krijuan | #25 | `disk_bench_never_deletes_what_it_did_not_create` |
| A23 | E ulët | LICENSE në repo, npm dhe release | #25 | `npm pack`, `release.sh` |
| A24 | E lartë | gjurmë vetëm me një bankë në slot; lidhja vetëm me slot refuzohet | #21 | `duplicate_banks_at_rooted_slot_are_not_certified` |
| A25 | Informuese | madhësitë e SDK-së të matura dhe të emërtuara | #25 | `MANIFEST.json` `sdk_js` |
| A26 | E lartë | kovat e staging-ut me numër + SHA-256 | #10 | `staging_buckets_are_checked_end_to_end` |

**Ndryshimet që thyejnë përputhshmërinë:**
- `reloadPlugin` dhe `loadPlugin` në mes të punës ndalojnë zilen (A02);
- release-i kërkon `tree_engine: "pages"` (A21);
- ditari i firmave v2 migron vetë dhe s'shërben historinë v1 (A03);
- `-32008` dhe `getZiljaRoot` historik pa `accounts` (A19);
- hex vetëm me shkronja të vogla (A20);
- klienti: `root` mund të jetë objekt, statuset e reja `ahead` / `unverifiable` / `unavailable`, `ok = false` kur kontrolli i kërkuar s'kryhet (A12–A14).

**Kufizimet që mbeten (s'zgjidhen dot vetëm nga ky repo):**
- **Pesha e kuorumit** është pamja e RPC-së, jo tabela e konsensusit e epokës (A10). Light client-i për `epoch_stakes` është punë e ardhshme.
- **Lidhja `BankId → bank_hash`** s'jepet nga Agave, ndaj një bllok i dyfishtë i rrënjosur ndalon zilen (A24).
- **Shkrimet e dykuptimta të diskut** refuzohen. S'ka rikuperim automatik pa një kufi të pavarur commit-i (A04).
- **Prova përfundimtare** mbetet (kushti 4 i auditimit): validator i izoluar me build-in e shpërndarjes, ndërprerje reale (ENOSPC/EIO, ikje rryme, ext4/XFS), kuorum me shumë validatorë mbi kufij epokash dhe ngarkesë p99.

## Rishikimi i katërt (v0.2.x): 5 gjetje mbi main

| # | Gjetja | Statusi | Prova |
|---|---|---|---|
| R4-1 | Quorum shadowing (identiteti shënohet para firmës) | Ishte rregulluar me A15 (#17) | `invalid_signature_does_not_shadow_valid_one` (Rust, JS) |
| R4-2 | Autorizimi i `signer`-it pa afat dhe pa revokim | **Rregulluar:** `zilja/authorize/v2` me `[from_epoch, until_epoch)`, maksimumi 15 epoka; v1 refuzohet; rinovim pa rinisje | `authorization_expires_and_cannot_be_extended`, `v1_authorization_is_rejected`, `authorization_file_is_reloaded_without_restart`, vektorët `dedicated_signers_*`, e2e hapi 6 |
| R4-3 | Slowloris me 1 bajt / 4 s | Ishte rregulluar me A17 (#23): afat i plotë për kërkesë | `slow_drip_client_is_cut_at_the_request_deadline` |
| R4-4 | Kufiri i sipërm i epokës | Ishte rregulluar me A10 (#17): të dy kufijtë + epoka para/pas | `StakeView::covers`, `client.js` |
| R4-5 | `StakeTable` me dyfishe, `MemTree::prove_at`, `accounts` historik, numri i parametrave | `accounts` dhe parametrat: A19 (#26). Mbledhja: `checked_add` qysh në A11. `StakeTable` dhe `MemTree`: PR i veçantë | — |

**Ndryshim që thyen përputhshmërinë:** `authorization` në `getZiljaAttestation` është objekt
`{fromEpoch, untilEpoch, signature}`; skedari i `zilja-authorize` ka `version: 2`; `zilja-authorize`
kërkon `from_epoch`. JS: `verifyAttestation` / `verifyQuorum` marrin `epoch` si argument të dytë.
