// Vektorët e formatit në rrjet (Safer-4): përgjigje të vërteta të getAccountProof dhe versione
// të prishura. Vendimi duhet të jetë i njëjti si te Rust-i dhe WASM-i.
import { readFileSync } from "node:fs";
import { verifyAccount, normalizeAccount } from "../src/index.js";

// JSON.parse rrumbullakon numrat e mëdhenj, njësoj si fetch().json() në një kuletë.
const v = JSON.parse(readFileSync(new URL("./wire-vectors.json", import.meta.url), "utf8"));
let fail = 0;
for (const c of v.cases) {
  const r = c.response;
  let got;
  try {
    got = await verifyAccount(r.root, r.pubkey, r.account == null ? null : normalizeAccount(r.account), r.proof);
  } catch {
    got = "invalid";
  }
  if (got !== c.expect) {
    fail++;
    console.error(`❌ ${c.name}: pritej ${c.expect}, doli ${got}`);
  }
}
if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log(`✅ ${v.cases.length} vektorë të formatit në rrjet kaluan`);
