// R5-4: lidhje "drop-in" për kuletat dhe dApp-et: të njëjtat thirrje si @solana/web3.js
// (`Connection`) dhe @solana/kit (`rpc.x().send()`), por çdo llogari kthehet vetëm pasi u verifikua
// kundër Ziles: dëftesa (një multi-proof për shumë llogari), besimi te root-i (minTrust), freskia, dhe
// bajtet e RPC-së të krahasuara me `dataHash` të vulosur. Pa varësi: s'importon web3.js ose kit.
import { ZiljaClient } from "./client.js";
import { b58decode, MAX_MULTI_KEYS } from "./verifier.js";

export const TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
export const TOKEN_2022_PROGRAM = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/** Hidhet kur një llogari s'mund të verifikohet; `results` mban arsyet (errors, checks). */
export class VerificationError extends Error {
  constructor(message, results) {
    super(message);
    this.name = "VerificationError";
    this.results = results;
  }
}

const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function b58encode(bytes) {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
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
}

/** PublicKey (web3.js), Address (kit, tekst) ose tekst base58 → tekst base58 i kontrolluar. */
function addr(x) {
  const s = typeof x === "string" ? x : typeof x?.toBase58 === "function" ? x.toBase58() : String(x);
  if (b58decode(s).length !== 32) throw new Error(`adresë e pavlefshme: ${s}`);
  return s;
}

const u64le = (b, o) => {
  let v = 0n;
  for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[o + i]);
  return v;
};

/** G12: Multisig i SPL Token ka po këtë gjatësi; Token-2022 s'krijon kurrë llogari me zgjerime me këtë gjatësi. */
const MULTISIG_LEN = 355;
const ACCOUNT_LEN = 165;
const MINT_LEN = 82;

/** COption<T>: etiketa u32 LE duhet 0 ose 1; çdo vlerë tjetër = të dhëna që s'janë të këtij lloji. */
function coptionAt(data, o) {
  const tag = (data[o] | (data[o + 1] << 8) | (data[o + 2] << 16)) + data[o + 3] * 2 ** 24;
  if (tag !== 0 && tag !== 1) throw new Error(`etiketë COption e pavlefshme (${tag}) te bajti ${o}`);
  return tag === 1;
}

/**
 * G12: lloji i llogarisë sipas gjatësisë: ekzaktësisht 165 (Account) / 82 (Mint), ose Token-2022
 * me zgjerime: > 165 bajte, jo 355 (Multisig), me AccountType te bajti 165 (1 = Mint, 2 = Account).
 */
function tokenKind(data) {
  if (!(data instanceof Uint8Array)) return null;
  if (data.length === ACCOUNT_LEN) return "account";
  if (data.length === MINT_LEN) return "mint";
  if (data.length > ACCOUNT_LEN && data.length !== MULTISIG_LEN) return data[ACCOUNT_LEN] === 2 ? "account" : data[ACCOUNT_LEN] === 1 ? "mint" : null;
  return null;
}

/**
 * SPL Token / Token-2022: pjesa bazë e llogarisë (165 bajtet e para). Hedh gabim kur bajtet s'janë
 * një llogari tokeni (G12: gjatësi, Multisig, AccountType, gjendje ose COption të pavlefshme). Mban
 * vetëm fushat; programin pronar e kontrollon thirrësi.
 */
export function parseTokenAccount(data) {
  if (tokenKind(data) !== "account") throw new Error("s'është llogari tokeni (gjatësia ose AccountType)");
  const state = data[108];
  if (state === 0) throw new Error("llogari tokeni e painicializuar");
  if (state !== 1 && state !== 2) throw new Error(`gjendje e panjohur e llogarisë së tokenit (${state})`);
  const hasDelegate = coptionAt(data, 72);
  const native = coptionAt(data, 109);
  const hasClose = coptionAt(data, 129);
  return {
    mint: b58encode(data.subarray(0, 32)),
    owner: b58encode(data.subarray(32, 64)),
    amount: u64le(data, 64),
    delegate: hasDelegate ? b58encode(data.subarray(76, 108)) : null,
    state: state === 1 ? "initialized" : "frozen",
    isNative: native ? u64le(data, 113) : null,
    delegatedAmount: u64le(data, 121),
    closeAuthority: hasClose ? b58encode(data.subarray(133, 165)) : null,
  };
}

