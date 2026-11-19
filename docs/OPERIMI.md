# Zilja: si operohet

Ky dokument tregon si ta mbash Ziles në punë në një server të vërtetë. Gjetjet e auditimit
që lidhen me secilën pjesë janë shënuar në kllapa.

## Portet

| Porti (parazgjedhja) | Çfarë shërben | Kujt i hapet |
|---|---|---|
| `bind` = `127.0.0.1:7799` | JSON-RPC (`POST`) dhe `GET /health` | publikut, **vetëm pas një proxy** |
| `metrics_bind` = `127.0.0.1:7798` | `GET /metrics` (Prometheus) dhe `/health` | vetëm rrjetit privat (F-06) |

`/health` kthen `200 ok` vetëm kur zilja jep dëftesa. Kthen `503 starting` gjatë ndërtimit
të pemës dhe `503 halted` pasi ka ndaluar (F-12). Load balancer-i duhet ta përdorë këtë.

## RPC pas proxy-t (F-06)

Serveri i Ziles ka kufij të vetët:
- trupi i kërkesës deri në 64 KB;
- 5 sekonda kohë maksimale leximi dhe shkrimi;
- 8 punëtorë, ndaj një klient i ngadaltë zë vetëm një punëtor për pak sekonda.

Nuk ka TLS, kufij për IP ose cache. Për to, vendose pas një proxy. Shembull me nginx:

```nginx
limit_req_zone $binary_remote_addr zone=zilja:10m rate=20r/s;
server {
  listen 443 ssl http2;
  server_name zilja.example.org;
  client_max_body_size 64k;
  client_body_timeout 5s;
  location / {
    limit_req zone=zilja burst=40 nodelay;
    proxy_pass http://127.0.0.1:7799;
    proxy_read_timeout 10s;
    # CORS për kuletat në shfletues (në vend të "cors": true në Ziles):
    add_header Access-Control-Allow-Origin * always;
    add_header Access-Control-Allow-Headers content-type always;
  }
  location /metrics { return 404; }
}
```

`"cors": true` në konfigurimin e Ziles i shton vetë kokat CORS. Përdore vetëm për faqen demo
në një makinë lokale.

## Kur zilja ndalon

Zilja ndalon sa herë nuk është e sigurt që vula është e saktë. Disa raste:
- një boshllëk sloteve pas rinisjes;
- zinxhir tjetër;
- disk i dëmtuar ose i palexueshëm (F-04);
- kanali i ngjarjeve i mbushur (F-05).

Kur ndalon:
1. RPC-ja refuzon çdo kërkesë me `-32003 zilja ndaloi: <arsyeja>`, dhe `/health` kthen `503`.
2. Plugin-i ndalon së punuari. Validatori vazhdon normalisht dhe nuk e ndien.
3. Pema në disk **nuk fshihet**, por shënohet `invalid` (F-13). Në nisjen e ardhshme të
   validatorit rindërtohet nga snapshot-i, pa asnjë veprim me dorë.

Pra, pas një ndalimi mjafton **një rinisje** e validatorit.

### Fazat dhe `/health` (P-08, P-10)

| Faza (`zilja_phase`) | Kuptimi | `/health` | RPC |
|---|---|---|---|
| `starting` (0) | pema po ndërtohet nga snapshot-i ose genesis-i | 503 `starting` | `-32002` |
| `recovered` (1) | pema u hap nga disku, por zinxhiri s'e ka konfirmuar ende (SlotHashes) | 503 `recovered` | `-32002` |
| `serving` (2) | jep dëftesa | 200 `ok`; 503 `stale` kur root-i është më i vjetër se `max_root_age_secs` | normal |
| `halted` (3) | ndaloi (shih më sipër) | 503 `halted` | `-32003` |

- Pas rinisjes, gjendja nga disku **s'shërbehet** derisa banka e parë e re ta konfirmojë. Kjo
  zgjat zakonisht pak sekonda.
