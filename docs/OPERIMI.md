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

## Motori i diskut: `tree_engine`

`"tree_engine": "rocksdb"` (parazgjedhja) ose `"pages"`. Të dy japin të njëjtat root-e dhe
dëftesa, bit për bit (testi `pages_match_rocksdb_tree_exactly` dhe fuzz-i diferencial).

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

## Rikuperimi dhe dëmtimi i ruajtjes (P-02, P-03)

Në hapje, motori me faqe lexon `meta.ckpt` dhe pas tij rekordet e `meta.log`. Çdo rekord
duhet të jetë i plotë, me CRC të saktë dhe i formatit të njohur. Rekordet duhet të jenë në
rend: numri rendor rritet me 1, sloti rritet rreptësisht. Madhësitë kontrollohen kundër
buxhetit: një rekord deri në 64 MB, log-u deri në 256 MB, checkpoint-i deri në 8 GB.

**I vetmi rast i toleruar** është bishti pas rekordit të fundit të vlefshëm, kur shtrihet
deri në fund të skedarit:
- kokë ose trup i prerë;
- vetëm zero;
- korniza e fundit me CRC të gabuar.

Me `sync_every_slots = 1`, ky është gjithmonë një slot që s'u konfirmua kurrë: commit-i kthehet
vetëm pas fsync-ut të rekordit, ndaj ai slot s'u shërbye dhe s'u firmos kurrë. Bishti ruhet në
`meta.log.tail-<offset>` dhe hiqet nga log-u.

Kufizim: një bit-flip në rekordin e fundit, që ishte i konfirmuar, s'dallohet nga një shkrim
i prerë. Në atë rast zilja kthehet një slot mbrapa. Pas rinisjes, kontrolli i zinxhirit
(fingerprint) s'gjen vazhdimësi me validatorin dhe e ndalon zilen (fail-closed). Ajo s'jep
dëftesa të gabuara.

**Çdo gjë tjetër ndalon hapjen me gabim.** Këtu hyjnë: CRC e gabuar me rekorde të vlefshme
pas saj, format i panjohur, rekord i përsëritur ose që mungon, log pa checkpoint, dhe faqe që
s'përputhen me gjendjen. Mesazhi tregon skedarin dhe bajtin, dhe **asnjë skedar s'ndryshohet**.
Plugin-i s'ngarkohet, dhe validatori s'niset me të.

Procedura:
1. Ndalo validatorin.
2. Ruaj dosjen për diagnozë: `mv /var/lib/zilja/db /var/lib/zilja/db.damaged-$(date +%s)`.
3. Rinise validatorin. Zilja ndërton pemën e re nga snapshot-i.

`INVALID` (zinxhir tjetër, ndalim) mbetet rruga e vetme e rindërtimit automatik, sepse
shënohet qëllimisht nga vetë zilja. Nëse shkrimi i tij dështon, `zilja_invalidate_failed`
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
`# zilja-attest-log v1 genesis=… identity=… signer=…`. Hapja me genesis, identitet ose çelës
tjetër refuzohet, dhe mesazhi emërton fushat që ndryshojnë. Kështu root-et e vjetra
s'rifirmosen kurrë në një kontekst tjetër.

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

## Kufijtë e konfigurimit (L-08)

- `cache_depth`: 12–24. Me ~2^thellësi nyje në RAM, 20 do të thotë ~1M nyje (~150 MB).
- `history_slots`: ≤ 100 000.
- `block_cache_mb`: 1 – 1 048 576.

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
