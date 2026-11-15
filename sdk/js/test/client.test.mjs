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
    if (method === "getZiljaRoot") return { slot, root: root ?? inc.root, accounts: 300, complete };
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

// 1) Një zile e vetme: e vlefshme, por besimi "single" dhe paralajmërim.
{
  const z = new ZiljaClient({ endpoints: ["a"], fetch: fakeFetch({ a: zilja(byKey) }) });
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

// 3) Zilja e dytë jep root tjetër → invalid.
{
  const z = new ZiljaClient({ endpoints: ["a", "b"], fetch: fakeFetch({ a: zilja(byKey), b: zilja(byKey, { root: "00".repeat(32) }) }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("disagreement → invalid", !r.ok && r.verdict === "invalid" && r.warnings.some((w) => w.includes("root TJETËR")), r);
}

// 4) Zilja e parë gënjen (dëftesë e rreme për lamports) → invalid.
{
  const z = new ZiljaClient({ endpoints: ["a"], fetch: fakeFetch({ a: zilja(() => attack) }) });
  const r = await z.verifyAccount(attack.pubkey);
  check("lying zilja → invalid", !r.ok && r.verdict === "invalid", r);
}

// 5) minTrust "agreement" me një zile të vetme → ok = false (dëftesa e vlefshme, besimi i pamjaftueshëm).
{
  const z = new ZiljaClient({ endpoints: ["a"], minTrust: "agreement", fetch: fakeFetch({ a: zilja(byKey) }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("minTrust not met", !r.ok && r.verdict === "exists" && r.trust === "single", r);
}

// 6) Zile me pemë në RAM (pa histori): krahasimi bie te getZiljaRoot në të njëjtin slot.
{
  const ram = (method, params) => {
    if (method === "getAccountProof" && params[1]) throw new Error("pema në RAM ka vetëm slotin e fundit");
    return zilja(byKey)(method, params);
  };
  const z = new ZiljaClient({ endpoints: ["a", "b"], fetch: fakeFetch({ a: zilja(byKey), b: ram }) });
  const r = await z.verifyAccount(inc.pubkey);
  check("ram fallback agreement", r.trust === "agreement" && r.agreeing === 2, r);
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

if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log("✅ 12 teste të klientit kaluan");
