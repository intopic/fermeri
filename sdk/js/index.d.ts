export type Verdict = "exists" | "does_not_exist" | "invalid";
export type Trust = "single" | "agreement" | "quorum";

/** Llogaria siç e jep Zilja (`getAccountProof`). */
export interface ZiljaAccount {
  /** u64: tekst dhjetor (si e jep RPC-ja), bigint, ose numër i plotë ≤ 2^53. */
  lamports: number | string | bigint;
  owner: string;
  executable: boolean;
  dataLen: number | string | bigint;
  dataHash: string;
}

/** Verifikimi i ulët: root, pubkey, pretendimi (ose null = "s'ekziston"), dëftesa. */
export function verifyAccount(rootHex: string, pubkeyB58: string, account: ZiljaAccount | null, proofHex: string): Promise<Verdict>;
export function dataMatches(account: Pick<ZiljaAccount, "dataLen" | "dataHash">, data: Uint8Array): Promise<boolean>;
export function valueHash(account: ZiljaAccount): Promise<Uint8Array>;
export function parseProof(bytes: Uint8Array): { siblings: Uint8Array[]; leaf: { key: Uint8Array; valueHash: Uint8Array } | null };
export function sha256(data: Uint8Array): Promise<Uint8Array>;
export function b58decode(s: string): Uint8Array;
export function hexToBytes(h: string): Uint8Array;
export function bytesToHex(b: Uint8Array): string;
/** u64 i rreptë në [0, 2^64); hedh gabim përndryshe. */
export function toU64(v: unknown): bigint;
/** Kontrollon rreptë një llogari dhe e kthen në formën kanonike; hedh gabim përndryshe. */
export function normalizeAccount(a: unknown): { lamports: bigint; owner: string; executable: boolean; dataLen: bigint; dataHash: string };

export interface Attestation {
  /** Identiteti i validatorit (çelësi në tabelën e stake-ut), base58. */
  validator: string;
  /** Çelësi që firmosi, kur s'është vetë identiteti (base58). */
  signer?: string | null;
  /** Firma e identitetit mbi authorizeMessage(genesis, signer), hex. */
  authorization?: string | null;
  signature: string;
}
export interface QuorumReport {
  attestedStake: bigint;
  totalStake: bigint;
  validSignatures: number;
  rejected: number;
  reached: boolean;
}
export function attestMessage(genesisHex: string, slot: number | bigint, rootHex: string): Uint8Array;
export function authorizeMessage(genesisHex: string, signerB58: string): Uint8Array;
export function verifyAttestation(genesisHex: string, slot: number | bigint, rootHex: string, a: Attestation): Promise<boolean>;
export function verifyQuorum(genesisHex: string, slot: number | bigint, rootHex: string, atts: Attestation[], table: Map<string, bigint>): Promise<QuorumReport>;
/** Vendi i llogarisë në pemë: SHA256("zilja/key/v2" ‖ pubkey). */
export function treeKey(pubkeyB58: string): Promise<Uint8Array>;
export const PROOF_VERSION: 2;
export function stakeTableFromVoteAccounts(result: unknown): Map<string, bigint>;

export const TRUST: Readonly<Record<Trust, number>>;

export interface ZiljaClientOptions {
  /** Adresat e zileve. E para jep dëftesën; të tjerat krahasojnë root-in. */
  endpoints: string[];
  /** RPC Solana: për të krahasuar të dhënat dhe për tabelën e stake-ut. */
  solanaRpc?: string;
  /** Sa zile duhet të bien dakord për nivelin "agreement". Parazgjedhja: 2. */
  minAgree?: number;
  /** Kontrollo firmat e validatorëve (> 2/3 e stake-ut). Kërkon `solanaRpc`. */
  quorum?: boolean;
  /** Hash-i i genesis-it (base58 ose hex). Pa të, merret nga `solanaRpc`. */
  genesisHash?: string;
  /** Nën këtë nivel besimi, `ok` del `false`. Parazgjedhja: "agreement". */
  minTrust?: Trust;
  timeoutMs?: number;
  fetch?: typeof fetch;
}

export interface VerifyResult {
  /** `true` kur dëftesa është e vlefshme DHE besimi te root-i ≥ `minTrust`. */
  ok: boolean;
  verdict: Verdict;
  slot: number;
  root: string;
  /** Vlerat e kontrolluara (jo përgjigja e papërpunuar); `null` kur verdikti s'është "exists". */
  account: { lamports: bigint; owner: string; executable: boolean; dataLen: bigint; dataHash: string } | null;
  trust: Trust;
  /** Sa zile dhanë të njëjtin root. */
  agreeing: number;
  endpoints: number;
  quorum: { reached: boolean; stakePercent: number; validSignatures: number } | null;
  /** Krahasimi me të dhënat e `solanaRpc`, nëse u bë. */
  /** `bytes` jepet vetëm kur `matches === true`: vetëm atëherë të dhënat janë të verifikuara. */
  data: { checked: boolean; matches: boolean; rpcSlot: number; bytes: Uint8Array | null } | null;
  warnings: string[];
}

export class ZiljaClient {
  constructor(options: ZiljaClientOptions);
  verifyAccount(pubkey: string, opts?: { slot?: number }): Promise<VerifyResult>;
}
