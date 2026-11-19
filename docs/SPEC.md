# Zilja: specifika e formatit (v2)

Ky dokument është kontrata mes Ziles (serveri) dhe çdo verifikuesi (kuletë, telefon, web).
Një verifikues që e ndjek saktë duhet t'i kalojë të gjithë vektorët në repo (I-04):

- `sdk/js/test/vectors.json`: dëftesat dhe firmat. Gjenerohet me `cargo run -p zilja-bench --bin zilja-vectors`.
- `sdk/js/test/wire-vectors.json`: përgjigje të vërteta të `getAccountProof` dhe versione të
  prishura të tyre. Gjenerohet me `cargo run -p zilja-bench --bin zilja-wire-vectors`.

CI kontrollon që skedarët në repo janë pikërisht ata që gjeneron kodi.

**Ndryshimet nga v1:**
- vendi në pemë është `tree_key = SHA256("zilja/key/v2" ‖ pubkey)` (F-09);
- dëftesa ka `version = 2` dhe duhet të jetë kanonike;
- firmat përfshijnë hash-in e genesis-it dhe lejojnë një çelës të veçantë firmosjeje (F-10);
- numrat e mëdhenj në JSON janë tekst (F-08).

## 1. Hash-et

Të gjitha hash-et janë SHA-256. `‖` = bashkim bajtesh. Numrat janë u64 little-endian.

| Emri | Përkufizimi |
|---|---|
| `data_hash` | `SHA256(data)` |
| `value_hash` | `SHA256("zilja/account/v1" ‖ lamports ‖ owner[32] ‖ executable[1] ‖ data_len ‖ data_hash[32])` |
| `tree_key` | `SHA256("zilja/key/v2" ‖ pubkey[32])` |
| `leaf` | `SHA256(0x00 ‖ tree_key[32] ‖ value_hash[32])` |
| `internal` | `SHA256(0x01 ‖ left[32] ‖ right[32])` |
| `empty` | 32 bajte zero |

- `executable` është 1 bajt: `0x00` ose `0x01`.
- Prefikset `0x00`/`0x01` ndajnë gjethet nga nyjet e brendshme, që një nyje e brendshme të mos maskohet si gjethe.
- Vetë `data` nuk hyn në pemë. Hyn `data_hash`, kështu që klienti mund t'i marrë të dhënat nga çdo RPC dhe t'i krahasojë me vulën.
- `rent_epoch` nuk hyn (I-04). Që nga SIMD-0215, rent-i i llogarive nuk mblidhet më dhe `rent_epoch` është i njëjtë për çdo llogari rent-exempt. Edhe *accounts lattice hash* i Agave e përjashton, ndaj vula do të ishte e varur nga një fushë që protokolli vetë s'e konsideron gjendje.
- Pse `tree_key` dhe jo vetë pubkey-i: çdokush mund të financojë një adresë arbitrare 32 bajtëshe. Me pubkey-in si vend, një sulmues krijon "binjakë" që ndajnë 255 bit me një llogari ekzistuese dhe e çon rrugën në thellësi 256 (30× më shumë punë disku, F-09). Me `tree_key`, për të thelluar rrugën me *k* bit duhet punë hashimi 2^k.

## 2. Pema

Sparse Merkle tree binar mbi `tree_key` (256 bit).

- Bit-i `i` i çelësit: `(key[i / 8] >> (7 - i % 8)) & 1`. `0` do të thotë majtas, `1` djathtas.
- **Forma kompakte:** një nënpemë me saktësisht një llogari përfaqësohet nga vetë gjethja, në thellësinë ku ndahet nga llogaritë e tjera. Një nënpemë bosh ka vlerën `empty`.
- Forma është kanonike: e njëjta bashkësi llogarish jep gjithmonë të njëjtin root, pavarësisht radhës së përditësimeve.
- Një llogari me `lamports = 0` nuk ekziston, pra hiqet nga pema.

## 3. Cilat slote

Root-i llogaritet vetëm për slotet **Rooted**. Kur rrënjoset sloti S, aplikohen ndryshimet e
të gjithë paraardhësve të tij, nga më i vjetri te më i riu, që nga sloti i fundit i rrënjosur.
Rregullat:
- Prindi i çdo banke merret nga hyrja e parë e sysvar-it `SlotHashes` të saj (F-01).
- Zinxhiri duhet të mbarojë saktësisht te sloti i fundit i rrënjosur. Përndryshe zilja ndalon.
- Bankat e tjera me slot ≤ S janë fork-e të braktisura dhe hidhen.
- Brenda një sloti, për çdo llogari mbetet versioni me `write_version` më të madh.
- Te snapshot-i fiton versioni me slotin më të madh (F-02).

