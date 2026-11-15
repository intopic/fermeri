# Zilja: specifika e formatit (v1)

Ky dokument është kontrata mes Ziles (serveri) dhe çdo verifikuesi (kuletë,
telefon, web). Një verifikues që e ndjek saktë duhet të kalojë të gjithë
vektorët në `web/vectors.json` (`cargo run -p zilja-bench --bin zilja-vectors`).

## 1. Hash-et

Të gjitha hash-et janë SHA-256. `‖` = bashkim bajtesh. Numrat janë u64 little-endian.

| Emri | Përkufizimi |
|---|---|
| `data_hash` | `SHA256(data)` |
| `value_hash` | `SHA256("zilja/account/v1" ‖ lamports ‖ owner[32] ‖ executable[1] ‖ data_len ‖ data_hash[32])` |
| `leaf` | `SHA256(0x00 ‖ pubkey[32] ‖ value_hash[32])` |
| `internal` | `SHA256(0x01 ‖ left[32] ‖ right[32])` |
| `empty` | 32 bajte zero |

- `executable` është 1 bajt: `0x00` ose `0x01`.
- Prefikset `0x00`/`0x01` ndajnë gjethet nga nyjet e brendshme, që një nyje e brendshme të mos maskohet si gjethe.
- Vetë `data` nuk hyn në pemë. Hyn `data_hash`, kështu që klienti mund t'i marrë të dhënat nga çdo RPC dhe t'i krahasojë me vulën.
- `rent_epoch` nuk hyn, sepse është hequr nga ndërfaqet e reja të Agave.

## 2. Pema

Sparse Merkle tree binar mbi çelësa 256-bit (`pubkey`).

- Bit-i `i` i çelësit: `(key[i / 8] >> (7 - i % 8)) & 1`. `0` do të thotë majtas, `1` djathtas.
- **Forma kompakte:** një nënpemë me saktësisht një llogari përfaqësohet nga vetë gjethja, në thellësinë ku ndahet nga llogaritë e tjera. Një nënpemë bosh ka vlerën `empty`.
- Forma është kanonike: e njëjta bashkësi llogarish jep gjithmonë të njëjtin root, pavarësisht radhës së përditësimeve.
- Një llogari me `lamports = 0` nuk ekziston, pra hiqet nga pema.

## 3. Cilat slote

Root-i llogaritet vetëm për slotet **Rooted**. Kur rrënjoset sloti S, aplikohen
ndryshimet e të gjithë paraardhësve të tij (nga më i vjetri te më i riu) që nga
sloti i fundit i rrënjosur. Bankat e tjera me slot ≤ S janë fork-e të braktisura
dhe hidhen. Brenda një sloti, për çdo llogari mbetet versioni me `write_version`
më të madh.

## 4. Dëftesa (binar)

```
version   u8        = 1
depth     u16 LE    = numri i vëllezërve (0..=256)
bitmap    ceil(depth/8) bajte; bit-i i (MSB i bajtit i/8) = 1 nëse vëllai i nuk është empty
siblings  32 bajte për çdo vëlla jo-bosh, nga root-i poshtë
leaf_flag u8        = 0 (rruga përfundon në empty) | 1 (përfundon në një gjethe)
[leaf]    pubkey[32] ‖ value_hash[32]   (vetëm kur leaf_flag = 1)
```

Bajtet e tepërta në fund e bëjnë dëftesën të pavlefshme.

## 5. Verifikimi

```
compute_root(key, proof):
  d = len(siblings)
  if leaf:   require common_prefix_len(leaf.pubkey, key) >= d
             cur = leaf_hash(leaf.pubkey, leaf.value_hash)
  else:      cur = empty
  for i = d-1 .. 0:
    cur = bit(key, i) == 0 ? internal(cur, siblings[i]) : internal(siblings[i], cur)
  return cur
```

- **Ekziston** me `meta`: `leaf.pubkey == key`, `leaf.value_hash == value_hash(meta)` dhe `compute_root == root`.
- **Nuk ekziston:** `leaf` mungon ose `leaf.pubkey != key`, dhe `compute_root == root`.
- Çdo gjë tjetër: **e pavlefshme**.

Pas kësaj, nëse klienti merr `data` nga një RPC i zakonshëm, kontrollon
`len(data) == data_len` dhe `SHA256(data) == data_hash`.

## 6. RPC (JSON-RPC 2.0 mbi HTTP, porta e paracaktuar 7799)

### `getAccountProof`

```json
{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["<pubkey base58>"]}
{"jsonrpc":"2.0","id":1,"method":"getAccountProof","params":["<pubkey base58>", {"slot": 1234}]}
```

Me pemën në disk, `slot` mund të jetë çdo slot brenda kujtesës (`provableFrom` deri te
sloti i fundit; parazgjedhja 32 slote). Dëftesa jep gjendjen pas slotit të fundit të
rrënjosur ≤ `slot`. Jashtë kujtesës kthehet gabim `-32602`.

```json
{"slot": 68, "root": "<hex>", "complete": true, "pubkey": "<base58>",
 "account": {"lamports": 1750000000, "owner": "<base58>", "executable": false,
             "dataLen": 0, "dataHash": "<hex>"},
 "proof": "<hex>", "proofBytes": 390}
```

Kur llogaria nuk ekziston, `account` është `null` dhe `proof` provon mospërfshirjen.

### `getZiljaRoot`

```json
{"slot": 68, "root": "<hex>", "accounts": 332, "complete": true, "provableFrom": 37}
```

Kur zilja ndalon (boshllëk ose zinxhir tjetër pas rinisjes), çdo metodë kthen
gabim `-32003` me arsyen, deri në rindërtimin nga snapshot-i.

`complete = false` do të thotë që pema nisi nga genesis pa snapshot dhe mbulon
vetëm llogaritë që kanë ndryshuar që atëherë.

### `GET /metrics`, `GET /health`

Metrikat jepen në formatin Prometheus (`zilja_*`).

## 7. Firma e root-it (Faza 4, jashtë protokollit)

```
message = "zilja/attest/v1" ‖ slot u64 LE ‖ root[32]
```

Çdo validator e firmos me Ed25519. Root-i pranohet kur validatorët unikë me firmë
të vlefshme kanë stake **> 2/3** e totalit të epokës. Në versionin e protokollit
(shih `SIMD-DRAFT.md`), root-i futet në bank hash dhe e firmosin votat ekzistuese.

## 8. Besimi, sot

Root-i që jep serveri **nuk është ende i firmosur nga validatorët**. Sot
verifikimi të mbron nga një RPC që gënjen *për një llogari*, por root-i duhet
krahasuar me burime të pavarura: disa operatorë që nisin Ziles vetë marrin root
identik për të njëjtin slot, sepse pema është kanonike. Besimi i plotë vjen me
firmat (seksioni 7) ose me SIMD-në.
