// Verifikuesi i Ziles në JavaScript, pa varësi (WebCrypto). Funksionon në
// shfletues dhe në Node ≥ 18. Ndjek saktë docs/SPEC.md.
//
//   import { verifyAccount } from "./zilja-verifier.js";
//   const v = await verifyAccount(rootHex, pubkeyB58, account /* ose null */, proofHex);
//   // v = "exists" | "does_not_exist" | "invalid"

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();
const ACCOUNT_DOMAIN = enc.encode("zilja/account/v1");
const EMPTY = new Uint8Array(32);
const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

const HEX = /^(?:[0-9a-fA-F]{2})*$/;
const U64_MAX = (1n << 64n) - 1n;

/** Hex i rreptë (L-03): vetëm shifra hex, gjatësi çift. */
export function hexToBytes(h) {
  if (typeof h !== "string" || !HEX.test(h)) throw new Error("hex i pavlefshëm");
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

function hex32(h) {
  const b = hexToBytes(h);
  if (b.length !== 32) throw new Error("duhen 32 bajt");
  return b;
}

/**
 * u64 i rreptë (F-03, F-08): bigint, tekst dhjetor ose numër i plotë i sigurt, në [0, 2^64).
 * Një numër JSON mbi 2^53 nuk pranohet, sepse JS e ka rrumbullakuar tashmë.
 */
export function toU64(v) {
  let n;
  if (typeof v === "bigint") n = v;
  else if (typeof v === "string" && /^(0|[1-9][0-9]{0,19})$/.test(v)) n = BigInt(v);
  else if (typeof v === "number" && Number.isSafeInteger(v)) n = BigInt(v);
  else throw new Error("u64 i pavlefshëm");
  if (n < 0n || n > U64_MAX) throw new Error("u64 jashtë kufijve");
  return n;
}

/**
 * Kontrollon dhe kthen llogarinë në formën kanonike (F-03):
 * `{lamports: bigint, owner: string, executable: boolean, dataLen: bigint, dataHash: string}`.
 * Hedh gabim për çdo fushë që mungon, ka llojin e gabuar ose del jashtë kufijve.
 */
export function normalizeAccount(a) {
  if (a === null || typeof a !== "object") throw new Error("llogari e pavlefshme");
  if (typeof a.executable !== "boolean") throw new Error("executable duhet boolean");
  if (typeof a.owner !== "string") throw new Error("owner duhet tekst");
  key32(a.owner);
  const dataHash = bytesToHex(hex32(a.dataHash));
  return { lamports: toU64(a.lamports), owner: a.owner, executable: a.executable, dataLen: toU64(a.dataLen), dataHash };
}

export function bytesToHex(b) {
  return Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
}

export function b58decode(s) {
  if (typeof s !== "string" || s.length === 0 || s.length > 64) throw new Error("base58 i pavlefshëm");
  let n = 0n;
  for (const c of s) {
    const i = B58.indexOf(c);
    if (i < 0) throw new Error("base58 i pavlefshëm");
    n = n * 58n + BigInt(i);
  }
  const bytes = [];
  while (n > 0n) {
    bytes.push(Number(n & 0xffn));
    n >>= 8n;
  }
  for (const c of s) {
    if (c !== "1") break;
    bytes.push(0);
  }
  return Uint8Array.from(bytes.reverse());
}

function key32(s) {
  const k = b58decode(s);
  if (k.length !== 32) throw new Error("duhen 32 bajt");
  return k;
}

function u64le(v) {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(v), true);
  return b;
}

function concat(...parts) {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

export async function sha256(data) {
  return new Uint8Array(await subtle.digest("SHA-256", data));
}

const eq = (a, b) => a.length === b.length && a.every((x, i) => x === b[i]);
const bit = (k, i) => (k[i >> 3] >> (7 - (i & 7))) & 1;

function commonPrefixLen(a, b) {
  for (let i = 0; i < 32; i++) {
    const d = a[i] ^ b[i];
    if (d) return i * 8 + (Math.clz32(d) - 24);
  }
  return 256;
}

/** value_hash sipas SPEC.md. `account` = {lamports, owner, executable, dataLen, dataHash}. */
export async function valueHash(account) {
  const a = normalizeAccount(account);
  return sha256(
    concat(ACCOUNT_DOMAIN, u64le(a.lamports), key32(a.owner), Uint8Array.of(a.executable ? 1 : 0), u64le(a.dataLen), hex32(a.dataHash)),
  );
}

/** Lexon formatin binar të dëftesës. */
export function parseProof(bytes) {
  let p = 0;
  const take = (n) => {
    if (p + n > bytes.length) throw new Error("dëftesë e cunguar");
    const s = bytes.subarray(p, p + n);
    p += n;
    return s;
  };
  const version = take(1)[0];
  if (version !== 1) throw new Error(`version i panjohur ${version}`);
  const d = take(2);
  const depth = d[0] | (d[1] << 8);
  if (depth > 256) throw new Error("shumë e thellë");
  const bitmap = take(Math.ceil(depth / 8));
  const siblings = [];
  for (let i = 0; i < depth; i++) {
    siblings.push((bitmap[i >> 3] >> (7 - (i & 7))) & 1 ? take(32) : EMPTY);
  }
  const flag = take(1)[0];
  let leaf = null;
  if (flag === 1) leaf = { key: take(32), valueHash: take(32) };
  else if (flag !== 0) throw new Error("flamur i pavlefshëm gjetheje");
  if (p !== bytes.length) throw new Error("bajte të tepërta");
  return { siblings, leaf };
}

async function computeRoot(key, proof) {
  const depth = proof.siblings.length;
  let cur;
  if (proof.leaf) {
    if (commonPrefixLen(proof.leaf.key, key) < depth) return null;
    cur = await sha256(concat(Uint8Array.of(0), proof.leaf.key, proof.leaf.valueHash));
  } else {
    cur = EMPTY;
  }
  for (let i = depth - 1; i >= 0; i--) {
    const s = proof.siblings[i];
    cur = await sha256(bit(key, i) === 0 ? concat(Uint8Array.of(1), cur, s) : concat(Uint8Array.of(1), s, cur));
  }
  return cur;
}

/**
 * Verifikimi kryesor.
 * @param {string} rootHex root-i (vula) në hex
 * @param {string} pubkeyB58 llogaria
 * @param {object|null} account pretendimi; null = "nuk ekziston"
 * @param {string} proofHex dëftesa në hex
 * @returns {Promise<"exists"|"does_not_exist"|"invalid">}
 */
export async function verifyAccount(rootHex, pubkeyB58, account, proofHex) {
  try {
    const root = hex32(rootHex);
    const key = key32(pubkeyB58);
    const proof = parseProof(hexToBytes(proofHex));
    if (account) {
      if (!proof.leaf || !eq(proof.leaf.key, key)) return "invalid";
      if (!eq(proof.leaf.valueHash, await valueHash(account))) return "invalid";
    } else if (proof.leaf && eq(proof.leaf.key, key)) {
      return "invalid";
    }
    const got = await computeRoot(key, proof);
    if (!got || !eq(got, root)) return "invalid";
    return account ? "exists" : "does_not_exist";
  } catch {
    return "invalid";
  }
}

/** Kontrollon që të dhënat (nga çdo RPC) përputhen me dataHash/dataLen të vulosur. */
export async function dataMatches(account, dataBytes) {
  return toU64(account.dataLen) === BigInt(dataBytes.length) && eq(await sha256(dataBytes), hex32(account.dataHash));
}
