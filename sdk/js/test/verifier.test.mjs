// Kalon verifikuesin JS nëpër vektorët e gjeneruar nga Rust.
//   node test/verifier.test.mjs [vectors.json]
import { readFileSync } from "node:fs";
import { verifyAccount, dataMatches, bytesToHex, sha256 } from "../src/verifier.js";

const file = process.argv[2] ?? new URL("./vectors.json", import.meta.url);
const { cases } = JSON.parse(readFileSync(file, "utf8"));
let fail = 0;
const t0 = performance.now();
for (const c of cases) {
  const got = await verifyAccount(c.root, c.pubkey, c.account, c.proof);
  if (got !== c.expect) {
    fail++;
    console.error(`❌ ${c.name}: pritej ${c.expect}, erdhi ${got}`);
  }
}
const ms = (performance.now() - t0) / cases.length;

// Dëftesë e prishur në mënyra të ndryshme: s'duhet të rrëzojë verifikuesin.
const c0 = cases.find((c) => c.expect === "exists");
for (const bad of [c0.proof.slice(0, -2), c0.proof + "00", "02" + c0.proof.slice(2), "zz", ""]) {
  if ((await verifyAccount(c0.root, c0.pubkey, c0.account, bad)) !== "invalid") {
    fail++;
    console.error("❌ dëftesa e prishur u pranua");
  }
}

// dataMatches
const data = new TextEncoder().encode("grure");
const acc = { dataLen: "5", dataHash: bytesToHex(await sha256(data)) };
if (!(await dataMatches(acc, data)) || (await dataMatches(acc, new TextEncoder().encode("elb")))) {
  fail++;
  console.error("❌ dataMatches");
}

if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log(`✅ ${cases.length} vektorë + 6 teste shtesë kaluan (${ms.toFixed(2)} ms/verifikim)`);
