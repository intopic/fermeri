# zilja-verify

Verifiko çdo llogari Solana kundër **Ziles** (vulë globale e gjendjes) pa i besuar
RPC-së. **Pa varësi**: vetëm WebCrypto. Rreth 15 KB kod.

> *Verify any Solana account against Zilja (a global state commitment) without trusting
> your RPC. Zero dependencies, WebCrypto only.*

## Instalimi

```bash
npm install zilja-verify
```

Kërkon Node ≥ 22 ose një shfletues modern. Për `quorum` duhet Ed25519 në WebCrypto
(Chrome/Edge ≥ 137, Firefox ≥ 129, Safari ≥ 17).

## Fillo këtu

Një zile mjafton:

```js
import { Zilja } from "zilja-verify";

const zilja = new Zilja("https://zilja.example");
const r = await zilja.verifyAccount("PUBKEY...");
if (r.ok && r.verdict === "exists") {
  console.log("Balanca e verifikuar:", r.account.lamports, "lamports, slot", r.slot);
} else {
  console.log("Mos i beso:", r.verdict, r.errors);
}
```

Dëftesa verifikohet gjithmonë matematikisht: një zile që gënjen për një llogari kapet
(`verdict: "invalid"`). Root-i i besohet kësaj zileje (`trust: "single"`). Kjo është
parazgjedhja. Për më shumë besim shih [Më shumë besim](#më-shumë-besim).

Për të ditur që gjendja është e freskët, shto një RPC Solana:

```js
const zilja = new Zilja("https://zilja.example", { solanaRpc: "https://api.mainnet-beta.solana.com" });
```

Me React, p.sh. pranë balancës në kuletë:

```jsx
const [badge, setBadge] = useState(null);
useEffect(() => {
  zilja.verifyAccount(pubkey).then((r) => setBadge(r.ok ? "✅ e verifikuar" : "⚠️ e paverifikuar"));
}, [pubkey]);
```

## Çfarë kontrollon `verifyAccount`

Katër pyetje të ndara, që shihen veç te `r.checks` (P-11). Një dëftesë e vlefshme **nuk**
do të thotë vetvetiu "kjo është gjendja aktuale e konsensusit".

1. **Dëftesa** (`checks.proof`): llogaria, ose mungesa e saj, provohet matematikisht kundër root-it. Një zile që gënjen për një llogari kapet gjithmonë: `verdict: "invalid"`. Përgjigja duhet të jetë për llogarinë dhe slotin e kërkuar. "Nuk ekziston" pranohet vetëm nga një pemë e plotë (`checks.completeness: "complete"`).
2. **Root-i** (`checks.root`, `trust`): pse i besojmë vulës?

| `trust` | Çfarë do të thotë | Si arrihet |
|---|---|---|
| `single` | Root-i i besohet një zileje (parazgjedhja) | Një zile mjafton |
| `agreement` | Disa zile të pavarura japin të njëjtin root për të njëjtin slot | ≥ `minAgree` (2) zile dakord |
| `quorum` | Validatorët me > 2/3 të stake-ut e firmosën root-in | `quorum: true` + `solanaRpc` |
| `user` | Root-in e dhe vetë | `root: "<64 hex>"` |

   Nëse dy zile japin root të ndryshëm për të njëjtin slot, `verdict` është `"conflict"`. Kuorumi, kur arrihet për root-in e dëftesës, e mposht një zile në konflikt (`warnings: ["conflict_outvoted_by_quorum"]`). Kuorumi për një root tjetër, ose për dy root-e, është gjithashtu `"conflict"`.
3. **Freskia** (`checks.freshness`): me `solanaRpc`, sloti nuk duhet të jetë më shumë se `maxSlotLag` (150) pas slotit të finalizuar, as mbi të (`ahead`: finaliteti s'konfirmohet). Një slot më i vjetër se ai që instanca ka parë më parë jep `rollback`. Me `{ slot }` kërkesa është `historical`. Pa `solanaRpc`, freskia mbetet `unchecked`, ndaj mos e trego si "gjendje aktuale".
4. **Të dhënat** (`checks.data`, me `solanaRpc`): `lamports`, `owner`, `executable` dhe bajtet nga RPC-ja e zakonshme duhet të përputhen me vulën. Kur krahasimi kërkohet por RPC-ja s'përgjigjet, `checks.data` është `unavailable` dhe `ok` është `false`.

**Referenca jote (`root`).** `root: "<hex>"` vërteton vetëm që llogaria është te ai root: sloti, freskia dhe mungesa s'vërtetohen. Për to jep `root: { root, slot, complete: true }`.

`ok` është `true` vetëm kur çdo kontroll kaloi. Kodet e atyre që dështuan janë te `r.errors`
(`stale`, `root_conflict`, `absence_incomplete`, `trust_below_min`…). Një konfigurim që s'mund
ta arrijë kurrë `minTrust`, p.sh. një zile me `"agreement"`, hedh gabim që në konstruktor.

E njëjta politikë (`decide`) përdoret nga faqja web dhe nga `zilja-check` në Rust. Të tria
testohen me të njëjtët vektorë (`test/policy-vectors.json`).

> **Kufizim i sotëm:** me `quorum`, pesha merret nga `getVoteAccounts` i `solanaRpc` dhe i
> besohet asaj: është pamja e RPC-së, jo tabela e konsensusit e epokës. Vlen vetëm për slotet
> e epokës së saj (të dy kufijtë) dhe kur epoka s'ndryshon gjatë leximit; përndryshe kuorumi
> s'numërohet (`quorum_epoch_unsupported`). Light client-i që e verifikon edhe
> tabelën (nga një checkpoint, epokë pas epoke) është punë e ardhshme.

## Më shumë besim

Opsionale. Parazgjedhja (`single`) i beson root-it të një zileje; këto e forcojnë:

```js
import { ZiljaClient } from "zilja-verify";

// agreement: disa zile të pavarura duhet të japin të njëjtin root.
const a = new ZiljaClient({ endpoints: ["https://zilja-1.example", "https://zilja-2.example"], minTrust: "agreement" });

// quorum: validatorët me > 2/3 të stake-ut e firmosën root-in.
const q = new ZiljaClient({ endpoints: ["https://zilja-1.example"], solanaRpc: "https://api.mainnet-beta.solana.com", quorum: true, minTrust: "quorum" });

// aggregator: firmat e shumë validatorëve me një thirrje (getZiljaQuorumBundle).
const g = new ZiljaClient({ endpoints: ["https://zilja-1.example"], solanaRpc: "https://api.mainnet-beta.solana.com", quorum: true, minTrust: "quorum", aggregators: ["https://aggregator.example"] });
```

`new Zilja(url, { also: ["https://zilja-2.example"], minTrust: "agreement" })` është e njëjta gjë si `a`.

## Dëftesë për një slot të kaluar

Zilet me pemë në disk mbajnë ~32 slotet e fundit:

```js
const r = await zilja.verifyAccount(pubkey, { slot: 312345678 });
```

## Drop-in për web3.js dhe kit (R5-4)

```js
import { Connection, PublicKey } from "@solana/web3.js";
import { VerifiableConnection } from "zilja-verify";

const zilja = { endpoints: ["https://zilja.example"], solanaRpc: "https://api.mainnet-beta.solana.com" };
const conn = new VerifiableConnection({ zilja, PublicKey });
const info = await conn.getAccountInfo(new PublicKey(addr));          // AccountInfo | null, e verifikuar
const [a, b] = await conn.getMultipleAccountsInfo([addrA, addrB]);   // një multi-proof
const bal = await conn.getTokenAccountBalance(ata);                   // ATA + mint, të verifikuara
```

Me `@solana/kit`: `const rpc = createVerifiedRpc(zilja); await rpc.getAccountInfo(address).send()`.

Çdo llogari kthehet vetëm kur dëftesa, besimi te root-i (`minTrust`), freskia dhe bajtet e RPC-së
(krahasuar me `dataHash` e vulosur) janë në rregull. Përndryshe hidhet `VerificationError` (ose
kthehet `null` me `onUnverified: "null"`). Llogaritë që ndryshojnë çdo slot (p.sh. `Clock`) mund të
mos verifikohen, sepse RPC-ja është para slotit të vulosur.

## API e ulët

```js
import { verifyAccount, dataMatches, verifyQuorum } from "zilja-verify";
const verdict = await verifyAccount(rootHex, pubkey, account /* ose null */, proofHex);
// "exists" | "does_not_exist" | "invalid"

// Kuorumi: `epoch` është epoka e slotit (ajo e tabelës së stake-ut). Një çelës i veçantë numërohet
// vetëm me autorizim v2 që e mbulon atë epokë (maksimumi 15 epoka); v1 s'pranohet.
const q = await verifyQuorum(genesisHex, epoch, slot, rootHex, attestations, stakeTable);
```

Formati i plotë: [`docs/SPEC.md`](https://github.com/intopic/fermeri/blob/main/docs/SPEC.md).

## Shembull nga terminali

```bash
node examples/verify.mjs <PUBKEY> --zilja http://127.0.0.1:7799 --rpc http://127.0.0.1:8899 --quorum
```

## Testet

```bash
npm test
```

Testet përdorin të njëjtët vektorë si verifikuesi në Rust: 110 dëftesa, 50 prej tyre sulme, dhe 15 raste firmash (përfshirë skadimin e autorizimit v2).

Licenca: Apache-2.0
