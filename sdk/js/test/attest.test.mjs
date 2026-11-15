// Firmat e validatorëve: JS duhet të japë saktësisht të njëjtin vendim si Rust.
import { readFileSync } from "node:fs";
import { verifyQuorum, stakeTableFromVoteAccounts, attestMessage, bytesToHex } from "../src/index.js";

const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")).attest;
const table = new Map(v.table.map(([k, s]) => [k, BigInt(s)]));
let fail = 0;
for (const c of v.cases) {
  const r = await verifyQuorum(c.slot, c.root, c.atts, table);
  if (r.reached !== c.expectReached || r.attestedStake.toString() !== c.expectAttested) {
    fail++;
    console.error(`❌ ${c.name}: pritej ${c.expectReached}/${c.expectAttested}, erdhi ${r.reached}/${r.attestedStake}`);
  }
}

// Mesazhi: domen ‖ slot LE ‖ root.
const m = attestMessage(1, "ab".repeat(32));
if (bytesToHex(m.subarray(15, 23)) !== "0100000000000000" || m.length !== 15 + 8 + 32) {
  fail++;
  console.error("❌ attestMessage");
}

// getVoteAccounts → tabela (stake 0 hiqet, i njëjti node mblidhet).
const t = stakeTableFromVoteAccounts({
  current: [{ nodePubkey: "A", activatedStake: 5 }, { nodePubkey: "A", activatedStake: 2 }],
  delinquent: [{ nodePubkey: "B", activatedStake: 0 }],
});
if (t.get("A") !== 7n || t.has("B")) {
  fail++;
  console.error("❌ stakeTableFromVoteAccounts");
}

if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log(`✅ ${v.cases.length} vektorë firmash + 2 teste shtesë kaluan`);
