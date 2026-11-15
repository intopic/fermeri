// Testet e auditimit për SDK-në (F-03, F-08, L-03). Duhet të kalojnë pas Fazës 2.
// Deri atëherë nisen me `npm run test:audit` dhe dështojnë me qëllim.
import { readFileSync } from "node:fs";
import { verifyAccount, hexToBytes, ZiljaClient } from "../src/index.js";

const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8"));
const c = v.cases.find((x) => x.name === "inclusion" && BigInt(x.account.lamports) < 1n << 50n);
const exe = v.cases.find((x) => x.name === "inclusion" && x.account.executable === true);
const big = v.cases.find((x) => x.name === "inclusion" && BigInt(x.account.lamports) > 1n << 53n);
let fail = 0;
const expect = (name, cond) => {
  if (!cond) {
    fail++;
    console.error(`❌ ${name}`);
  }
};

// F-03: vlera jashtë [0, 2^64) ose të llojit të gabuar duhet të refuzohen.
const L = BigInt(c.account.lamports);
for (const [name, acc] of [
  ["lamports + 2^64", { ...c.account, lamports: (L + (1n << 64n)).toString() }],
  ["lamports - 2^64", { ...c.account, lamports: (L - (1n << 64n)).toString() }],
  ["dataLen + 2^64", { ...c.account, dataLen: (BigInt(c.account.dataLen) + (1n << 64n)).toString() }],
  ["executable si tekst", { ...exe.account, executable: "false" }],
]) {
  const root = name.startsWith("executable") ? exe.root : c.root;
  const pk = name.startsWith("executable") ? exe.pubkey : c.pubkey;
  const proof = name.startsWith("executable") ? exe.proof : c.proof;
  expect(`F-03 ${name} → invalid`, (await verifyAccount(root, pk, acc, proof)) === "invalid");
}
{
  const lie = { ...c.account, lamports: (L + (1n << 64n)).toString() };
  const f = async () => ({ json: async () => ({ result: { slot: 5, root: c.root, complete: true, pubkey: c.pubkey, account: lie, proof: c.proof } }) });
  const r = await new ZiljaClient({ endpoints: ["http://x"], minTrust: "single", fetch: f }).verifyAccount(c.pubkey);
  expect("F-03 ZiljaClient refuzon zilen që gënjen", r.ok === false && r.verdict === "invalid");
}

// F-08: një server i ndershëm me lamports > 2^53 (si tekst, siç do t'i nxjerrë RPC-ja).
if (big) {
  const body = `{"result":{"slot":5,"root":"${big.root}","complete":true,"pubkey":"${big.pubkey}","account":{"lamports":"${big.account.lamports}","owner":"${big.account.owner}","executable":${big.account.executable},"dataLen":"${big.account.dataLen}","dataHash":"${big.account.dataHash}"},"proof":"${big.proof}"}}`;
  const f = async () => ({ json: async () => JSON.parse(body) });
  const r = await new ZiljaClient({ endpoints: ["http://x"], minTrust: "single", fetch: f }).verifyAccount(big.pubkey);
  expect("F-08 numër i madh si tekst → exists", r.verdict === "exists" && r.account.lamports === BigInt(big.account.lamports));
}

// L-03: hex i prishur refuzohet.
for (const bad of ["-1", "1g", " f"]) {
  let threw = false;
  try {
    hexToBytes(bad);
  } catch {
    threw = true;
  }
  expect(`L-03 hexToBytes(${JSON.stringify(bad)}) refuzohet`, threw);
}

if (fail) {
  console.error(`${fail} teste auditimi dështuan (të pritura deri në Fazën 2)`);
  process.exit(1);
}
console.log("✅ testet e auditimit për SDK-në kaluan");