## 4. Dëftesa (binar)

```
version   u8        = 2
depth     u16 LE    = numri i vëllezërve (0..=256)
bitmap    ceil(depth/8) bajte; bit-i i (MSB i bajtit i/8) = 1 nëse vëllai i nuk është empty
siblings  32 bajte për çdo vëlla jo-bosh, nga root-i poshtë
leaf_flag u8        = 0 (rruga përfundon në empty) | 1 (përfundon në një gjethe)
[leaf]    tree_key[32] ‖ value_hash[32]   (vetëm kur leaf_flag = 1)
```

Bajtet e tepërta në fund e bëjnë dëftesën të pavlefshme.

**Kodimi kanonik (A20), i detyrueshëm për prodhuesin dhe për çdo verifikues:**
- **Bitët mbushës** të bajtit të fundit të `bitmap` (kur `depth % 8 ≠ 0`, bitët pas `depth`)
  duhet të jenë 0. Një bit mbushës 1 → e pavlefshme.
- **Vëlla bosh i koduar eksplicitisht** ndalohet: një vëlla `empty` kodohet vetëm me bit 0 në
  `bitmap`. Bit 1 me 32 bajte zero → e pavlefshme.
- `leaf_flag` është vetëm 0 ose 1; `version` vetëm 2; `depth` ≤ 256.

Kështu e njëjta dëftesë ka një kodim të vetëm bajt për bajt. Verifikuesit Rust, WASM dhe JS i
refuzojnë të gjitha këto raste.

## 5. Verifikimi

```
compute_root(key = tree_key(pubkey), proof):
  d = len(siblings)
  if d > 0 and siblings[d-1] == empty: return INVALID     # dëftesë jo kanonike
  if leaf:   require common_prefix_len(leaf.tree_key, key) >= d
             cur = leaf_hash(leaf.tree_key, leaf.value_hash)
  else:      cur = empty
  for i = d-1 .. 0:
    cur = bit(key, i) == 0 ? internal(cur, siblings[i]) : internal(siblings[i], cur)
  return cur
```

- **Ekziston** me `meta`: `leaf.tree_key == key`, `leaf.value_hash == value_hash(meta)` dhe `compute_root == root`.
- **Nuk ekziston:** `leaf` mungon ose `leaf.tree_key != key`, dhe `compute_root == root`.
- Çdo gjë tjetër: **e pavlefshme**.

**Kanonike:** në një pemë kanonike vëllai i fundit s'është kurrë `empty`. Një gjethe me vëlla
bosh do të ishte ngjitur lart, dhe dy fëmijë bosh do të ishin një nyje bosh. Rregulli e bën
dëftesën për një çelës të vetme.

**Kujdes:** një dëftesë "nuk ekziston" ka kuptim vetëm kundër një root-i të një peme të plotë
(`complete = true`), të ndërtuar nga një snapshot. Një pemë e nisur nga genesis mbulon vetëm
llogaritë që kanë ndryshuar. Zilja s'jep dëftesa mospërfshirjeje nga një pemë e tillë, dhe
SDK-ja nuk i pranon (L-02).

Pas kësaj, nëse klienti merr `data` nga një RPC i zakonshëm, kontrollon `len(data) == data_len`
dhe `SHA256(data) == data_hash`.

## 6. RPC (JSON-RPC 2.0 mbi HTTP, porta e paracaktuar 7799)

Kërkesat janë deri në 64 KB, dhe çdo kërkesë ka afat të plotë prej 5 s, monoton, që nga pranimi i lidhjes: përfshin kohën në radhë, leximin dhe shkrimin e përgjigjes (A17). Një kërkesë që skadon në radhë merr `503 timeout` pa u lexuar. Parametrat e
gabuar kthejnë `-32602` (L-04). Shih `docs/OPERIMI.md` për portet dhe proxy-n.

### `getAccountProof`

```json
{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["<pubkey base58>"]}
{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["<pubkey base58>", {"slot": 1234}]}
```

Me pemën në disk, `slot` mund të jetë çdo slot brenda kujtesës (`provableFrom` deri te sloti
i fundit; parazgjedhja 32 slote). Dëftesa jep gjendjen pas slotit të fundit të rrënjosur
≤ `slot`, dhe **`slot` në përgjigje është ai slot i rrënjosur** (L-05). Jashtë kujtesës
kthehet gabim `-32602`.

