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

// getVoteAccounts → tabela (stake 0 hiqet, i njëjti node mblidhet, vetëm epochVoteAccount).
const K1 = "11111111111111111111111111111111", K2 = "SysvarS1otHashes111111111111111111111111111", K3 = "Vote111111111111111111111111111111111111111";
const va = (node, vote, stake, epochVoteAccount = true) => ({ nodePubkey: node, votePubkey: vote, activatedStake: stake, epochVoteAccount });
const t = stakeTableFromVoteAccounts({
  current: [va(K1, K3, 5), va(K1, K2, 2), va(K3, K1, 9, false)],
  delinquent: [va(K2, K3, 0)],
});
if (t.get(K1) !== 7n || t.has(K2) || t.has(K3)) {
  fail++;
  console.error("❌ stakeTableFromVoteAccounts", t);
}
// A11: stake mbi 2^53 lexohet saktë nga teksti; Number i pasigurt, grup që mungon ose çelës i
// pavlefshëm refuzohen (s'bëhen 0 në heshtje).
const big = stakeTableFromVoteAccounts({ current: [va(K1, K3, "9007199254740993")], delinquent: [] });
if (big.get(K1) !== 9007199254740993n) {
  fail++;
  console.error("❌ stake mbi 2^53 humbi saktësi", big);
}
for (const [name, r] of [
  ["Number i pasigurt", { current: [va(K1, K3, 9007199254740993)], delinquent: [] }],
  ["pa delinquent", { current: [va(K1, K3, 5)] }],
  ["çelës i pavlefshëm", { current: [va("A", K3, 5)], delinquent: [] }],
  ["stake negativ", { current: [va(K1, K3, -1)], delinquent: [] }],
]) {
  let threw = false;
  try {
    stakeTableFromVoteAccounts(r);
  } catch {
    threw = true;
  }
  if (!threw) {
    fail++;
    console.error(`❌ A11: ${name} u pranua`);
  }
}
// A16: sloti jashtë u64 s'mbështillet në 64 bit.
for (const bad of [2n ** 64n, -1n, -1, 2 ** 53, 1.5, "18446744073709551616"]) {
  let threw = false;
  try {
    attestMessage("11".repeat(32), bad, "22".repeat(32));
  } catch {
    threw = true;
  }
  if (!threw) {
    fail++;
    console.error(`❌ A16: slot ${bad} u pranua`);
  }
}
// A15: një firmë e pavlefshme e një identiteti s'e fshin të vlefshmen; radha s'ka rëndësi.
{
  const { generateKeyPairSync, sign } = await import("node:crypto");
  const keys = [0, 1, 2].map(() => generateKeyPairSync("ed25519"));
  const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  const b58 = (bytes) => {
    let n = BigInt("0x" + Buffer.from(bytes).toString("hex"));
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
  };
  const ids = keys.map((k) => b58(new Uint8Array(k.publicKey.export({ format: "der", type: "spki" }).subarray(-32))));
  const G = "33".repeat(32), R = "44".repeat(32);
  const good = (i) => ({ validator: ids[i], signer: null, authorization: null, signature: Buffer.from(sign(null, attestMessage(G, 9, R), keys[i].privateKey)).toString("hex") });
  const bad = (i) => ({ ...good(i), signature: "01".repeat(64) });
  const table = new Map(ids.map((id) => [id, 10n]));
  const atts = [bad(0), good(0), good(1), good(2)];
  const a = await verifyQuorum(G, 9, R, atts, table);
  const b = await verifyQuorum(G, 9, R, [...atts].reverse(), table);
  if (!a.reached || !b.reached || a.attestedStake !== b.attestedStake) {
    fail++;
    console.error("❌ A15: firma e pavlefshme fshehu të vlefshmen ose radha ndryshoi rezultatin", a, b);
  }
}

if (fail) {
  console.error(`${fail} gabime`);
  process.exit(1);
}
console.log(`✅ ${v.cases.length} vektorë firmash + 3 teste shtesë kaluan`);