/** SPL Mint: `decimals` dhe `supply` (82 bajtet e para). G12: vetëm një mint i vërtetë (gjatësia, AccountType, COption). */
export function parseMint(data) {
  if (tokenKind(data) !== "mint") throw new Error("s'është mint (gjatësia ose AccountType)");
  if (data[45] === 0) throw new Error("mint i painicializuar");
  if (data[45] !== 1) throw new Error(`fushë is_initialized e pavlefshme (${data[45]})`);
  coptionAt(data, 0);
  coptionAt(data, 46);
  return { supply: u64le(data, 36), decimals: data[44] };
}

/**
 * Uint8Array → base64. Pa `...u8` (spread): një llogari 10 MB do ta tejkalonte kufirin e
 * argumenteve të funksionit; ndahet në copa 32 KB.
 */
export function bytesToBase64(u8) {
  if (typeof Buffer !== "undefined") return Buffer.from(u8).toString("base64");
  let s = "";
  for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
  return btoa(s);
}

function uiAmountString(amount, decimals) {
  const s = amount.toString().padStart(decimals + 1, "0");
  const int = s.slice(0, s.length - decimals);
  const frac = s.slice(s.length - decimals).replace(/0+$/, "");
  return frac ? `${int}.${frac}` : int;
}

/**
 * Si `Connection` i web3.js, për leximin e llogarive, me verifikim. `zilja` është një ZiljaClient
 * ose opsionet e tij (duhet `solanaRpc`, që të merren bajtet dhe të krahasohen).
 * `PublicKey` (opsional): konstruktori i web3.js, që `owner` të kthehet si PublicKey.
 * `onUnverified`: "throw" (parazgjedhja) ose "null".
 */
export class VerifiableConnection {
  constructor({ zilja, PublicKey = null, onUnverified = "throw" } = {}) {
    this.client = zilja instanceof ZiljaClient ? zilja : new ZiljaClient(zilja ?? {});
    if (!this.client.solanaRpc || this.client.compareData === false) {
      throw new Error("VerifiableConnection kërkon solanaRpc dhe compareData: bajtet duhet të krahasohen me vulën");
    }
    this.PublicKey = PublicKey;
    this.onUnverified = onUnverified;
  }

  /**
   * Verifikon të gjitha me një multi-proof (≤ 256; listat më të gjata ndahen në copa, secila me
   * slotin e vet: `slot` i çdo elementi është i copës së tij); kthen [{ok, exists, account, bytes, slot}] ose hedh.
   */
  async verified(pubkeys) {
    const keys = pubkeys.map(addr);
    // Multi-proof-i mban ≤ 256 llogari: listat më të gjata ndahen.
    const rs = [];
    if (keys.length === 1) rs.push(await this.client.verifyAccount(keys[0]));
    else for (let i = 0; i < keys.length; i += 256) rs.push(...(await this.client.verifyAccounts(keys.slice(i, i + 256))));
    const bad = rs.filter((r) => !r.ok);
    if (bad.length && this.onUnverified === "throw") {
      const why = bad[0].errors?.join(", ") || bad[0].verdict;
      throw new VerificationError(`${bad.length}/${keys.length} llogari s'u verifikuan (${why})`, rs);
    }
    return rs.map((r) => ({
      ok: r.ok,
      exists: r.ok && r.verdict === "exists",
      account: r.ok ? r.account : null,
      bytes: r.ok && r.verdict === "exists" ? r.data?.bytes ?? null : null,
      slot: r.slot,
    }));
  }

