// ZiljaClient me zile të simuluara (fetch i rremë), pa rrjet.
import { readFileSync } from "node:fs";
import { ZiljaClient } from "../src/index.js";

const vec = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8"));
const inc = vec.cases.find((c) => c.name === "inclusion");
const exc = vec.cases.find((c) => c.name === "exclusion");
const attack = vec.cases.find((c) => c.name === "attack_wrong_lamports");

/** Zile e simuluar: kthen dëftesën e një rasti, me root-in që i japim. */
function zilja(caseFor, { root, slot = 100, complete = true, attest = null } = {}) {
  return (method, params) => {
    if (method === "getAccountProof") {
      const c = caseFor(params[0]);
      return { slot: params[1]?.slot ?? slot, root: root ?? c.root, complete, pubkey: c.pubkey, account: c.account, proof: c.proof, proofBytes: c.proof.length / 2 };
    }
    if (method === "getZiljaRoot") {
      if (params[0] != null && params[0] !== slot) throw new Error("s'ka root për këtë slot");
      return { slot, root: root ?? inc.root, accounts: 300, complete };
    }
    if (method === "getZiljaAttestation") {
      if (!attest) throw new Error("s'firmos");
      return attest;
    }
    throw new Error("metodë e panjohur");
  };
}

function fakeFetch(servers) {
  return async (url, init) => {
    const { method, params } = JSON.parse(init.body);
    try {
      const result = await servers[url](method, params);
      return { json: async () => ({ jsonrpc: "2.0", id: 1, result }) };
    } catch (e) {
      return { json: async () => ({ jsonrpc: "2.0", id: 1, error: { code: -1, message: e.message } }) };
    }
  };
}

let fail = 0;
const check = (name, cond, extra) => {
  if (!cond) {
    fail++;
    console.error(`❌ ${name}`, extra ?? "");
  }
};

const byKey = (pk) => vec.cases.find((c) => c.pubkey === pk && (c.name === "inclusion" || c.name === "exclusion"));

// 1) Një zile e vetme me minTrust "single": e vlefshme, por paralajmërim.
{
  const z = new ZiljaClient({ endpoints: ["a"], minTrust: "single", fetch: fakeFetch({ a: zilja(byKey) }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("single ok", r.ok && r.verdict === "exists" && r.trust === "single", r);
  check("single lamports bigint", r.account.lamports === BigInt(inc.account.lamports));
  check("single warns", r.warnings.some((w) => w.includes("vetëm një zile")));
  const r2 = await z.verifyAccount(exc.pubkey);
  check("exclusion", r2.ok && r2.verdict === "does_not_exist" && r2.account === null, r2);
}

// 2) Dy zile dakord → "agreement"; me minTrust "agreement" kalon.
{
  const z = new ZiljaClient({ endpoints: ["a", "b"], minTrust: "agreement", fetch: fakeFetch({ a: zilja(byKey), b: zilja(byKey) }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("agreement", r.ok && r.trust === "agreement" && r.agreeing === 2, r);
}

// 3) Zilja e dytë jep root tjetër → conflict (P-11: jo "invalid", por konflikt i dukshëm).
{
  const z = new ZiljaClient({ endpoints: ["a", "b"], fetch: fakeFetch({ a: zilja(byKey), b: zilja(byKey, { root: "00".repeat(32) }) }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("disagreement → conflict", !r.ok && r.verdict === "conflict" && r.errors.includes("root_conflict"), r);
}

// 4) Zilja e parë gënjen (dëftesë e rreme për lamports) → invalid.
{
  const z = new ZiljaClient({ endpoints: ["a"], minTrust: "single", fetch: fakeFetch({ a: zilja(() => attack) }) });
  const r = await z.verifyAccount(attack.pubkey);
  check("lying zilja → invalid", !r.ok && r.verdict === "invalid", r);
}

// 5) Parazgjedhja (L-01) është "agreement": një zile e vetme refuzohet që në konfigurim (P-11).
{
  let msg = "";
  try {
    new ZiljaClient({ endpoints: ["a"], fetch: fakeFetch({ a: zilja(byKey) }) });
  } catch (e) {
    msg = String(e.message);
  }
  check("minTrust not met → config_min_agree", msg.includes("config_min_agree"), msg);
}

// 6) E njëjta adresë dy herë nuk jep "agreement" (L-04): numërohet si një zile.
{
  let msg = "";
  try {
    new ZiljaClient({ endpoints: ["a", "a/"], fetch: fakeFetch({ a: zilja(byKey) }) });
  } catch (e) {
    msg = String(e.message);
  }
  check("dyfishimi nuk numërohet", msg.includes("1 zile"), msg);
}

// 6b) Dëftesë për një llogari tjetër ose slot tjetër → invalid (L-04).
{
  const other = (m, p) => ({ ...zilja(byKey)(m, p), pubkey: exc.pubkey });
  const z = new ZiljaClient({ endpoints: ["a"], minTrust: "single", fetch: fakeFetch({ a: other }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("pubkey tjetër → invalid", !r.ok && r.verdict === "invalid", r);
  const later = (m, p) => ({ ...zilja(byKey)(m, p), slot: 999 });
  const z2 = new ZiljaClient({ endpoints: ["a"], minTrust: "single", fetch: fakeFetch({ a: later }) });
  const r2 = await z2.verifyAccount(inc.pubkey, { slot: 50 });
  check("slot pas të kërkuarit → invalid", !r2.ok && r2.verdict === "invalid", r2);
}

// 6c) "Nuk ekziston" nga pemë jo e plotë nuk është ok (L-02).
{
  const z = new ZiljaClient({ endpoints: ["a", "b"], fetch: fakeFetch({ a: zilja(byKey, { complete: false }), b: zilja(byKey, { complete: false }) }) });
  const r = await z.verifyAccount(exc.pubkey);
  check("exclusion jo e plotë → jo ok", !r.ok && r.verdict === "does_not_exist", r);
}

// 7) Gabimet e konfigurimit.
for (const [name, o] of [
  ["pa endpoints", {}],
  ["quorum pa solanaRpc", { endpoints: ["a"], quorum: true }],
  ["minTrust i panjohur", { endpoints: ["a"], minTrust: "x" }],
]) {
  let threw = false;
  try {
    new ZiljaClient(o);
  } catch {
    threw = true;
  }
  check(`konfigurim: ${name}`, threw);
}

// A17: një zile që kthen një përgjigje të madhe (stream ose Content-Length) s'lexohet e gjitha në RAM.
{
  const big = (stream) => async () => {
    const chunk = new TextEncoder().encode(" ".repeat(64 << 10));
    if (!stream) return { headers: new Map([["content-length", String(8 << 20)]]), text: async () => " ".repeat(8 << 20) };
    let sent = 0;
    const body = new ReadableStream({
      pull(c) {
        sent += chunk.length;
        if (sent > 32 << 20) c.close();
        else c.enqueue(chunk);
      },
    });
    return { headers: new Map(), body };
  };
  for (const stream of [true, false]) {
    let msg = "";
    try {
      await new ZiljaClient({ endpoints: ["a"], minTrust: "single", fetch: big(stream) }).verifyAccount(inc.pubkey);
    } catch (e) {
      msg = e.message;
    }
    check(`A17: përgjigje e madhe ${stream ? "(stream)" : "(content-length)"} → refuzohet`, msg.includes("kufirin"), msg);
  }
}

if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log("✅ 17 teste të klientit kaluan");
