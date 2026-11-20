// Firmat e validatorëve, formati v2 (F-10): "zilja/attest/v2" ‖ genesis_hash ‖ slot LE ‖ root.
// Një çelës i veçantë mund të firmosë, kur identiteti e ka autorizuar për një periudhë epokash (v2):
// "zilja/authorize/v2" ‖ genesis_hash ‖ signer ‖ from_epoch LE ‖ until_epoch LE, e vlefshme për
// from_epoch ≤ epoka < until_epoch, me maksimumi MAX_AUTH_EPOCHS (15) epoka. Autorizimi v1 (pa
// afat) s'pranohet.
// Përdor Ed25519 të WebCrypto (Node ≥ 22, Chrome/Edge ≥ 137, Firefox ≥ 129, Safari ≥ 17).
import { b58decode, bytesToHex, hexToBytes, toU64 } from "./verifier.js";

const enc = new TextEncoder();
const ATTEST_DOMAIN = enc.encode("zilja/attest/v2");
const AUTHORIZE_DOMAIN = enc.encode("zilja/authorize/v2");
const REVOKE_DOMAIN = enc.encode("zilja/revoke/v1");
/** Gjatësia maksimale e një autorizimi, në epoka (15 ≈ 30 ditë në mainnet). */
export const MAX_AUTH_EPOCHS = 15n;

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
  // A16: vetëm u64 (BigInt, tekst dhjetor ose Number i sigurt); asgjë s'mbështillet në 64 bit.
  new DataView(m.buffer).setBigUint64(d + 32, toU64(slot), true);
  m.set(hex32(rootHex), d + 40);
  return m;
}

/** Mesazhi me të cilin identiteti autorizon çelësin që firmos, për epokat [fromEpoch, untilEpoch). */
export function authorizeMessage(genesisHex, signerB58, fromEpoch, untilEpoch) {
  const d = AUTHORIZE_DOMAIN.length;
  const m = new Uint8Array(d + 64 + 16);
  m.set(AUTHORIZE_DOMAIN, 0);
  m.set(hex32(genesisHex), d);
  const s = b58decode(signerB58);
  if (s.length !== 32) throw new Error("signer duhet 32 bajt");
  m.set(s, d + 32);
  const v = new DataView(m.buffer);
  v.setBigUint64(d + 64, toU64(fromEpoch), true);
  v.setBigUint64(d + 72, toU64(untilEpoch), true);
  return m;
}

/** R5-1: mesazhi me të cilin identiteti revokon përgjithmonë çelësin `signer`. */
export function revokeMessage(genesisHex, signerB58) {
  const d = REVOKE_DOMAIN.length;
  const m = new Uint8Array(d + 64);
  m.set(REVOKE_DOMAIN, 0);
  m.set(hex32(genesisHex), d);
  const s = b58decode(signerB58);
  if (s.length !== 32) throw new Error("signer duhet 32 bajt");
  m.set(s, d + 32);
  return m;
}

/**
 * R5-1: revokim i vlefshëm? Firma e identitetit `validator` mbi revokeMessage(genesis, signer).
 * Identiteti s'mund të revokojë veten.
 */
export async function verifyRevocation(genesisHex, r) {
  try {
    if (r == null || r.signer === r.validator) return false;
    const id = b58decode(r.validator);
    const sig = hexToBytes(r.signature);
    if (id.length !== 32 || sig.length !== 64) return false;
    return await ed25519Verify(id, sig, revokeMessage(genesisHex, r.signer));
  } catch {
    return false;
  }
}

/** Periudha 1..=15 epoka dhe e mbulon `epoch`? (Hedh gabim për numra jo-u64.) */
function authCovers(au, epoch) {
  const from = toU64(au.fromEpoch);
  const until = toU64(au.untilEpoch);
  const e = toU64(epoch);
  return until > from && until - from <= MAX_AUTH_EPOCHS && from <= e && e < until;
}

/**
 * L1: kodimet (y, me bitin e shenjës të hequr) e pikave me rend të vogël (rendi i 8-torsionit), plus
 * kodimet jo-kanonike y = p dhe y = p + 1. WebCrypto i pranon këto si `A` ose `R`; Rust
 * `verify_strict` jo. I refuzojmë që JS dhe Rust të japin të njëjtin vendim (si libsodium).
 */
const SMALL_ORDER_Y = [
  "0000000000000000000000000000000000000000000000000000000000000000", // y = 0
  "0100000000000000000000000000000000000000000000000000000000000000", // y = 1 (identiteti)
  "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", // y = p - 1
  "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a", // rend 8
  "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05", // rend 8
  "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", // y = p (jo-kanonik)
  "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", // y = p + 1 (jo-kanonik)
];

