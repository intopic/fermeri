// Firmat e validatorëve: JS duhet të japë saktësisht të njëjtin vendim si Rust.
import { readFileSync } from "node:fs";
import { verifyQuorum, stakeTableFromVoteAccounts, attestMessage, bytesToHex } from "../src/index.js";

const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")).attest;
const table = new Map(v.table.map(([k, s]) => [k, BigInt(s)]));
let fail = 0;
for (const c of v.cases) {
  const r = await verifyQuorum(v.genesis, c.slot, c.root, c.atts, table);
  if (r.reached !== c.expectReached || r.attestedStake.toString() !== c.expectAttested) {
    fail++;
    console.error(`❌ ${c.name}: pritej ${c.expectReached}/${c.expectAttested}, erdhi ${r.reached}/${r.attestedStake}`);
  }
}

// Mesazhi: domen ‖ genesis ‖ slot LE ‖ root.
const m = attestMessage("cd".repeat(32), 1, "ab".repeat(32));
if (bytesToHex(m.subarray(47, 55)) !== "0100000000000000" || m.length !== 15 + 32 + 8 + 32 || m[15] !== 0xcd) {
  fail++;
  console.error("❌ attestMessage");
}

// Një zinxhir tjetër (genesis tjetër) → asnjë firmë s'vlen.
{
  const c = v.cases.find((x) => x.name === "quorum_reached");
  const r = await verifyQuorum("00".repeat(32), c.slot, c.root, c.atts, table);
  if (r.validSignatures !== 0) {
    fail++;
    console.error("❌ genesis tjetër u pranua");
  }
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
console.log(`✅ ${v.cases.length} vektorë firmash + 3 teste shtesë kaluan`);
