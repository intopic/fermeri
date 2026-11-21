// P-11: i njëjti rast duhet të japë të njëjtin vendim sigurie në SDK, CLI dhe web. Këto teste
// i ushtrojnë SDK-në dhe logjikën e web-it me RPC të simuluar (dëftesa të vërteta nga vectors.json,
// firma të vërteta ed25519 për kuorumin). CLI-ja testohet me të njëjtët vektorë politike
// (policy-vectors.json) dhe me server të simuluar në crates/zilja-cli/tests.

import { readFileSync } from "node:fs";
import { generateKeyPairSync, sign } from "node:crypto";
import { Zilja, ZiljaClient, attestMessage, authorizeMessage, revokeMessage, bytesToHex } from "../src/index.js";
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
      else if (method === "getZiljaRoot") result = { slot: params[0] ?? z.proof.slot, root: z.root ?? z.proof.root, complete: z.complete };
      else if (method === "getZiljaAttestation") result = z.attest ?? null;
      else if (method === "getZiljaRevocations") result = z.revocations;
      else if (method === "getMultipleAccountProofs") result = z.multi;
      else if (method === "getZiljaQuorumBundle") result = z.bundle;
    } else result = typeof sol[method] === "function" ? sol[method](params) : sol[method];
    if (result === undefined) return { json: async () => ({ error: { message: `s'ka ${method}` } }) };
    return { json: async () => ({ result }) };
  };
}
const proofAt = (slot, c = inc, complete = true) => ({ slot, root: c.root, complete, pubkey: c.pubkey, account: c.account ?? null, proof: c.proof });
const uniq = () => `http://z${Math.random().toString(36).slice(2)}`;

// G10: e njëjta zile e shkruar ndryshe (shkronja, port parazgjedhës, "/" në fund) numërohet një herë.
{
  const c = new ZiljaClient({ endpoints: ["https://Zile.Example:443/", "https://zile.example", "HTTPS://ZILE.EXAMPLE//", "http://zile.example:8899"], fetch: async () => ({}) });
  expect("G10: tre shkrime të së njëjtës zile → një", c.endpoints.length === 2 && c.endpoints[0] === "https://zile.example", JSON.stringify(c.endpoints));
  let threw = false;
  try {
    new ZiljaClient({ endpoints: ["http://a", "HTTP://A:80/"], minTrust: "agreement", minAgree: 2, fetch: async () => ({}) });
  } catch {
    threw = true;
  }
  expect("G10: minAgree 2 me dy shkrime të së njëjtës zile → gabim konfigurimi", threw);
}

// Hyrja e thjeshtë: new Zilja(url) me një zile jep vendim të plotë me parazgjedhjet.
{
  const a = uniq();
  const r = await new Zilja(a, { fetch: mock({ ziles: { [a]: { proof: proofAt(100) } } }) }).verifyAccount(inc.pubkey);
  expect("new Zilja(url): një zile → ok, single", r.ok && r.trust === "single" && r.verdict === "exists", JSON.stringify({ ok: r.ok, trust: r.trust, errors: r.errors }));
  let threw = false;
  try { new Zilja(); } catch { threw = true; }
  expect("new Zilja() pa adresë → gabim i qartë", threw);
}