```json
{"slot": 68, "root": "<hex>", "complete": true, "pubkey": "<base58>",
 "account": {"lamports": "1750000000", "owner": "<base58>", "executable": false,
             "dataLen": "0", "dataHash": "<hex>"},
 "proof": "<hex>", "proofBytes": 390}
```

Rregullat e formatit (F-03, F-08):
- `lamports` dhe `dataLen` janë **tekst dhjetor** në [0, 2^64), pa shenjë dhe pa zero në fillim. Një verifikues mund të pranojë edhe një numër JSON, por vetëm ≤ 2^53 − 1.
- `executable` është boolean JSON i vërtetë.
- Hex-i është vetëm `[0-9a-f]`, me gjatësi çift (A20). Prodhuesi s'nxjerr kurrë shkronja të mëdha, dhe **çdo verifikues duhet t'i refuzojë** (root, dëftesë, `dataHash`, firma, autorizim), edhe pse dekodohen në të njëjtat bajte. Rastet `*_uppercase` te `wire-vectors.json` e kontrollojnë.

Kur llogaria nuk ekziston, `account` është `null` dhe `proof` provon mospërfshirjen. Kur
pema s'është e plotë, kthehet gabim `-32006`.

### `getZiljaRoot`

```json
{"jsonrpc":"2.0","id":1,"method":"getZiljaRoot","params":[1234]}
{"slot": 68, "root": "<hex>", "accounts": 332, "complete": true, "provableFrom": 37}
```

Pa parametër: sloti i fundit. Me `slot`: root-i i atij sloti nga unaza e 1024 sloteve të
fundit, ose `-32005` kur s'është aty.

Kur zilja ndalon (boshllëk, zinxhir tjetër, disk i dëmtuar), çdo metodë kthen gabim
`-32003` me arsyen, deri në rindërtimin nga snapshot-i.

### `getZiljaAttestation [slot?]`

```json
{"slot": 68, "root": "<hex>", "validator": "<identiteti base58>", "signer": "<base58>|null",
 "authorization": "<hex>|null", "genesis": "<base58>", "signature": "<hex>"}
```

### `GET /health`, `GET /metrics`

- `/health`: `200 ok` vetëm kur jep dëftesa, përndryshe `503`.
- `/metrics`: formati Prometheus (`zilja_*`), në portin privat `metrics_bind`.

## 7. Firma e root-it (jashtë protokollit), v2

```
message       = "zilja/attest/v2"    ‖ genesis_hash[32] ‖ slot u64 LE ‖ root[32]
authorization = "zilja/authorize/v1" ‖ genesis_hash[32] ‖ signer[32]     (firmosur nga identiteti)
```

- Firmos `signer`, me Ed25519. Kur `signer` mungon, firmos vetë identiteti. Përndryshe
  identiteti duhet ta ketë autorizuar `signer`-in për të njëjtin genesis (`zilja-authorize`).
- Root-i pranohet kur validatorët unikë me firmë të vlefshme kanë stake **> 2/3** e totalit
  të epokës. Stake-u numërohet sipas identitetit.
- Zilja firmos vetëm kur pema është e plotë dhe, pas rinisjes, vetëm pasi zinxhiri u konfirmua.
- Zilja nuk firmos kurrë dy root-e të ndryshme për të njëjtin slot. Çdo (slot, root) shkruhet
  në një skedar me `fsync` para se firma të dalë.
- **Bashkimi i firmave:** sot klienti mbledh N firma Ed25519 nga N zile. Në versionin e
  protokollit (`SIMD-DRAFT.md`), root-i futet në bank hash dhe e mbulojnë votat ekzistuese
  (ose certifikatat BLS të Alpenglow), pra një objekt i vetëm. Logjika e kuorumit mbetet e njëjtë.

## 8. Besimi, sot

Pa firmat, root-i që jep serveri i besohet atij. SDK-ja kërkon si parazgjedhje që të paktën
dy zile të pavarura të japin të njëjtin root për të njëjtin slot (`minTrust = "agreement"`,
L-01). Kjo funksionon sepse pema është kanonike. Me `quorum`, root-i pranohet vetëm me firmat
e > 2/3 të stake-ut. Tabela e stake-ut sot merret nga një RPC dhe i besohet atij; ta provosh
edhe atë (light client mbi votat) është punë e ardhshme.
