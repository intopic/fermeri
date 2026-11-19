// P-11: vektorët e përbashkët të politikës. I njëjti skedar lexohet edhe nga
// crates/zilja-cli/tests/policy_vectors.rs, pra SDK, web dhe CLI vendosin njësoj.
import { readFileSync } from "node:fs";
import { decide } from "../src/policy.js";

const vectors = JSON.parse(readFileSync(new URL("./policy-vectors.json", import.meta.url)));
let fail = 0;
for (const v of vectors) {
  const d = decide(v.input);
  const got = { ok: d.ok, verdict: d.verdict, trust: d.trust, agreeing: d.agreeing, checks: d.checks, errors: d.errors, warnings: d.warnings };
  if (JSON.stringify(got) !== JSON.stringify(v.expect)) {
    fail++;
    console.log(`❌ ${v.name}\n   pritej ${JSON.stringify(v.expect)}\n   u mor  ${JSON.stringify(got)}`);
  }
}
if (fail) {
  console.log(`❌ ${fail}/${vectors.length} vektorë dështuan`);
  process.exit(1);
}
console.log(`✅ ${vectors.length} vektorë të politikës kaluan`);