// 1. minAgree s'ulet vetë kur ka më pak zile se ç'kërkon politika: gabim konfigurimi.
{
  let threw = false;
  try {
    new ZiljaClient({ endpoints: ["http://a", "http://b"], minTrust: "agreement", minAgree: 3, fetch: async () => ({}) });
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

// R5-2: verifyAccounts: një multi-proof dhe një kontroll i root-it për të gjitha llogaritë.
{
  const mc = v.multi.cases.find((x) => x.name === "valid_12");
  const pubkeys = mc.claims.map((c) => c.pubkey);
  const multi = (proof = mc.proof, order = pubkeys) => ({
    slot: 100, root: mc.root, complete: true, proof,
    accounts: order.map((pk) => ({ pubkey: pk, account: mc.claims.find((c) => c.pubkey === pk).account })),
  });
  const run = async (primaryMulti) => {
    const [a, b] = [uniq(), uniq()];
    const ziles = { [a]: { multi: primaryMulti, root: mc.root, proof: { slot: 100, root: mc.root } }, [b]: { root: mc.root, complete: true, proof: { slot: 100, root: mc.root } } };
    return new ZiljaClient({ endpoints: [a, b], fetch: mock({ ziles }) }).verifyAccounts(pubkeys);
  };
  const good = await run(multi());
  const want = mc.expect;
  expect("R5-2: verifyAccounts → çdo llogari ok me agreement, verdikti si Rust-i",
    good.length === pubkeys.length && good.every((r, i) => r.ok === true && r.trust === "agreement" && r.verdict === want[i]),
    JSON.stringify(good.map((r) => [r.ok, r.trust, r.verdict])));
  const bad = await run(multi(v.multi.cases.find((x) => x.name === "attack_tampered_sibling_12").proof));
  expect("R5-2: multi-proof i ndryshuar → asnjë ok", bad.every((r) => r.ok === false && r.verdict === "invalid"), JSON.stringify(bad.map((r) => r.verdict)));
  const swapped = await run(multi(mc.proof, [...pubkeys].reverse()));
  expect("R5-2: llogari në radhë tjetër nga kërkesa → asnjë ok", swapped.every((r) => r.ok === false), JSON.stringify(swapped.map((r) => r.ok)));
  let threw = false;
  try { await new ZiljaClient({ endpoints: [uniq()], minTrust: "single", fetch: mock({ ziles: {} }) }).verifyAccounts([]); } catch { threw = true; }
  expect("R5-2: 0 pubkey → gabim", threw);
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

// R5-1: një revokim i shërbyer nga cilado zile (edhe një që s'ka asnjë firmë) e heq çelësin e
// veçantë; një revokim i falsifikuar s'ka efekt.
{
  const keyOf = () => {
    const { publicKey, privateKey } = generateKeyPairSync("ed25519");
    return { b58: b58(new Uint8Array(publicKey.export({ format: "der", type: "spki" }).subarray(-32))), privateKey };
  };
  const s0 = keyOf();
  const ded = { slot: 100, root: inc.root, validator: vals[0].b58, signer: s0.b58, genesis: null,
    authorization: { fromEpoch: 0, untilEpoch: 15, signature: bytesToHex(sign(null, authorizeMessage(GEN, s0.b58, 0, 15), vals[0].privateKey)) },
    signature: bytesToHex(sign(null, attestMessage(GEN, 100, inc.root), s0.privateKey)) };
  const real = { validator: vals[0].b58, signer: s0.b58, signature: bytesToHex(sign(null, revokeMessage(GEN, s0.b58), vals[0].privateKey)) };
  const fake = { ...real, signature: bytesToHex(sign(null, revokeMessage(GEN, s0.b58), vals[2].privateKey)) };
  const run = async (revocations) => {
    const [a, b, c] = [uniq(), uniq(), uniq()];
    const ziles = { [a]: { proof: proofAt(100), attest: ded }, [b]: { proof: proofAt(100), attest: att(vals[1], 100, inc.root) }, [c]: { proof: proofAt(100), revocations } };
    const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 100, getEpochInfo: { absoluteSlot: 100, slotIndex: 100, slotsInEpoch: 432000, epoch: 0 } } });
    return new ZiljaClient({ endpoints: [a, b, c], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, compareData: false, fetch: f }).verifyAccount(inc.pubkey);
  };
  const ok = await run([]);
  const revoked = await run([real]);
  const forged = await run([fake]);
  expect("R5-1: pa revokim → kuorum", ok.ok === true && ok.trust === "quorum", JSON.stringify({ ok: ok.ok, trust: ok.trust, errors: ok.errors }));
  expect("R5-1: revokim nga një zile tjetër → s'ka kuorum", revoked.trust !== "quorum", JSON.stringify({ ok: revoked.ok, trust: revoked.trust }));
  expect("R5-1: revokim i falsifikuar → kuorum", forged.ok === true && forged.trust === "quorum", JSON.stringify({ ok: forged.ok, trust: forged.trust }));
  // G03: e njëjta instancë s'e harron revokimin kur burimi s'e jep më herën tjetër.
  {
    const [a, b, c] = [uniq(), uniq(), uniq()];
    const ziles = { [a]: { proof: proofAt(100), attest: ded }, [b]: { proof: proofAt(100), attest: att(vals[1], 100, inc.root) }, [c]: { proof: proofAt(100), revocations: [real] } };
    const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 100, getEpochInfo: { absoluteSlot: 100, slotIndex: 100, slotsInEpoch: 432000, epoch: 0 } } });
    const client = new ZiljaClient({ endpoints: [a, b, c], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, compareData: false, fetch: f });
    const first = await client.verifyAccount(inc.pubkey);
    ziles[c].revocations = [];
    const second = await client.verifyAccount(inc.pubkey);
    expect("G03: revokimi mbetet në instancë kur burimi mungon", first.trust !== "quorum" && second.trust !== "quorum", JSON.stringify({ first: first.trust, second: second.trust }));
    const learned = client.learnedRevocations(GEN);
    expect("G03: learnedRevocations jep revokimin e verifikuar", learned.length === 1 && learned[0].signer === s0.b58, JSON.stringify(learned));
  }
}