- `max_root_age_secs` (parazgjedhja 60, 0 = pa kontroll): rrënjosja në Solana ndodh çdo
  ~0.4 s, ndaj 60 s pa root të ri do të thotë që zilja ose validatori kanë mbetur pas. Gjatë kësaj
  kohe RPC-ja përgjigjet ende, me slotin në çdo përgjigje, por `/health` = 503 i largon
  klientët nga kjo zile.
- **Punëtori bie** (panic) ose mbaron papritur: kalon nëpër të njëjtën rrugë ndalimi
  (`stop_serving`), dhe `/health` = 503. Kur kanali mbyllet (`Disconnected`), callback-et e
  validatorit s'presin kurrë: vetëm shënojnë ndalimin.
- **Gabim ruajtjeje gjatë një dëftese** (I/O ose dëmtim): zilja **ndalon** së shërbyeri,
  sepse s'garanton më korrektësinë, dhe pema shënohet për rindërtim.

### Kodet e gabimeve JSON-RPC (P-09)

| Kodi | Kuptimi |
|---|---|
| `-32602` | parametra të pavlefshëm (vetëm kjo) |
| `-32002` | s'është gati (`starting` ose `recovered`) |
| `-32003` | ndaloi; arsyeja në mesazh |
| `-32004` | kjo zile s'firmos |
| `-32005` | s'ka root/firmë për këtë slot |
| `-32006` | pema s'mbulon çdo llogari: s'jep dëftesa mospërfshirjeje |
| `-32007` | sloti është jashtë historisë së pemës |
| `-32010` | gabim i ruajtjes: zilja ndalon |

### Snapshot i shkarkuar (dy rinisje)

Kur pema në disk vazhdon, njoftimet e snapshot-it fiken, sepse nisja është shumë më e shpejtë
pa to. Rasti i veçantë: e ndalove validatorin gjatë, ose shkarkove një snapshot më të ri se
pema. Kështu del një boshllëk:
- nisja e parë e zbulon boshllëkun, ndalon dhe e shënon pemën;
- nisja e dytë e rindërton nga snapshot-i.

Kur e di që po vendos një snapshot të ri, vendos `"snapshot_on_resume": true`. Zilja i
pranon njoftimet edhe kur vazhdon nga disku, dhe nëse snapshot-i është më i ri se pema, ndalon
që në nisjen e parë. Rindërtimi bëhet në nisjen tjetër.

### Gjendje me version të panjohur

Nëse dosja `db_path` është krijuar nga një version tjetër i Ziles, plugin-i refuzon të
ngarkohet dhe nuk e fshin. Fshije dosjen me dorë, ose përdor versionin e duhur.

**Ndalimi është i serializuar me commit-in (A07).** `stop_serving`, nga cilido thread që thirret
(RPC, punëtori, rënia), e merr vendimin nën kyçin e shkrimit të gjendjes. Nën të njëjtin kyç
bëhen commit-i i slotit, publikimi i root-it, firmosja dhe kalimi në `serving`. Kur ndalimi
kthehet, puna e pranuar në radhë s'bëhet më commit, s'firmoset dhe s'e rishkruan fazën.

## Motori i diskut: `tree_engine` (A21)

**Release-i mbështet vetëm `"tree_engine": "pages"` (A21).** Me `db_path`, `tree_engine: "pages"` është
i detyrueshëm. Një konfigurim i vjetër me `db_path` pa `tree_engine` (parazgjedhja e dikurshme ishte
RocksDB), me `"rocksdb"`, ose me `block_cache_mb` (opsion i RocksDB) refuzohet në ngarkim. Plugin-i
s'ngarkohet dhe s'krijohet asnjë skedar. Release-i ndërtohet pa RocksDB dhe pa libclang/bindgen: CI
e kontrollon grafikun e varësive dhe ndërton nga zero pa libclang.

**Kalimi nga RocksDB:** ndalo validatorin, zhvendos dosjen e vjetër `db_path`, vendos
`tree_engine: "pages"` dhe `genesis_path`, pastaj rinise validatorin. Zilja ndërton pemën nga snapshot-i.
Një dosje RocksDB (`CURRENT`, `IDENTITY`, `OPTIONS-*`, `*.sst`, `MANIFEST-*`) s'hapet si `pages` dhe
s'preket.

