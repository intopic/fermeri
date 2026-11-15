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
