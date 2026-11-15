// Safer-3: testi diferencial. Korpusi vjen nga `zilja-fuzz` (Rust): përgjigje të mutuara
// të getAccountProof me vendimin e Rust-it. JS duhet të japë të njëjtin vendim për secilën.
//   node test/fuzz-diff.mjs /rruga/korpus.json
import { readFileSync } from "node:fs";
import { verifyAccount, normalizeAccount } from "../src/index.js";

const { cases } = JSON.parse(readFileSync(process.argv[2], "utf8"));
let fail = 0;
for (const c of cases) {
  const r = c.response ?? {};
  let got;
  try {
    got = await verifyAccount(r.root, r.pubkey, r.account == null ? null : normalizeAccount(r.account), r.proof);
  } catch {
    got = "invalid";
  }
  if (got !== c.expect) {
    if (fail++ < 5) console.error(`❌ pritej ${c.expect}, doli ${got}: ${JSON.stringify(r).slice(0, 300)}`);
  }
}
if (fail) {
  console.error(`${fail} mospërputhje nga ${cases.length}`);
  process.exit(1);
}
console.log(`✅ ${cases.length} raste: JS = Rust`);