Motori i vjetër mbetet vetëm për bench dhe krahasime (`--features legacy-rocksdb`, kërkon libclang).
Të dy motorët japin të njëjtat root-e dhe dëftesa, bit për bit (testi
`pages_match_rocksdb_tree_exactly` dhe fuzz-i diferencial).

`"pages"` ruan pemën në një skedar blloqesh 4 KB (`pages.dat`), pa bazë të dhënash:
- çdo faqe ka adresë direkte, ndaj leximi është një `pread` pa indeks;
- faqet e reja të një sloti paketohen në blloqe të plota, dhe faqja e vjetër s'mbishkruhet kurrë;
- dëftesat historike lexohen nga root-i i vjetër, pa arkivë;
- blloqet gjysmë bosh mblidhen vetë.

Çdo slot shton një rekord në `meta.log`, dhe herë pas here gjendja e plotë shkon në
`meta.ckpt`. Me `sync_every_slots` = 1 (parazgjedhja për `pages`), faqet shkojnë në disk para
rekordit, ndaj një ikje rryme humb maksimumi slotin që po shkruhej. Një faqe e dëmtuar ose e
zhvendosur jep gabim, kurrë dëftesë të gabuar, sepse çdo faqe kontrollohet me crc32, me
hash-in e prindit dhe me rrugën e saj.

Ndryshimi i motorit kërkon një dosje të re ose rindërtim nga snapshot-i.

## Rikuperimi dhe dëmtimi i ruajtjes (P-02, P-03, A04–A06, A26)

Në hapje, motori me faqe lexon `meta.ckpt` dhe pas tij rekordet e `meta.log`. Çdo rekord
duhet të jetë i plotë, me CRC të saktë dhe i formatit të njohur. Rekordet duhet të jenë në
rend: numri rendor rritet me 1, sloti rritet rreptësisht. Madhësitë kontrollohen kundër
buxhetit: një rekord deri në 64 MB, log-u deri në 256 MB, checkpoint-i deri në 8 GB.

**I vetmi rast i toleruar** është bishti pas rekordit të fundit të vlefshëm, kur shtrihet
deri në fund të skedarit dhe **s'është kornizë e plotë**:
- kokë ose trup i prerë;
- vetëm zero.

Me `sync_every_slots = 1`, ky është gjithmonë një slot që s'u konfirmua kurrë: commit-i kthehet
vetëm pas fsync-ut të rekordit, ndaj ai slot s'u shërbye dhe s'u firmos kurrë. Bishti ruhet në
`meta.log.tail-<offset>` dhe hiqet nga log-u.

**Korniza e plotë me CRC të gabuar refuzohet (A04)**, edhe kur është e fundit. Pas faktit, një
slot i konfirmuar i dëmtuar s'dallohet nga një shkrim i papërfunduar, ndaj zilja s'e hedh.
Kjo vlen edhe kur trupi i saj është krejt zero. Pas një ikjeje rryme kjo mund të kërkojë
rindërtim me dorë (procedura më poshtë), por kurrë s'humbet në heshtje një slot të konfirmuar.

**Checkpoint-i që mungon (A05).** Pa `meta.ckpt`, pema fshihet dhe rindërtohet vetëm kur dosja
është bosh, ose kur ekziston shënuesi `BUILDING`. Ky shënues shkruhet (me fsync) para hapit të
parë shkatërrues të një ndërtimi nga snapshot-i, dhe hiqet vetëm pasi checkpoint-i i parë është
në disk. Faqe ose `staging/` pa checkpoint dhe pa shënues refuzohen: asgjë s'fshihet.

**Staging-u i snapshot-it (A26).** Gjatë ndërtimit, llogaritë shkruhen në kova të përkohshme në
`staging/`. Për çdo kovë mbahen në RAM numri i rekordeve dhe SHA-256 i bajteve. Para ndërtimit
kontrollohen që të dyja përputhen; një kovë që mungon lejohet vetëm kur priteshin zero rekorde.
Mospërputhja ndal ndërtimin para publikimit dhe firmosjes.

