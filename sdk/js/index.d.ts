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
export const MULTI_VERSION: 0x11;
export const MAX_MULTI_KEYS: 256;
/** R5-2: një verdikt për çdo pretendim; të gjithë "invalid" kur dëftesa s'vlen për root-in. */
export function verifyMultiProof(rootHex: string, claims: { pubkey: string; account: ZiljaAccount | null }[], proofHex: string): Promise<Verdict[]>;
export function parseMultiProof(bytes: Uint8Array): { terminals: { depth: number; leaf: { key: Uint8Array; valueHash: Uint8Array } | null }[]; siblings: Uint8Array[] };
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
  /** Sloti i dëftesës; `null` kur zilja s'u përgjigj ose dha një slot të pavlefshëm (G20). */
  slot: number | null;
  /** Root-i hex kundrejt të cilit u kontrollua; `null` kur s'ka (G20). */
  root: string | null;
  /** Vlerat e kontrolluara (jo përgjigja e papërpunuar); `null` kur verdikti s'është "exists". */
  account: { lamports: bigint; owner: string; executable: boolean; dataLen: bigint; dataHash: string } | null;
  trust: RootTrust;
  /** Sa zile dhanë të njëjtin root. */
  agreeing: number;
  endpoints: number;
  quorum: { reached: boolean; stakePercent: number; validSignatures: number } | null;
  /** Krahasimi me të dhënat e `solanaRpc`, nëse u bë. */
  /** `bytes` jepet vetëm kur `matches === true`: vetëm atëherë të dhënat janë të verifikuara. */
  data: { checked: boolean; matches: boolean; rpcSlot: number | null; bytes: Uint8Array | null } | null;
  checks: Checks;
  /** Kodet e kontrolleve që dështuan (p.sh. "stale", "root_conflict", "absence_incomplete"). */
  errors: string[];
  warnings: string[];
}

/** G10: forma kanonike e adresës së një zileje (host i vogël, pa portin parazgjedhës, pa "/" në fund). */
export function canonicalEndpoint(e: string): string;

export class ZiljaClient {
  /** Hedh gabim për konfigurim të pavlefshëm (p.sh. një zile me minTrust "agreement"). */
  constructor(options: ZiljaClientOptions);
  /** Sloti më i lartë i pranuar në këtë instancë: një përgjigje më e vjetër jep "rollback". */
  highestSeen: number | null;
  verifyAccount(pubkey: string, opts?: { slot?: number }): Promise<VerifyResult>;
  /** G03: revokimet e verifikuara që kjo instancë ka mësuar nga rrjeti (vetëm rriten); ruaji dhe jepi te `revocations` pas rinisjes. */
  learnedRevocations(genesisHex: string): Revocation[];
  /** R5-2: shumë llogari me një multi-proof (`getMultipleAccountProofs`); një rezultat për çdo pubkey, në radhën e kërkesës. */
  verifyAccounts(pubkeys: string[], opts?: { slot?: number }): Promise<VerifyResult[]>;
}

// R5-4: lidhje "drop-in" me verifikim (web3.js / kit), pa varësi.
export const TOKEN_PROGRAM: "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
export const TOKEN_2022_PROGRAM: "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
export class VerificationError extends Error {
  results: unknown[];
}
type PubkeyLike = string | { toBase58(): string };
export interface VerifiedAccountInfo<O = string> {
  lamports: number;
  owner: O;
  executable: boolean;
  data: Uint8Array;
  rentEpoch: 0;
  space: number;
}
export interface TokenAccount {
  mint: string;
  owner: string;
  amount: bigint;
  delegate: string | null;
  state: "initialized" | "frozen" | "unknown";
  isNative: bigint | null;
  delegatedAmount: bigint;
  closeAuthority: string | null;
}
export function parseTokenAccount(data: Uint8Array): TokenAccount;
export function parseMint(data: Uint8Array): { supply: bigint; decimals: number };
/** Uint8Array → base64, me copa (pa spread), edhe pa Buffer. */
export function bytesToBase64(u8: Uint8Array): string;
export class VerifiableConnection {
  /** `zilja`: ZiljaClient ose opsionet e tij (duhet `solanaRpc`). `PublicKey`: konstruktori i web3.js (opsional). */
  constructor(o: { zilja: ZiljaClient | ZiljaClientOptions; PublicKey?: new (s: string) => any; onUnverified?: "throw" | "null" });
  getAccountInfo(pubkey: PubkeyLike): Promise<VerifiedAccountInfo<any> | null>;
  getMultipleAccountsInfo(pubkeys: PubkeyLike[]): Promise<(VerifiedAccountInfo<any> | null)[]>;
  getBalance(pubkey: PubkeyLike): Promise<number | null>;
  getTokenAccount(pubkey: PubkeyLike): Promise<(TokenAccount & { program: string }) | null>;
  getTokenAccountBalance(pubkey: PubkeyLike): Promise<{ value: { amount: string; decimals: number; uiAmountString: string } }>;
}
interface KitAccount {
  lamports: bigint;
  owner: string;
  executable: boolean;
  data: [string, "base64"];
  space: bigint;
}
/**
 * G24: konfigurimi i kit-it që respektohet; çdo çelës tjetër (p.sh. `dataSlice`) hedh gabim.
 * `commitment`: çdo nivel pranohet (zilja jep gjendjen e një sloti të rrënjosur).
 * `minContextSlot`: përgjigjja duhet të jetë të paktën në atë slot, përndryshe gabim.
 */
interface KitConfig {
  commitment?: "processed" | "confirmed" | "finalized";
  minContextSlot?: bigint | number;
}
interface KitAccountConfig extends KitConfig {
  /** Vetëm "base64" (forma që kthehet). */
  encoding?: "base64";
}
/** Për @solana/kit: të njëjtat metoda si `rpc`, me `.send()`, vetëm me llogari të verifikuara. */
export function createVerifiedRpc(
  zilja: ZiljaClient | ZiljaClientOptions,
  o?: { onUnverified?: "throw" | "null" },
): {
  getAccountInfo(address: string, config?: KitAccountConfig): { send(): Promise<{ context: { slot: bigint }; value: KitAccount | null }> };
  /** ≤ 256 adresa (një multi-proof, një `context.slot`); më shumë hedh gabim (G02). */
  getMultipleAccounts(addresses: string[], config?: KitAccountConfig): { send(): Promise<{ context: { slot: bigint }; value: (KitAccount | null)[] }> };
  /** `value`: lamports; `0n` vetëm kur mungesa provohet; `null` kur verifikimi dështoi (me `onUnverified: "null"`). */
  getBalance(address: string, config?: KitConfig): { send(): Promise<{ context: { slot: bigint }; value: bigint | null }> };
};
