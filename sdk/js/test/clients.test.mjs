// P-11: i njëjti rast duhet të japë të njëjtin vendim sigurie në SDK, CLI dhe web. Këto teste
// i ushtrojnë SDK-në dhe logjikën e web-it me RPC të simuluar (dëftesa të vërteta nga vectors.json,
// firma të vërteta ed25519 për kuorumin). CLI-ja testohet me të njëjtët vektorë politike
// (policy-vectors.json) dhe me server të simuluar në crates/zilja-cli/tests.

import { readFileSync } from "node:fs";
import { generateKeyPairSync, sign } from "node:crypto";
import { ZiljaClient, attestMessage, bytesToHex } from "../src/index.js";
import { checkAccount } from "../../../web/check-core.js";

const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8"));
const inc = v.cases.find((x) => x.name === "inclusion" && BigInt(x.account.lamports) < 1n << 50n);
const exc = v.cases.find((x) => x.name === "exclusion");
let fail = 0;
const expect = (name, cond, extra = "") => {
  if (!cond) {
    fail++;
    console.error(`❌ ${name} ${extra}`);
  } else console.log(`✅ ${name}`);
};

const A = "0".repeat(58) + "000001";
const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function b58(bytes) {
  let n = BigInt("0x" + (bytesToHex(bytes) || "0"));
  let s = "";
  while (n > 0n) {
    s = B58[Number(n % 58n)] + s;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    s = "1" + s;
  }
  return s;
}
const GEN = "11".repeat(32);

/** RPC i simuluar: `zile[url]` → { proof, root, attest }, `sol` → metodat e Solana-s. */
function mock({ ziles, sol = {} }) {
  return async (url, req) => {
    const { method, params } = JSON.parse(req.body);
    let result;
    if (url in ziles) {
      const z = ziles[url];
      if (method === "getAccountProof") result = z.proof;
      else if (method === "getZiljaRoot") result = { slot: params[0] ?? z.proof.slot, root: z.root ?? z.proof.root };
      else if (method === "getZiljaAttestation") result = z.attest ?? null;
    } else result = typeof sol[method] === "function" ? sol[method](params) : sol[method];
    if (result === undefined) return { json: async () => ({ error: { message: `s'ka ${method}` } }) };
    return { json: async () => ({ result }) };
  };
}
const proofAt = (slot, c = inc, complete = true) => ({ slot, root: c.root, complete, pubkey: c.pubkey, account: c.account ?? null, proof: c.proof });
const uniq = () => `http://z${Math.random().toString(36).slice(2)}`;

// 1. minAgree s'ulet vetë kur ka më pak zile se ç'kërkon politika: gabim konfigurimi.
{
  let threw = false;
  try {
    new ZiljaClient({ endpoints: ["http://a", "http://b"], minAgree: 3, fetch: async () => ({}) });
  } catch {
    threw = true;
  }
  expect("minAgree 3 me 2 zile → gabim konfigurimi", threw);
}

// 2. Freskia: root-i 1000 slote pas majës së finalizuar s'është "gjendja aktuale".
{
  const [a, b] = [uniq(), uniq()];
  const f = mock({ ziles: { [a]: { proof: proofAt(100) }, [b]: { proof: proofAt(100) } }, sol: { getSlot: 1100, getAccountInfo: undefined } });
  const r = await new ZiljaClient({ endpoints: [a, b], solanaRpc: "http://sol", maxSlotLag: 150, fetch: f }).verifyAccount(inc.pubkey);
  expect("root i vjetër (lag 1000 > 150) → jo ok", r.ok === false && r.checks?.freshness === "stale", JSON.stringify({ ok: r.ok, checks: r.checks }));
}

// 3. Mbrojtja nga kthimi pas: pas slotit 100, një përgjigje për slotin 90 refuzohet.
{
  const [a, b] = [uniq(), uniq()];
  const ziles = { [a]: { proof: proofAt(100) }, [b]: { proof: proofAt(100) } };
  const c = new ZiljaClient({ endpoints: [a, b], fetch: mock({ ziles }) });
  const r1 = await c.verifyAccount(inc.pubkey);
  ziles[a].proof = proofAt(90);
  ziles[b].proof = proofAt(90);
  const r2 = await c.verifyAccount(inc.pubkey);
  expect("kthim pas në slotin 90 pas 100 → jo ok", r1.ok === true && r2.ok === false && r2.checks?.freshness === "rollback", JSON.stringify([r1.ok, r2.ok, r2.checks]));
}

