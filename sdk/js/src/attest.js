// Firmat e validatorëve mbi vulën e një sloti: "zilja/attest/v1" ‖ slot LE ‖ root.
// Përdor Ed25519 të WebCrypto (Node ≥ 22, Chrome/Edge ≥ 137, Firefox ≥ 129, Safari ≥ 17).
import { b58decode, hexToBytes } from "./verifier.js";

const enc = new TextEncoder();
const ATTEST_DOMAIN = enc.encode("zilja/attest/v1");

/** Mesazhi që firmos validatori për vulën e slotit. */
export function attestMessage(slot, rootHex) {
  const m = new Uint8Array(ATTEST_DOMAIN.length + 8 + 32);
  m.set(ATTEST_DOMAIN, 0);
  new DataView(m.buffer).setBigUint64(ATTEST_DOMAIN.length, BigInt(slot), true);
  m.set(hexToBytes(rootHex), ATTEST_DOMAIN.length + 8);
  return m;
}

async function ed25519Verify(pubkey, sig, msg) {
  try {
    const key = await globalThis.crypto.subtle.importKey("raw", pubkey, { name: "Ed25519" }, false, ["verify"]);
    return await globalThis.crypto.subtle.verify({ name: "Ed25519" }, key, sig, msg);
  } catch {
    return false;
  }
}

/**
 * Tabela e stake-ut nga `getVoteAccounts`: Map<nodePubkey base58, stake bigint>.
 * KUJDES: kjo tabelë i beson RPC-së që e jep.
 */
export function stakeTableFromVoteAccounts(result) {
  const t = new Map();
  for (const group of ["current", "delinquent"]) {
    for (const v of result?.[group] ?? []) {
      const s = BigInt(v.activatedStake ?? 0);
      if (s > 0n) t.set(v.nodePubkey, (t.get(v.nodePubkey) ?? 0n) + s);
    }
  }
  return t;
}

/**
 * Kontrollon firmat mbi (slot, root) dhe kuorumin > 2/3 të stake-ut.
 * @param {number|bigint} slot
 * @param {string} rootHex
 * @param {{validator: string, signature: string}[]} atts validator në base58, firma në hex
 * @param {Map<string, bigint>} table
 */
export async function verifyQuorum(slot, rootHex, atts, table) {
  const msg = attestMessage(slot, rootHex);
  const seen = new Set();
  let attested = 0n;
  let valid = 0;
  let rejected = 0;
  for (const a of atts) {
    if (seen.has(a.validator)) {
      rejected++;
      continue;
    }
    seen.add(a.validator);
    const stake = table.get(a.validator);
    if (!stake) {
      rejected++;
      continue;
    }
    let ok = false;
    try {
      const pk = b58decode(a.validator);
      const sig = hexToBytes(a.signature);
      ok = pk.length === 32 && sig.length === 64 && (await ed25519Verify(pk, sig, msg));
    } catch {
      ok = false;
    }
    if (ok) {
      valid++;
      attested += stake;
    } else {
      rejected++;
    }
  }
  let total = 0n;
  for (const s of table.values()) total += s;
  return {
    attestedStake: attested,
    totalStake: total,
    validSignatures: valid,
    rejected,
    reached: total > 0n && attested * 3n > total * 2n,
  };
}