// R5-3: aggregator-i i pabesuar: firmat e shumë zileve me një thirrje. Firmat e vërteta numërohen;
// një firmë e falsifikuar s'kalon; një aggregator që s'përgjigjet s'e prish verifikimin.
{
  const run = async (bundle) => {
    const [z, ag] = [uniq(), uniq()];
    const ziles = { [z]: { proof: proofAt(100) }, [ag]: { bundle } };
    const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 100, getEpochInfo: { absoluteSlot: 100, slotIndex: 100, slotsInEpoch: 432000, epoch: 0 } } });
    return new ZiljaClient({ endpoints: [z], aggregators: [ag], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, compareData: false, fetch: f }).verifyAccount(inc.pubkey);
  };
  const real = [att(vals[0], 100, inc.root), att(vals[1], 100, inc.root)];
  const good = await run({ slot: 100, attestations: real, revocations: [] });
  expect("R5-3: kuorum nga bundle-i i aggregator-it", good.ok === true && good.trust === "quorum", JSON.stringify({ ok: good.ok, trust: good.trust, errors: good.errors }));
  const forged = await run({ slot: 100, attestations: [real[0], { ...real[0], validator: vals[1].b58 }], revocations: [] });
  expect("R5-3: firmë e falsifikuar nga aggregator-i → s'ka kuorum", forged.trust !== "quorum", JSON.stringify({ ok: forged.ok, trust: forged.trust }));
  const down = await run(undefined);
  expect("R5-3: aggregator që s'përgjigjet → s'ka kuorum, pa rrëzim", down.trust !== "quorum" && down.warnings.some((w) => w.includes("aggregator")), JSON.stringify(down.warnings));
}

// 4. Një zile në konflikt s'e bllokon një kuorum të vlefshëm (> 2/3) për root-in e dëftesës.
{
  const [a, b, c] = [uniq(), uniq(), uniq()];
  const ziles = {
    [a]: { proof: proofAt(100), attest: att(vals[0], 100, inc.root) },
    [b]: { proof: proofAt(100), attest: att(vals[1], 100, inc.root) },
    [c]: { proof: proofAt(100), root: A, attest: att(vals[2], 100, A) },
  };
  const f = mock({ ziles, sol: { getVoteAccounts: voteAccounts, getSlot: 100, getEpochInfo: { absoluteSlot: 100, slotIndex: 100, slotsInEpoch: 432000, epoch: 0 } } });
  const r = await new ZiljaClient({ endpoints: [a, b, c], solanaRpc: "http://sol", quorum: true, minTrust: "quorum", genesisHash: GEN, compareData: false, fetch: f }).verifyAccount(inc.pubkey);
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
  // Parazgjedhja "single": një zile mjafton, web dhe SDK japin të njëjtin vendim.
  const w = await web({ ziljaUrl: a, pubkey: inc.pubkey }, ziles);
  const s = await new ZiljaClient({ endpoints: [a], fetch: mock({ ziles }) }).verifyAccount(inc.pubkey);
  expect("një zile e vetme me parazgjedhjen: web = SDK = ok (single)", w.ok === s.ok && s.ok === true && s.trust === "single", JSON.stringify({ web: w.ok, webErrors: w.errors, sdk: s.ok }));
  // Me "agreement" të kërkuar, të dyja e refuzojnë një zile (minAgree s'ulet vetë).
  const w2 = await web({ ziljaUrl: a, pubkey: inc.pubkey, minTrust: "agreement" }, ziles);
  let s2;
  try {
    s2 = await new ZiljaClient({ endpoints: [a], minTrust: "agreement", fetch: mock({ ziles }) }).verifyAccount(inc.pubkey);
  } catch (e) {
    s2 = { ok: false, config: /config_min_agree/.test(e.message) };
  }
  expect("një zile me 'agreement': web = SDK, të dyja refuzojnë", w2.ok === false && s2.ok === false && s2.config && w2.errors?.includes("config_min_agree"), JSON.stringify({ web: w2.errors, sdk: s2 }));
}
{
  const a = uniq();
  const ziles = { [a]: { proof: proofAt(100, exc, false) } };
  const w = await web({ ziljaUrl: a, pubkey: exc.pubkey, minTrust: "single" }, ziles);
  expect("web: 'nuk ekziston' nga pemë jo e plotë → jo ok", w.ok === false, JSON.stringify({ ok: w.ok }));
  expect("web s'paraqet dëftesë të vlefshme si gjendje aktuale të konsensusit", !w.steps.some((x) => x.ok === true && /NUK ekziston/.test(x.text)));
}

