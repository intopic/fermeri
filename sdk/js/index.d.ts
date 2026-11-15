export type Verdict = "exists" | "does_not_exist" | "invalid";
export type Trust = "single" | "agreement" | "quorum";

/** Llogaria siç e jep Zilja (`getAccountProof`). */
export interface ZiljaAccount {
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

export interface Attestation {
  validator: string;
  signature: string;
}
export interface QuorumReport {
  attestedStake: bigint;
  totalStake: bigint;
  validSignatures: number;
  rejected: number;
  reached: boolean;
}
export function attestMessage(slot: number | bigint, rootHex: string): Uint8Array;
export function verifyQuorum(slot: number | bigint, rootHex: string, atts: Attestation[], table: Map<string, bigint>): Promise<QuorumReport>;
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
  /** Nën këtë nivel besimi, `ok` del `false`. Parazgjedhja: "single". */
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
  account: { lamports: bigint; owner: string; executable: boolean; dataLen: number; dataHash: string } | null;
  trust: Trust;
  /** Sa zile dhanë të njëjtin root. */
  agreeing: number;
  endpoints: number;
  quorum: { reached: boolean; stakePercent: number; validSignatures: number } | null;
  /** Krahasimi me të dhënat e `solanaRpc`, nëse u bë. */
  data: { checked: boolean; matches: boolean; rpcSlot: number } | null;
  warnings: string[];
}

export class ZiljaClient {
  constructor(options: ZiljaClientOptions);
  verifyAccount(pubkey: string, opts?: { slot?: number }): Promise<VerifyResult>;
}