  toInfo(v) {
    if (!v.exists) return null;
    if (v.bytes == null) {
      if (this.onUnverified === "throw") throw new VerificationError("të dhënat s'u verifikuan", [v]);
      return null;
    }
    const lamports = v.account.lamports;
    if (lamports > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("lamports > 2^53: përdor createVerifiedRpc (bigint)");
    return {
      lamports: Number(lamports),
      owner: this.PublicKey ? new this.PublicKey(v.account.owner) : v.account.owner,
      executable: v.account.executable,
      data: v.bytes,
      rentEpoch: 0,
      space: v.bytes.length,
    };
  }

  /** web3.js: `getAccountInfo(publicKey)` → AccountInfo | null. */
  async getAccountInfo(pubkey) {
    const [v] = await this.verified([pubkey]);
    return this.toInfo(v);
  }

  /** web3.js: `getMultipleAccountsInfo(publicKeys)` → (AccountInfo | null)[], me një multi-proof. */
  async getMultipleAccountsInfo(pubkeys) {
    if (pubkeys.length === 0) return [];
    return (await this.verified(pubkeys)).map((v) => this.toInfo(v));
  }

  /** web3.js: `getBalance(publicKey)` → lamports (0 kur llogaria s'ekziston). */
  async getBalance(pubkey) {
    const [v] = await this.verified([pubkey]);
    if (!v.ok) return null;
    if (!v.exists) return 0;
    if (v.account.lamports > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("lamports > 2^53: përdor createVerifiedRpc (bigint)");
    return Number(v.account.lamports);
  }

  /** Llogaria e tokenit, e verifikuar dhe e lexuar; programi pronar duhet të jetë Token ose Token-2022. */
  async getTokenAccount(pubkey) {
    const info = await this.getAccountInfo(pubkey);
    if (!info) return null;
    const owner = typeof info.owner === "string" ? info.owner : info.owner.toBase58();
    if (owner !== TOKEN_PROGRAM && owner !== TOKEN_2022_PROGRAM) throw new Error(`s'është llogari tokeni (pronari ${owner})`);
    return { ...parseTokenAccount(info.data), program: owner };
  }

  /**
   * web3.js: `getTokenAccountBalance(publicKey)` → {value: {amount, decimals, uiAmountString}}.
   * Dy verifikime njëra pas tjetrës: llogaria e tokenit, pastaj mint-i i saj (decimals). S'mund të
   * bëhen me një multi-proof, sepse adresa e mint-it dihet vetëm pasi lexohet llogaria e tokenit.
   */
  async getTokenAccountBalance(pubkey) {
    const ta = await this.getTokenAccount(pubkey);
    if (!ta) throw new Error("llogaria e tokenit s'ekziston");
    const [m] = await this.verified([ta.mint]);
    const mi = this.toInfo(m);
    if (!mi) throw new Error("mint-i s'ekziston");
    const mintOwner = typeof mi.owner === "string" ? mi.owner : mi.owner.toBase58();
    if (mintOwner !== ta.program) throw new Error("mint-i s'i përket të njëjtit program tokenash");
    const { decimals } = parseMint(mi.data);
    return { value: { amount: ta.amount.toString(), decimals, uiAmountString: uiAmountString(ta.amount, decimals) } };
  }
}

/**
 * Për @solana/kit: një objekt me të njëjtat metoda si `rpc` (`getAccountInfo(address, config).send()`,
 * `getMultipleAccounts`, `getBalance`), që kthen vetëm llogari të verifikuara (lamports si bigint,
 * data si [base64, "base64"]).
 */
/**
 * G24: konfigurimi i kit-it që respektohet. `commitment`: çdo vlerë e kit-it pranohet, sepse zilja jep
 * gjendjen e një sloti të rrënjosur (≥ çdo nivel). `encoding`: vetëm "base64" (ose pa të), sepse kjo
 * është forma që kthehet. `minContextSlot`: përgjigja duhet të jetë të paktën në atë slot. Çdo çelës
 * tjetër (p.sh. `dataSlice`) hedh gabim, në vend që të injorohet pa zë.
 */
function kitConfig(method, cfg, { encoding }) {
  if (cfg == null) return {};
  if (typeof cfg !== "object") throw new Error(`${method}: konfigurimi duhet objekt`);
  const allowed = encoding ? ["commitment", "encoding", "minContextSlot"] : ["commitment", "minContextSlot"];
  for (const k of Object.keys(cfg)) {
    if (cfg[k] === undefined) continue;
    if (!allowed.includes(k)) throw new Error(`${method}: opsioni "${k}" s'mbështetet nga leximi i verifikuar`);
  }
  if (cfg.commitment !== undefined && !["processed", "confirmed", "finalized"].includes(cfg.commitment)) {
    throw new Error(`${method}: commitment i panjohur "${cfg.commitment}"`);
  }
  if (cfg.encoding !== undefined && cfg.encoding !== "base64") throw new Error(`${method}: vetëm encoding "base64" mbështetet`);
  let min = null;
  if (cfg.minContextSlot !== undefined) {
    const m = cfg.minContextSlot;
    if (typeof m === "bigint" ? m < 0n : !Number.isSafeInteger(m) || m < 0) throw new Error(`${method}: minContextSlot duhet numër i plotë jo negativ`);
    min = BigInt(m);
  }
  return { min };
}

/** G24: si RPC-ja e Solana-s (-32016): përgjigjja nën `minContextSlot` s'kthehet. */
function checkMinSlot(method, min, slot) {
  if (min != null && (slot == null || BigInt(slot) < min)) {
    throw new Error(`${method}: sloti minimal i kontekstit (${min}) s'është arritur (zilja: ${slot ?? "pa slot"})`);
  }
}

export function createVerifiedRpc(zilja, { onUnverified = "throw" } = {}) {
  const conn = new VerifiableConnection({ zilja, onUnverified });
  const b64 = bytesToBase64;
  const kitInfo = (v) =>
    !v.exists || v.bytes == null
      ? null
      : { lamports: v.account.lamports, owner: v.account.owner, executable: v.account.executable, data: [b64(v.bytes), "base64"], space: BigInt(v.bytes.length) };
  const wrap = (fn) => ({ send: fn });
  return {
    getAccountInfo: (address, config) =>
      wrap(async () => {
        const { min } = kitConfig("getAccountInfo", config, { encoding: true });
        const [v] = await conn.verified([address]);
        checkMinSlot("getAccountInfo", min, v.slot);
        return { context: { slot: BigInt(v.slot ?? 0) }, value: kitInfo(v) };
      }),
    getMultipleAccounts: (addresses, config) =>
      wrap(async () => {
        const { min } = kitConfig("getMultipleAccounts", config, { encoding: true });
        if (addresses.length === 0) return { context: { slot: 0n }, value: [] };
        // G02: një `context.slot` i vetëm pohon një pamje të vetme. Mbi një multi-proof (256), copat
        // do të ishin në slote të ndryshme dhe pamja e bashkuar mund të mos ketë ekzistuar kurrë.
        // (Edhe RPC-ja e Solana-s pranon vetëm ≤ 100 adresa për kërkesë.)
        if (addresses.length > MAX_MULTI_KEYS) {
          throw new Error(`getMultipleAccounts: ≤ ${MAX_MULTI_KEYS} adresa për kërkesë (një multi-proof, një slot); ndaje listën`);
        }
        const vs = await conn.verified(addresses);
        const slots = new Set(vs.map((v) => v.slot));
        if (slots.size > 1) throw new Error("getMultipleAccounts: multi-proof-i ktheu slote të ndryshme");
        checkMinSlot("getMultipleAccounts", min, vs[0]?.slot);
        return { context: { slot: BigInt(vs[0]?.slot ?? 0) }, value: vs.map(kitInfo) };
      }),
    getBalance: (address, config) =>
      wrap(async () => {
        const { min } = kitConfig("getBalance", config, { encoding: false });
        const [v] = await conn.verified([address]);
        checkMinSlot("getBalance", min, v.slot);
        // G01: 0n vetëm për mungesë të provuar; një verifikim i dështuar (onUnverified: "null")
        // kthen null, si getBalance i web3.js, që kuleta të mos shfaqë "0" për diçka të paverifikuar.
        if (!v.ok) return { context: { slot: BigInt(v.slot ?? 0) }, value: null };
        return { context: { slot: BigInt(v.slot ?? 0) }, value: v.exists ? v.account.lamports : 0n };
      }),
  };
}