// Kuorumi: 3 validatorë; 2 prej tyre firmosin root-in e zilës së parë.
const vals = [0, 1, 2].map(() => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const pub = new Uint8Array(publicKey.export({ format: "der", type: "spki" }).subarray(-32));
  return { b58: b58(pub), privateKey };
});
// Stake 2 + 2 + 1: dy firmat e para = 80% (> 2/3).
const voteAccounts = { current: vals.map((x, i) => ({ nodePubkey: x.b58, votePubkey: x.b58, epochVoteAccount: true, activatedStake: i < 2 ? 2_000_000_000 : 1_000_000_000 })), delinquent: [] };
const att = (x, slot, root) => ({ slot, root, validator: x.b58, signer: null, authorization: null, genesis: null, signature: bytesToHex(sign(null, attestMessage(GEN, slot, root), x.privateKey)) });

// 4. Një zile në konflikt s'e bllokon një kuorum të vlefshëm (> 2/3) për root-in e dëftesës.
{
  const [a, b, c] = [uniq(), uniq(), uniq()];
  const ziles = {
    [a]: { proof: proofAt(100), attest: att(vals[0], 100, inc.root) },
    [b]: { proof: proofAt(100), attest: att(vals[1], 100, inc.root) },
    [c]: { proof: proofAt(100), root: A, attest: att(vals[2], 100, A) },
  };
  const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 100, getEpochInfo: { absoluteSlot: 100, slotIndex: 100, slotsInEpoch: 432000, epoch: 0 } } });
  const r = await new ZiljaClient({ endpoints: [a, b, c], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, fetch: f }).verifyAccount(inc.pubkey);
  expect("1 zile në konflikt, kuorum 2/3 për root-in → ok, trust quorum", r.ok === true && r.trust === "quorum", JSON.stringify({ ok: r.ok, trust: r.trust, verdict: r.verdict, errors: r.errors }));
}

// 5. Dëftesë historike nga një epokë tjetër: tabela e stake-ut e epokës aktuale s'vlen për të.
{
  const [a, b] = [uniq(), uniq()];
  const ziles = { [a]: { proof: proofAt(50), attest: att(vals[0], 50, inc.root) }, [b]: { proof: proofAt(50), attest: att(vals[1], 50, inc.root) } };
  const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 500, getEpochInfo: { absoluteSlot: 500, slotIndex: 100, slotsInEpoch: 400, epoch: 1 } } });
  const r = await new ZiljaClient({ endpoints: [a, b], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, fetch: f }).verifyAccount(inc.pubkey, { slot: 50 });
  expect("kuorum historik në epokë tjetër → s'numërohet", r.ok === false && r.trust !== "quorum", JSON.stringify({ ok: r.ok, trust: r.trust }));
}

// 6–7. Web-i dhe SDK-ja japin të njëjtin vendim (web-i përdor fetch global).
async function web(opts, ziles) {
  const saved = globalThis.fetch;
  globalThis.fetch = mock({ ziles });
  try {
    return await checkAccount(opts);
  } finally {
    globalThis.fetch = saved;
  }
}
{
  const a = uniq();
  const ziles = { [a]: { proof: proofAt(100) } };
  const w = await web({ ziljaUrl: a, pubkey: inc.pubkey }, ziles);
  // SDK-ja: një zile me parazgjedhjen "agreement" është gabim konfigurimi (minAgree s'ulet vetë).
  let s;
  try {
    s = await new ZiljaClient({ endpoints: [a], fetch: mock({ ziles }) }).verifyAccount(inc.pubkey);
  } catch (e) {
    s = { ok: false, config: /config_min_agree/.test(e.message) };
  }
  expect("një zile e vetme: web = SDK (të dyja s'pranojnë me parazgjedhjen 'agreement')", w.ok === s.ok && s.ok === false && s.config && w.errors?.includes("config_min_agree"), JSON.stringify({ web: w.ok, webErrors: w.errors, sdk: s }));
}
{
  const a = uniq();
  const ziles = { [a]: { proof: proofAt(100, exc, false) } };
  const w = await web({ ziljaUrl: a, pubkey: exc.pubkey, minTrust: "single" }, ziles);
  expect("web: 'nuk ekziston' nga pemë jo e plotë → jo ok", w.ok === false, JSON.stringify({ ok: w.ok }));
  expect("web s'paraqet dëftesë të vlefshme si gjendje aktuale të konsensusit", !w.steps.some((x) => x.ok === true && /NUK ekziston/.test(x.text)));
}

if (fail) {
  console.error(`${fail} teste dështuan`);
  process.exit(1);
}
console.log("✅ klientët: të gjitha kalojnë");