// A12, A13, A14 përmes klientit (jo vetëm politikës).
{
  const z = uniq();
  const mk = (o, sol = {}, c = inc, complete = true) => new ZiljaClient({ endpoints: [z], minTrust: "single", fetch: mock({ ziles: { [z]: { proof: proofAt(100, c, complete) } }, sol }), ...o });
  // A12: zilja në slotin 100, RPC-ja e finalizuar në 99 → "ahead", jo "fresh", dhe kufiri s'rritet.
  const c12 = mk({ solanaRpc: "http://sol", compareData: false }, { getSlot: 99 });
  const r12 = await c12.verifyAccount(inc.pubkey);
  expect("A12: slot mbi referencën → ahead, jo ok", !r12.ok && r12.checks.freshness === "ahead" && r12.errors.includes("ahead_of_reference") && c12.highestSeen === null, JSON.stringify(r12.checks));
  // A13: getAccountInfo s'përgjigjet → data "unavailable", jo ok.
  const r13 = await mk({ solanaRpc: "http://sol", maxSlotLag: null }, {}).verifyAccount(inc.pubkey);
  expect("A13: krahasim i kërkuar që s'u krye → jo ok", !r13.ok && r13.checks.data === "unavailable" && r13.errors.includes("data_unavailable"), JSON.stringify(r13.errors));
  // A14: referencë vetëm-hash + mungesë → s'pranohet edhe kur serveri thotë complete.
  const r14 = await mk({ root: exc.root }, {}, exc).verifyAccount(exc.pubkey);
  expect("A14: root vetëm-hash s'provon mungesë", !r14.ok && r14.errors.includes("absence_incomplete") && r14.checks.freshness === "unverifiable", JSON.stringify(r14.errors));
  const r14b = await mk({ root: { root: exc.root, slot: 100, complete: true } }, {}, exc, false).verifyAccount(exc.pubkey);
  expect("A14: referencë me slot + complete → mungesa pranohet", r14b.ok && r14b.verdict === "does_not_exist", JSON.stringify(r14b.errors));
  const r14c = await mk({ root: { root: exc.root, slot: 101, complete: true } }, {}, exc).verifyAccount(exc.pubkey);
  expect("A14: slot tjetër nga referenca → invalid", !r14c.ok && r14c.errors.includes("anchor_slot_mismatch"), JSON.stringify(r14c.errors));
}

// G23: web/check.mjs refuzon flamurët e panjohur dhe argumentet e tepërta (kodi 2), para çdo kërkese.
{
  const { spawnSync } = await import("node:child_process");
  const run = (...a) => spawnSync(process.execPath, [new URL("../../../web/check.mjs", import.meta.url).pathname, ...a], { encoding: "utf8" });
  const bad = [["http://x", "pk", "--min-turst", "single"], ["http://x", "pk", "sol", "extra"], ["http://x", "pk", "--root"], ["http://x", "pk", "--min-trust", "low"]];
  const res = bad.map((a) => run(...a));
  expect("G23: check.mjs → kodi 2 për argumente të gabuara", res.every((r) => r.status === 2), JSON.stringify(res.map((r) => [r.status, r.stderr.split("\n")[0]])));
}

// G20: slot-i dhe root-i i rezultatit janë numër/tekst ose null, kurrë undefined apo vlera e papastër e zilës.
{
  const a = uniq();
  const r = await new ZiljaClient({ endpoints: [a], minTrust: "single", fetch: mock({ ziles: { [a]: { proof: { ...proofAt(100), slot: "100", root: 7 } } } }) }).verifyAccount(inc.pubkey);
  expect("G20: slot jo-numër → null, root jo-tekst → null", r.slot === null && r.root === null && !r.ok, JSON.stringify({ slot: r.slot, root: r.root }));
}

// G19: faqja shpjegon çdo vlerë të freskisë dhe e mban mbrojtjen nga kthimi pas veç për çdo kontekst.
{
  const a = uniq();
  const w = await web({ ziljaUrl: a, pubkey: exc.pubkey, rootOverride: exc.root }, { [a]: { proof: proofAt(100, exc) } });
  const fresh = w.steps.find((x) => x.text.startsWith("Freskia"));
  expect("G19: freskia 'unverifiable' ka tekst", w.checks?.freshness === "unverifiable" && !fresh.text.includes("undefined"), fresh?.text);
  const b = uniq(), c = uniq();
  const w1 = await web({ ziljaUrl: b, pubkey: inc.pubkey, minTrust: "single" }, { [b]: { proof: proofAt(100) } });
  const w2 = await web({ ziljaUrl: c, pubkey: inc.pubkey, minTrust: "single" }, { [c]: { proof: proofAt(50) } });
  expect("G19: zile tjetër me slot më të ulët ≠ kthim pas", w1.ok && w2.ok && w2.checks.freshness !== "rollback", JSON.stringify(w2.checks));
  const w3 = await web({ ziljaUrl: b, pubkey: inc.pubkey, minTrust: "single" }, { [b]: { proof: proofAt(60) } });
  expect("G19: e njëjta zile më pas → kthim pas", !w3.ok && w3.checks.freshness === "rollback", JSON.stringify(w3.checks));
}

if (fail) {
  console.error(`${fail} teste dështuan`);
  process.exit(1);
}
console.log("✅ klientët: të gjitha kalojnë");

