// P-11: politika e vetme e vendimit për SDK-në, web-in dhe CLI-në (në Rust:
// crates/zilja-cli/src/policy.rs). Funksion i pastër: s'flet me rrjetin, merr atë që u mblodh dhe
// kthen vendimin. Të dy implementimet testohen me të njëjtët vektorë (test/policy-vectors.json).
//
// Katër pyetje të ndara, që ndërfaqja s'duhet t'i përziejë:
//   proof      - a përputhet dëftesa me root-in?            ("valid" | "invalid")
//   root       - pse i besojmë root-it?                      ("single" | "agreement" | "quorum" | "user")
//   freshness  - a është ky gjendja aktuale?                 ("fresh" | "stale" | "rollback" | "unknown" | "unchecked" | "historical")
//   data       - a përputhen të dhënat e RPC-së me dëftesën? ("match" | "mismatch" | "unchecked")
// Një dëftesë e vlefshme NUK do të thotë "kjo është gjendja aktuale e konsensusit": kjo kërkon edhe
// besim të mjaftueshëm te root-i dhe freski.

/** Nivelet e besimit te root-i. "user" = root-in e dha vetë përdoruesi (spirancë e tij). */
export const TRUST = Object.freeze({ single: 0, agreement: 1, quorum: 2, user: 3 });

/** Parazgjedhjet e përbashkëta. */
export const DEFAULTS = Object.freeze({ minTrust: "agreement", minAgree: 2, maxSlotLagWithRpc: 150 });

/**
 * Gabimet e konfigurimit (para çdo kërkese). Kthen listën e kodeve; bosh = në rregull.
 * minAgree s'ulet kurrë vetë sipas numrit të zileve.
 */
export function configErrors({ minTrust, minAgree, endpoints, quorumEnabled, rootOverride }) {
  const e = [];
  if (!["single", "agreement", "quorum"].includes(minTrust)) e.push("config_min_trust");
  if (!Number.isSafeInteger(minAgree) || minAgree < 2) e.push("config_min_agree");
  else if (minTrust === "agreement" && !rootOverride && endpoints < minAgree) e.push("config_min_agree");
  if (minTrust === "quorum" && !rootOverride && !quorumEnabled) e.push("config_quorum");
  return e;
}

/**
 * @param {object} i
 * @param {{minTrust: string, minAgree: number, maxSlotLag: number|null, minSlot: number|null}} i.policy
 * @param {{pubkey: string, slot: number|null}} i.request
 * @param {number} i.endpoints zile të ndryshme të konfiguruara
 * @param {boolean} i.quorumEnabled
 * @param {boolean} i.rootOverride
 * @param {{pubkey: string, slot: number, root: string, complete: boolean, verdict: "exists"|"does_not_exist"|"invalid"}} i.primary
 * @param {{ok: boolean, slot?: number, root?: string}[]} i.others
 * @param {{root: string, reached: boolean}[]|null} i.quorum grupet sipas root-it (null = s'u kontrollua)
 * @param {boolean|null} i.quorumEpochOk tabela e stake-ut i përket epokës së slotit
 * @param {number|null} i.finalizedSlot
 * @param {number|null} i.highestSeen
 * @param {{matches: boolean}|null} i.data
 */
export function decide(i) {
  const p = i.policy;
  const errors = configErrors({ ...p, endpoints: i.endpoints, quorumEnabled: i.quorumEnabled, rootOverride: i.rootOverride });
  const warnings = [];
  const r = i.primary;
  const checks = { proof: "invalid", completeness: r?.complete === true ? "complete" : "partial", root: "single", freshness: "unchecked", data: "unchecked" };

  // Përgjigja duhet të jetë për llogarinë dhe slotin e kërkuar.
  const rootOk = typeof r?.root === "string" && /^[0-9a-f]{64}$/.test(r.root);
  if (!r || !rootOk || !Number.isSafeInteger(r.slot) || r.slot < 0) errors.push("bad_response");
  else if (r.pubkey !== i.request.pubkey) errors.push("wrong_account");
  else if (i.request.slot != null && r.slot > i.request.slot) errors.push("wrong_slot");

  // 1. Dëftesa.
  if (r?.verdict === "exists" || r?.verdict === "does_not_exist") checks.proof = "valid";
  else errors.push("proof_invalid");
  // "Nuk ekziston" ka kuptim vetëm nga një pemë që mbulon çdo llogari.
  if (r?.verdict === "does_not_exist" && r.complete !== true) errors.push("absence_incomplete");

  // 2. Besimi te root-i.
  let trust = "single";
  let agreeing = 1;
  if (i.rootOverride) trust = "user";
  else if (r) {
    const conflicts = [];
    for (const o of i.others ?? []) {
      if (!o?.ok || o.slot !== r.slot) continue;
      if (o.root === r.root) agreeing++;
      else conflicts.push(o.root);
    }
    if (i.endpoints >= 2 && agreeing >= p.minAgree) trust = "agreement";
    let quorumRoot = null;
    if (i.quorum) {
      if (i.quorumEpochOk === false) warnings.push("quorum_epoch_unsupported");
      else {
        const reached = [...new Set(i.quorum.filter((g) => g.reached).map((g) => g.root))];
        if (reached.length > 1) errors.push("quorum_split");
        else if (reached.length === 1) quorumRoot = reached[0];
      }
    }
    if (quorumRoot != null) {
      if (quorumRoot === r.root) {
        trust = "quorum";
        // Kuorumi vendos: një zile në konflikt s'e bllokon, por shënohet.
        if (conflicts.length) warnings.push("conflict_outvoted_by_quorum");
      } else errors.push("quorum_other_root");
    } else if (conflicts.length) errors.push("root_conflict");
  }
  checks.root = trust;
  if (TRUST[trust] < (TRUST[p.minTrust] ?? 99)) errors.push("trust_below_min");

  // 3. Freskia (vetëm për gjendjen "aktuale"; një slot i kërkuar është historik).
  if (i.request.slot != null) checks.freshness = "historical";
  else if (r && Number.isSafeInteger(r.slot)) {
    if (i.highestSeen != null && r.slot < i.highestSeen) {
      checks.freshness = "rollback";
      errors.push("rollback");
    } else if (p.minSlot != null && r.slot < p.minSlot) {
      checks.freshness = "stale";
      errors.push("below_min_slot");
    } else if (p.maxSlotLag != null) {
      if (i.finalizedSlot == null) {
        checks.freshness = "unknown";
        errors.push("freshness_unknown");
      } else if (i.finalizedSlot - r.slot > p.maxSlotLag) {
        checks.freshness = "stale";
        errors.push("stale");
      } else checks.freshness = "fresh";
    }
  }

  // 4. Të dhënat nga RPC-ja (kur u kërkuan).
  if (i.data) {
    checks.data = i.data.matches ? "match" : "mismatch";
    if (!i.data.matches) errors.push("data_mismatch");
  }

  const invalid = ["bad_response", "wrong_account", "wrong_slot", "proof_invalid"].some((e) => errors.includes(e));
  const conflict = ["root_conflict", "quorum_other_root", "quorum_split"].some((e) => errors.includes(e));
  const verdict = invalid ? "invalid" : conflict ? "conflict" : r.verdict;
  return { ok: errors.length === 0, verdict, trust, agreeing, checks, errors, warnings };
}
