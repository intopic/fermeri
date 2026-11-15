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

## Përdorimi

```js
import { ZiljaClient } from "zilja-verify";

const zilja = new ZiljaClient({
  endpoints: ["https://zilja-1.example", "https://zilja-2.example"],
  solanaRpc: "https://api.mainnet-beta.solana.com", // opsionale
  minTrust: "agreement",
});

const r = await zilja.verifyAccount("PUBKEY...");
if (r.ok && r.verdict === "exists") {
  console.log("Balanca e verifikuar:", r.account.lamports, "lamports, slot", r.slot);
}
```

Me React, p.sh. pranë balancës në kuletë:

```jsx
const [badge, setBadge] = useState(null);
useEffect(() => {
  zilja.verifyAccount(pubkey).then((r) => setBadge(r.ok ? "✅ e verifikuar" : "⚠️ e paverifikuar"));
}, [pubkey]);
```

## Çfarë kontrollon `verifyAccount`

1. **Dëftesa:** llogaria (ose mungesa e saj) provohet matematikisht kundër root-it. Një zile që gënjen për një llogari kapet gjithmonë: `verdict: "invalid"`.
2. **Root-i:** sa i besohet vetë vulës? Kjo shihet te `trust`:

| `trust` | Çfarë do të thotë | Si arrihet |
|---|---|---|
| `single` | Root-i i besohet një zileje | Një `endpoint` |
| `agreement` | Disa zile të pavarura japin të njëjtin root për të njëjtin slot | ≥ `minAgree` (2) zile dakord |
| `quorum` | Validatorët me > 2/3 të stake-ut e firmosën root-in | `quorum: true` + `solanaRpc` |

   Nëse dy zile japin root të ndryshëm për të njëjtin slot, rezultati është `invalid`.
3. **Të dhënat** (me `solanaRpc`): `lamports` dhe hash-i i të dhënave nga RPC-ja e zakonshme duhet të përputhen me vulën.

`ok` është `true` vetëm kur dëftesa është e vlefshme **dhe** `trust` ≥ `minTrust`.

> **Kufizim i sotëm:** me `quorum`, tabela e stake-ut merret nga `solanaRpc` dhe i
> besohet asaj. Light client-i që e verifikon edhe tabelën (nga një checkpoint, epokë
> pas epoke) është punë e ardhshme.

## Dëftesë për një slot të kaluar

Zilet me pemë në disk mbajnë ~32 slotet e fundit:

```js
const r = await zilja.verifyAccount(pubkey, { slot: 312345678 });
```

## API e ulët

```js
import { verifyAccount, dataMatches, verifyQuorum } from "zilja-verify";
const verdict = await verifyAccount(rootHex, pubkey, account /* ose null */, proofHex);
// "exists" | "does_not_exist" | "invalid"
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

Testet përdorin të njëjtët vektorë si verifikuesi në Rust: 110 dëftesa, 50 prej tyre sulme, dhe 8 raste firmash.

Licenca: Apache-2.0