**Çdo gjë tjetër ndalon hapjen me gabim.** Këtu hyjnë: CRC e gabuar me rekorde të vlefshme
pas saj, format i panjohur, rekord i përsëritur ose që mungon, log pa checkpoint, dhe faqe që
s'përputhen me gjendjen. Mesazhi tregon skedarin dhe bajtin, dhe **asnjë skedar s'ndryshohet**.
Plugin-i s'ngarkohet, dhe validatori s'niset me të.

Procedura:
1. Ndalo validatorin.
2. Ruaj dosjen për diagnozë: `mv /var/lib/zilja/db /var/lib/zilja/db.damaged-$(date +%s)`.
3. Rinise validatorin. Zilja ndërton pemën e re nga snapshot-i.

`INVALID` (zinxhir tjetër, ndalim) mbetet rruga e vetme e rindërtimit automatik të një peme
ekzistuese, sepse shënohet qëllimisht nga vetë zilja. **Kur `INVALID` ekziston por s'lexohet**
(leje, I/O, s'është skedar), hapja refuzohet pa ndryshuar asgjë (A06). Nëse shkrimi i tij dështon, `zilja_invalidate_failed`
bëhet 1, dhe dosja duhet zhvendosur me dorë para rinisjes.

**`sync_every_slots`**: motori me faqe pranon vetëm 1 në këtë release. Mënyra asinkrone kërkon
që ripërdorimi i blloqeve të lidhet me rekordin e fundit të konfirmuar në disk, jo vetëm me
historinë dhe me `reuse_slack`. Ky ndryshim s'është bërë ende.

## Pronësia ekskluzive e ruajtjes (P-01, P-04)

- **Pema me faqe:** `db_path/LOCK` mbahet me lock ekskluziv (`flock`) gjatë gjithë jetës së
  instancës. Lock-u merret para çdo recovery, wipe ose shkrimi, dhe rindërtimi s'e fshin. Një
  hapje e dytë e së njëjtës dosje refuzohet me gabim dhe pa ndryshuar asnjë skedar. RocksDB ka
  lock-un e vet (`LOCK`).
- **Log-u i firmave:** `<attest_state>.lock` mbahet në të njëjtën mënyrë, ndaj dy procese në të
  njëjtin host s'mund të firmosin përmes të njëjtit log. Një log që përmban dy root-e të ndryshme
  për të njëjtin slot refuzohet në hapje dhe nuk preket. Ai është provë firmosjeje të dyfishtë dhe
  duhet hetuar me dorë.
- **Kufizim:** lock-u është lokal. Ai **nuk** mbron dy host-e që përdorin të njëjtin çelës
  firmosjeje me log-e të ndryshme, dhe nuk garantohet mbi sisteme skedarësh të ndarë (NFS etj.).
  Një çelës firmosjeje duhet të jetë aktiv në një host të vetëm. Për rezervë (failover), përdor
  çelës tjetër me autorizim të vetin.
- Gjatë ringarkimit të plugin-it (`reloadPlugin`), `on_unload` ndalon serverët dhe punëtorin, dhe
  i liron të dy lock-et para se të hapet instanca e re.
- **Kufizim i ringarkimit me pemë në disk:** bankat që po përpunoheshin gjatë ringarkimit i mori
  instanca e vjetër, ndaj e reja s'mund ta lidhë zinxhirin dhe ndalon menjëherë, fail-closed
  (`zilja_broken` = 1, `/health` = 503). Pema shënohet për rindërtim në rinisjen e ardhshme të
  validatorit. E verifikuar në e2e (`disk=pages`, hapat 9–10). Për ndryshime konfigurimi,
  rinis validatorin në vend të `reloadPlugin`.

## Konteksti: genesis, identiteti, çelësi (P-05)

**Pema me faqe** (`db_path/MANIFEST`) ruan formatin (`zilja-pages`, `format_version`) dhe
genesis-in. Me `tree_engine: "pages"`, `genesis_path` (ose `genesis_hash`) është i
detyrueshëm. Në hapje:
- format ose version tjetër → refuzohet;
- genesis tjetër → refuzohet;
- dosje RocksDB → refuzohet, s'hapet kurrë si instalim i ri pages;
- dosje me të dhëna por pa `MANIFEST` → refuzohet.

Asnjë skedar s'ndryshohet në këto raste.

**Log-u i firmave** (`attest_state`) e nis me rreshtin
`# zilja-attest-log v2 genesis=… identity=… signer=… floor=<slot>`. Hapja me genesis, identitet
ose çelës tjetër refuzohet, dhe mesazhi emërton fushat që ndryshojnë.

**Formati v2 (A03).** Çdo rresht është `<slot> <root hex> <firma hex>`, me firmën origjinale. Në
hapje çdo rresht kontrollohet: format kanonik (hex i vogël), slote në rritje strikte, asnjë slot
nën `floor`, dhe firma vërtetohet me çelësin e firmosjes. Çdo rresht i ndryshuar, i paplotë ose
pa firmë refuzon hapjen, pa ndryshuar skedarin. Zilja s'rifirmos kurrë historinë: pas rinisjes
shërben firmat origjinale. I vetmi rast i toleruar është rreshti i fundit pa `\n` (shkrim i
ndërprerë, firma s'u kthye kurrë): ruhet në `<log>.tail-<offset>` dhe hiqet.

`floor` është kufiri i historisë së kompaktuar: asnjë slot nën të s'firmoset. Edhe një slot
para firmës së fundit refuzohet, sepse s'firmoset jashtë radhe.

**Migrimi nga v1** bëhet vetë në hapjen e parë. Rreshtat v1 s'kanë firma, ndaj s'vërtetohen: s'shërbehen
dhe s'rifirmosen. Origjinali ruhet te `<log>.v1-migrated`, dhe `floor` vendoset te sloti më i madh i
v1 + 1. Pra pas migrimit asnjë slot i asaj historie s'firmoset më. Një log v1 me rresht të dëmtuar
refuzohet.

**Genesis-i real i validatorit:** përdor `genesis_path` = `<ledger>/genesis.bin`, sepse ky
skedar i përket vetë rrjetit ku punon validatori. Kur jepen edhe `genesis_hash` edhe
`genesis_path`, ato duhet të përputhen; ndryshe plugin-i s'ngarkohet.

**Procedurat:**
- **Ndryshimi i çelësit të firmosjes:**
  1. Krijo autorizimin e ri me `zilja-authorize`.
  2. Vendos një `attest_state` **të ri**.
  3. **Ruaje** log-un e vjetër: nëse çelësi i vjetër përdoret ndonjëherë sërish, duhet të
     përdoret me log-un e tij, që të mbetet mbrojtja kundër firmosjes së dyfishtë.
- **Rrjet tjetër (genesis i ri):**
  1. Ndalo validatorin.
  2. Zhvendos `db_path`, dhe vendos `attest_state` të ri (ose autorizim të ri).
  3. Rinise. Pema ndërtohet nga snapshot-i i rrjetit të ri.
- **Log firmash pa kontekst (version para P-05):** plugin-i e refuzon. Kur je i sigurt që log-u
  u krijua nga ky çelës për këtë rrjet, vendos `"attest_adopt_legacy": true` **një herë**.
  Plugin-i e rishkruan log-un me kokën, pa humbur historinë. Pastaj hiqe opsionin.
- **Migrimi i formatit të pemës** (`format_version` i ri në një version të ardhshëm): s'ka
  migrim në vend. Zhvendos dosjen dhe rindërto nga snapshot-i.

## Lidhja me bankat dhe fork-et (P-06, P-07)

Agave 4.3.2 i jep plugin-it `bank_id` për çdo status dhe ndryshim llogarie, por prindin e
jep vetëm si **slot**. Zilja e merr lidhjen e saktë bankë → bankë prind nga sysvar-i
SlotHashes i çdo banke: `(slot_prind, hash_banke_prind)` dhe paraardhësit më tej.

- Lidhja e parë e çdo zinxhiri të rrënjosur duhet të jetë me **hash-in** e bankës së fundit të
  rrënjosur, jo vetëm me slotin e saj.
- Kur një slot ka disa banka të gjalla, zgjidhet vetëm ajo që përputhet me SlotHashes e
  fëmijës. Nëse mbeten disa, zilja **ndalon**, rritet `zilja_ambiguous_banks`, dhe pema
  rindërtohet në rinisje. Asnjë degë s'zgjidhet me hamendje.
- Gjurma e ruajtur është hash-i i bankës së vetë slotit, nga SlotHashes i pasardhësve, kurrë
  hash-i i prindit. Pas rinisjes, një gjurmë e prindit (gjendje të vjetra) ose një gjurmë që
  mungon s'e verifikon gjendjen: pema rindërtohet.

Kufizim: rastet me banka të dyfishta (bllok i dyfishtë, UpdateParent pa njoftim pastrimi)
janë testuar me ngjarje sintetike (`zilja-node/tests/forks.rs`) që ndjekin kontratën e
ndërfaqes. Ato s'mund të prodhohen me një validator të vetëm në e2e, dhe kërkojnë një
cluster testues me fork-e.

## Klientët: një politikë e vetme verifikimi (P-11)

SDK-ja (`sdk/js/src/policy.js`), faqja (`web/check-core.js`, përmes SDK-së) dhe `zilja-check`
(`crates/zilja-cli/src/policy.rs`) vendosin me të njëjtën politikë. Të dyja implementimet
testohen me të njëjtët vektorë (`sdk/js/test/policy-vectors.json`). Një dëftesë e vlefshme
nuk mjafton: `ok` / kodi 0 kërkon që të kalojnë katër kontrolle të ndara.

| Kontrolli | Kalon kur | Parazgjedhja |
|---|---|---|
| dëftesa | përputhet me root-in, për llogarinë dhe slotin e kërkuar; "nuk ekziston" vetëm nga pemë e plotë | gjithmonë |
| root-i | besimi ≥ `min-trust`: `single` < `agreement` < `quorum` < `user` (`--root`) | `agreement`, `min-agree` 2 |
| freskia | sloti ≤ `max-lag` slote pas të finalizuarit të RPC-së; jo më i vjetër se `min-slot` ose se i pari | 150 kur ka RPC Solana |
| të dhënat | bajtet dhe lamports nga RPC-ja përputhen me vulën | kur ka RPC Solana |

- Një konfigurim që s'mund ta arrijë kurrë `min-trust` refuzohet që në fillim: një zile me `agreement` jep `config_min_agree`. `min-agree` s'ulet vetë sipas numrit të zileve.
- Dy zile me root-e të ndryshme për të njëjtin slot japin `conflict`, jo `invalid`. Kuorumi për root-in e dëftesës e mposht konfliktin, por me paralajmërim. Kuorumi për një root tjetër, ose për dy root-e, është `conflict`.
- `zilja-quorum` dhe `--quorum` grupojnë firmat sipas root-it dhe e kontrollojnë root-in (64 hex) para përdorimit. Një zile me përgjigje të keqformuar s'numërohet dhe s'e rrëzon veglën.
- **Burimi i besimit për kuorumin (A10, A11):** pesha merret nga `getVoteAccounts` i RPC-së dhe i besohet asaj. Kjo është **pamja e RPC-së** (`bank.vote_accounts()` i bankës së saj), jo tabela e konsensusit e epokës, dhe kështu etiketohet në çdo dalje. SDK-ja dhe të dy CLI-të përdorin të njëjtin furnizues:
  - merren vetëm llogaritë me `epochVoteAccount`, sipas identitetit (`nodePubkey`);
  - kërkohen të dy grupet (`current`, `delinquent`), çelësat 32 bajt dhe `activatedStake` u64 i saktë (në JS lexohet nga teksti i përgjigjes, pa kaluar nga `Number`); shuma kontrollohet për tejmbushje; çdo vlerë e pasigurt = gabim;
  - `getEpochInfo` lexohet para dhe pas `getVoteAccounts`: epoka duhet të jetë e njëjtë;
  - tabela vlen vetëm për slotet e epokës së saj, **të dy kufijtë** (`quorum_epoch_unsupported`); `zilja-quorum` del me 1 jashtë tyre.
- **Kuorumi s'varet nga radha (A15):** një identitet numërohet vetëm pas firmës së tij të parë të vlefshme.
- `zilja-check` është një thirrje e vetme: mbrojtja nga rollback-u midis thirrjeve bëhet me `--min-slot`. SDK-ja dhe faqja e mbajnë vetë slotin më të lartë të parë.

## Kufijtë e konfigurimit (L-08)

- `cache_depth`: 12–24. Me ~2^thellësi nyje në RAM, 20 do të thotë ~1M nyje (~150 MB).
- `history_slots`: ≤ 100 000.
- `block_cache_mb`: vetëm për motorin e vjetër (`legacy-rocksdb`); me `pages` refuzohet.

## Metrikat kryesore

| Metrika | Kuptimi |
|---|---|
| `zilja_ready` | 1 kur jep dëftesa |
| `zilja_broken` | 1 kur ka ndaluar |
| `zilja_last_root_age_seconds` | sekonda nga root-i i fundit. Alarm mbi ~60 |
| `zilja_queue_len` | ngjarje në pritje (kufiri: 1 048 576) |
| `zilja_resume_verified` | 1 kur pas rinisjes zinxhiri u konfirmua me SlotHashes |
| `zilja_ambiguous_banks_total` | herët kur u përdor rruga rezervë për bankat |

## Ngarkimi i plugin-it

`on_unload` thirret vetëm kur plugin-i hiqet ose ringarkohet me admin RPC (`reloadPlugin`).
Nuk thirret kur validatori ndalet me SIGTERM (u mat në Fazën 0). Prandaj gjendja në disk
është e sigurt ndaj ndalimit pa paralajmërim: çdo slot shkruhet në një `WriteBatch` atomik.

**Ngarkimi në mes të punës s'mbështetet (A02).** Agave lëshon `CreatedBank` pasi konstruktori i
bankës ka shkruar tashmë llogari: epokën, shpërblimet e ndara dhe sysvar-ët. Ndryshimet e bankave
në fluturim i shkojnë instancës së vjetër. Kështu asgjë nga upstream s'provon që një bankë u
vëzhgua e plotë pas një ngarkimi në mes të punës. Prandaj:

- `reloadPlugin` (`on_load(.., true)`): instanca e re s'aplikon asnjë bankë dhe ndalon fail-closed
  (`/health` 503), si në RAM edhe me `pages`. Validatori vazhdon pa u rrëzuar.
- `loadPlugin` pa snapshot (`on_load(.., false)` në mes të punës): pranohet si nisje nga genesis
  vetëm kur zinxhiri i parë i rrënjosur zbret te sloti 0. Përndryshe zilja ndalon.
- Rruga e mbështetur: **rinis validatorin.** Zilja ndërtohet nga snapshot-i, ose vazhdon nga
  pema në disk pas kontrollit të zinxhirit.

## Firmosja (F-10)

1. Krijo një çelës të veçantë: `solana-keygen new -o attest-signer.json`.
2. Autorizoje një herë me identitetin:
   `zilja-authorize validator-keypair.json attest-signer.json $(solana genesis-hash) > zilja-authorization.json`.
   Kjo është e vetmja herë që përdoret çelësi i identitetit.
3. Në konfigurim:
   ```json
   "attest_keypair": "attest-signer.json",
   "attest_authorization": "zilja-authorization.json",
   "genesis_path": "<ledger>/genesis.bin"
   ```

Autorizimi vlen vetëm për atë genesis. Në një zinxhir tjetër (`--reset`, klaster tjetër)
plugin-i refuzon të ngarkohet derisa të bësh një autorizim të ri.

Zilja firmos vetëm pemë të plota (nisur nga snapshot) dhe kurrë dy root-e për të njëjtin slot.
Firmat ruhen te `attest_state`, parazgjedhja `<db_path>.attest.log`. **Mos e fshi këtë skedar:**
pa të, zilja s'e di më çfarë ka firmosur.
