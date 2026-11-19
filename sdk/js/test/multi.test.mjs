// R5-2: multi-proof-i në JS jep të njëjtin verdikt si Rust-i për çdo pretendim.
import { readFileSync } from "node:fs";
import { verifyMultiProof, parseMultiProof, hexToBytes } from "../src/index.js";

const v = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")).multi;
let fail = 0;
for (const c of v.cases) {
  const got = await verifyMultiProof(c.root, c.claims, c.proof);
  if (JSON.stringify(got) !== JSON.stringify(c.expect)) {
    fail++;
    console.error(`❌ ${c.name}: pritej ${c.expect}, erdhi ${got}`);
  }
}
// Kodime jo kanonike → gabim (dhe "invalid").
{
  const c = v.cases.find((x) => x.name === "valid_12");
  const b = hexToBytes(c.proof);
  const bad = [
    new Uint8Array([...b, 0]),
    b.subarray(0, b.length - 1),
    Uint8Array.of(0x11, 0, 0, 0, 0, 0, 0),
    (() => { const x = new Uint8Array(b); x[0] = 2; return x; })(),
  ];
  for (const x of bad) {
    let threw = false;
    try { parseMultiProof(x); } catch { threw = true; }
    if (!threw) { fail++; console.error("❌ kodim jo kanonik u pranua"); }
  }
}
if (fail) {
  console.error(`❌ ${fail} raste multi-proof dështuan`);
  process.exit(1);
}
console.log(`✅ ${v.cases.length} raste multi-proof (Rust = JS)`);
