// Firmat e validatorëve, formati v2 (F-10): "zilja/attest/v2" ‖ genesis_hash ‖ slot LE ‖ root.
// Një çelës i veçantë mund të firmosë, kur identiteti e ka autorizuar me
// "zilja/authorize/v1" ‖ genesis_hash ‖ signer.
// Përdor Ed25519 të WebCrypto (Node ≥ 22, Chrome/Edge ≥ 137, Firefox ≥ 129, Safari ≥ 17).
import { b58decode, hexToBytes } from "./verifier.js";

const enc = new TextEncoder();
const ATTEST_DOMAIN = enc.encode("zilja/attest/v2");
const AUTHORIZE_DOMAIN = enc.encode("zilja/authorize/v1");

function hex32(h) {
  const b = hexToBytes(h);
  if (b.length !== 32) throw new Error("duhen 32 bajt");
  return b;
}

/** Mesazhi që firmos validatori për vulën e slotit. */
export function attestMessage(genesisHex, slot, rootHex) {
  const d = ATTEST_DOMAIN.length;
  const m = new Uint8Array(d + 32 + 8 + 32);
  m.set(ATTEST_DOMAIN, 0);
  m.set(hex32(genesisHex), d);
  new DataView(m.buffer).setBigUint64(d + 32, BigInt(slot), true);
  m.set(hex32(rootHex), d + 40);
  return m;
}

/** Mesazhi me të cilin identiteti autorizon çelësin që firmos. */
export function authorizeMessage(genesisHex, signerB58) {
  const d = AUTHORIZE_DOMAIN.length;
  const m = new Uint8Array(d + 64);
  m.set(AUTHORIZE_DOMAIN, 0);
  m.set(hex32(genesisHex), d);
  const s = b58decode(signerB58);
  if (s.length !== 32) throw new Error("signer duhet 32 bajt");
  m.set(s, d + 32);
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

/** A është kjo firmë e vlefshme për (genesis, slot, root)? */
export async function verifyAttestation(genesisHex, slot, rootHex, a) {
  try {
    const id = b58decode(a.validator);
    const sig = hexToBytes(a.signature);
    if (id.length !== 32 || sig.length !== 64) return false;
    let signer = id;
    if (a.signer != null && a.signer !== a.validator) {
      if (a.authorization == null) return false;
      const auth = hexToBytes(a.authorization);
      if (auth.length !== 64 || !(await ed25519Verify(id, auth, authorizeMessage(genesisHex, a.signer)))) return false;
      signer = b58decode(a.signer);
      if (signer.length !== 32) return false;
    }
    return await ed25519Verify(signer, sig, attestMessage(genesisHex, slot, rootHex));
  } catch {
    return false;
  }
}

/**
 * Kontrollon firmat mbi (genesis, slot, root) dhe kuorumin > 2/3 të stake-ut.
 * @param {string} genesisHex hash-i i genesis-it të zinxhirit që pret klienti
 * @param {number|bigint} slot
 * @param {string} rootHex
 * @param {{validator: string, signer?: string|null, authorization?: string|null, signature: string}[]} atts
 * @param {Map<string, bigint>} table
 */
export async function verifyQuorum(genesisHex, slot, rootHex, atts, table) {
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
    if (await verifyAttestation(genesisHex, slot, rootHex, a)) {
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
