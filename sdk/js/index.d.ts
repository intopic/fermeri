export type Verdict = "exists" | "does_not_exist" | "invalid";
/** Vendimi i klientit: `conflict` = zilet ose kuorumi s'pajtohen për root-in. */
export type ClientVerdict = Verdict | "conflict";
export type Trust = "single" | "agreement" | "quorum";
/** Burimi i besimit te root-i; "user" = root-i i dhënë nga vetë përdoruesi me `root`. */
export type RootTrust = Trust | "user";

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
  /** Autorizimi v2 i `signer`-it nga identiteti, i vlefshëm për epokat [fromEpoch, untilEpoch) (maksimumi 15). */
  authorization?: Authorization | null;
  signature: string;
}
export interface Authorization {
  fromEpoch: number | bigint;
  untilEpoch: number | bigint;
  /** Firma e identitetit mbi authorizeMessage(genesis, signer, fromEpoch, untilEpoch), hex. */
  signature: string;
}
export const MAX_AUTH_EPOCHS: 15n;
export interface QuorumReport {
  attestedStake: bigint;
  totalStake: bigint;
  validSignatures: number;
  rejected: number;
  reached: boolean;
}
export function attestMessage(genesisHex: string, slot: number | bigint, rootHex: string): Uint8Array;
export function authorizeMessage(genesisHex: string, signerB58: string, fromEpoch: number | bigint, untilEpoch: number | bigint): Uint8Array;
/** `epoch` = epoka e `slot`-it (ajo e tabelës së stake-ut). */
export function verifyAttestation(genesisHex: string, epoch: number | bigint, slot: number | bigint, rootHex: string, a: Attestation): Promise<boolean>;
export interface Revocation {
  validator: string;
  signer: string;
  /** Firma e identitetit mbi revokeMessage(genesis, signer), hex. */
  signature: string;
}
export function revokeMessage(genesisHex: string, signerB58: string): Uint8Array;
export function verifyRevocation(genesisHex: string, r: Revocation): Promise<boolean>;
/** `revocations` (R5-1): nga cilido burim; secila verifikohet. */
export function verifyQuorum(genesisHex: string, epoch: number | bigint, slot: number | bigint, rootHex: string, atts: Attestation[], table: Map<string, bigint>, revocations?: Revocation[]): Promise<QuorumReport>;
/** Vendi i llogarisë në pemë: SHA256("zilja/key/v2" ‖ pubkey). */
export function treeKey(pubkeyB58: string): Promise<Uint8Array>;
export const PROOF_VERSION: 2;
export function stakeTableFromVoteAccounts(result: unknown): Map<string, bigint>;

export const TRUST: Readonly<Record<RootTrust, number>>;
export const DEFAULTS: Readonly<{ minTrust: "agreement"; minAgree: 2; maxSlotLagWithRpc: 150 }>;

/** Kontrollet e ndara (P-11): një dëftesë e vlefshme s'do të thotë "gjendje aktuale e konsensusit". */
export interface Checks {
  proof: "valid" | "invalid";
  completeness: "complete" | "partial";
  root: RootTrust;
  /** A12: "ahead" = sloti mbi referencën e finalizuar; A14: "unverifiable" = referencë vetëm-hash. */
  freshness: "fresh" | "stale" | "rollback" | "ahead" | "unknown" | "unverifiable" | "unchecked" | "historical";
  /** A13: "unavailable" = krahasimi u kërkua por s'u krye (s'jep `ok`). */
  data: "match" | "mismatch" | "unavailable" | "unchecked";
}

/** Politika e përbashkët me web-in dhe CLI-në (crates/zilja-cli/src/policy.rs). */
export function configErrors(o: { minTrust: string; minAgree: number; endpoints: number; quorumEnabled: boolean; rootOverride: boolean }): string[];
export function decide(i: unknown): { ok: boolean; verdict: ClientVerdict; trust: RootTrust; agreeing: number; checks: Checks; errors: string[]; warnings: string[]; raiseFloor: number | null };

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
  /** R5-1: revokime shtesë; bashkohen me ato që jep `getZiljaRevocations` i çdo zileje. */
  revocations?: Revocation[];
  /** R5-3: aggregator-ë të pabesuar (`getZiljaQuorumBundle`): firmat e shumë zileve me një thirrje; çdo firmë verifikohet. */
  aggregators?: string[];
  /** Nën këtë nivel besimi, `ok` del `false`. Parazgjedhja: "agreement". */
  minTrust?: Trust;
  /** Sa slote pas slotit të finalizuar të `solanaRpc` pranohen. Parazgjedhja: 150 kur ka `solanaRpc`; `null` = pa kontroll. */
  maxSlotLag?: number | null;
  /** Slot minimal i pranuar (p.sh. i fundit që pe në një seancë të kaluar). */
  minSlot?: number;
  /**
   * A14: referenca që e njeh vetë (besimi "user"). Vetëm hash: vërteton përfshirjen te ai root, por
   * jo slotin, freskinë apo mungesën. Me `slot` (dhe `complete: true` për mungesë), dëftesa duhet të
   * jetë për atë slot dhe nga një pemë e plotë.
   */
  root?: string | { root: string; slot?: number; complete?: boolean };
  /** Krahaso lamports, owner, executable dhe bajtet me `solanaRpc`. Parazgjedhja: po. A13: kur kërkohet, vetëm përputhja jep `ok`. */
  compareData?: boolean;
  /** Koha maksimale e një kërkese, përfshi leximin e trupit. Parazgjedhja: 10000. */
  timeoutMs?: number;
  /** A17: kufijtë e përgjigjeve në bajte. Parazgjedhja: zilja 1 MB, solana 64 MB. */
  maxResponseBytes?: { zilja?: number; solana?: number };
  fetch?: typeof fetch;
}

export interface VerifyResult {
  /** `true` vetëm kur çdo kontroll kaloi: dëftesa, besimi ≥ `minTrust`, freskia dhe të dhënat (kur u kontrolluan). */
  ok: boolean;
  verdict: ClientVerdict;
  slot: number;
  root: string;
  /** Vlerat e kontrolluara (jo përgjigja e papërpunuar); `null` kur verdikti s'është "exists". */
  account: { lamports: bigint; owner: string; executable: boolean; dataLen: bigint; dataHash: string } | null;
  trust: RootTrust;
  /** Sa zile dhanë të njëjtin root. */
  agreeing: number;
  endpoints: number;
  quorum: { reached: boolean; stakePercent: number; validSignatures: number } | null;
  /** Krahasimi me të dhënat e `solanaRpc`, nëse u bë. */
  /** `bytes` jepet vetëm kur `matches === true`: vetëm atëherë të dhënat janë të verifikuara. */
  data: { checked: boolean; matches: boolean; rpcSlot: number; bytes: Uint8Array | null } | null;
  checks: Checks;
  /** Kodet e kontrolleve që dështuan (p.sh. "stale", "root_conflict", "absence_incomplete"). */
  errors: string[];
  warnings: string[];
}

export class ZiljaClient {
  /** Hedh gabim për konfigurim të pavlefshëm (p.sh. një zile me minTrust "agreement"). */
  constructor(options: ZiljaClientOptions);
  /** Sloti më i lartë i pranuar në këtë instancë: një përgjigje më e vjetër jep "rollback". */
  highestSeen: number | null;
  verifyAccount(pubkey: string, opts?: { slot?: number }): Promise<VerifyResult>;
}