/** A ka 32 bajtet e një pike me rend të vogël (pavarësisht bitit të shenjës)? */
export function isSmallOrderEncoding(b) {
  if (b.length !== 32) return true;
  const y = Uint8Array.from(b);
  y[31] &= 0x7f;
  const hex = bytesToHex(y);
  return SMALL_ORDER_Y.includes(hex);
}

async function ed25519Verify(pubkey, sig, msg) {
  if (isSmallOrderEncoding(pubkey) || isSmallOrderEncoding(sig.subarray(0, 32))) return false;
  try {
    const key = await globalThis.crypto.subtle.importKey("raw", pubkey, { name: "Ed25519" }, false, ["verify"]);
    return await globalThis.crypto.subtle.verify({ name: "Ed25519" }, key, sig, msg);
  } catch {
    return false;
  }
}

/**
 * Tabela e stake-ut nga `getVoteAccounts`: Map<nodePubkey base58, stake bigint>.
 *
 * KUJDES (A10): kjo është pamja e RPC-së (`bank.vote_accounts()` i bankës së saj), jo tabela e
 * konsensusit e epokës; i besohet RPC-së që e jep. Merren vetëm llogaritë me `epochVoteAccount`.
 * A11: kërkohen të dy grupet; çelësat duhet të jenë 32 bajt; `activatedStake` duhet të jetë u64 i
 * saktë (BigInt / tekst dhjetor / Number i sigurt). Çdo vlerë e pasigurt hedh gabim.
 */
export function stakeTableFromVoteAccounts(result) {
  const t = new Map();
  for (const group of ["current", "delinquent"]) {
    const list = result?.[group];
    if (!Array.isArray(list)) throw new Error(`getVoteAccounts pa grupin "${group}"`);
    for (const v of list) {
      if (b58decode(String(v?.votePubkey ?? "")).length !== 32 || b58decode(String(v?.nodePubkey ?? "")).length !== 32) throw new Error("getVoteAccounts: çelës i pavlefshëm");
      const s = toU64(v.activatedStake);
      if (v.epochVoteAccount !== true || s === 0n) continue;
      const sum = (t.get(v.nodePubkey) ?? 0n) + s;
      if (sum > 0xffffffffffffffffn) throw new Error("getVoteAccounts: stake jashtë u64");
      t.set(v.nodePubkey, sum);
    }
  }
  return t;
}

/**
 * A është kjo firmë e vlefshme për (genesis, slot, root)? `epoch` është epoka e `slot`-it; një
 * autorizim që s'e mbulon s'vlen.
 */
export async function verifyAttestation(genesisHex, epoch, slot, rootHex, a) {
  try {
    const id = b58decode(a.validator);
    const sig = hexToBytes(a.signature);
    if (id.length !== 32 || sig.length !== 64) return false;
    let signer = id;
    if (a.signer != null && a.signer !== a.validator) {
      const au = a.authorization;
      // v2: objekt {fromEpoch, untilEpoch, signature}. Një tekst (v1, pa afat) s'pranohet.
      if (au == null || typeof au !== "object" || !authCovers(au, epoch)) return false;
      const asig = hexToBytes(au.signature);
      if (asig.length !== 64 || !(await ed25519Verify(id, asig, authorizeMessage(genesisHex, a.signer, au.fromEpoch, au.untilEpoch)))) return false;
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
 * @param {number|bigint} epoch epoka e `slot`-it (ajo e tabelës së stake-ut)
 * @param {number|bigint} slot
 * @param {string} rootHex
 * @param {{validator: string, signer?: string|null, authorization?: {fromEpoch: number|bigint, untilEpoch: number|bigint, signature: string}|null, signature: string}[]} atts
 * @param {Map<string, bigint>} table
 * @param {{validator: string, signer: string, signature: string}[]} [revocations] R5-1: nga cilido
 *   burim; secila verifikohet, të falsifikuarat injorohen.
 */
export async function verifyQuorum(genesisHex, epoch, slot, rootHex, atts, table, revocations = []) {
  const revoked = new Set();
  for (const r of revocations ?? []) {
    if (await verifyRevocation(genesisHex, r)) revoked.add(`${r.validator}:${r.signer}`);
  }
  const seen = new Set();
  let attested = 0n;
  let valid = 0;
  let rejected = 0;
  for (const a of atts) {
    // A15: identiteti shënohet vetëm pas një firme të vlefshme: rezultati s'varet nga radha.
    if (seen.has(a.validator)) {
      rejected++;
      continue;
    }
    const stake = table.get(a.validator);
    if (!stake) {
      rejected++;
      continue;
    }
    if (a.signer != null && a.signer !== a.validator && revoked.has(`${a.validator}:${a.signer}`)) {
      rejected++;
      continue;
    }
    if (await verifyAttestation(genesisHex, epoch, slot, rootHex, a)) {
      seen.add(a.validator);
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
