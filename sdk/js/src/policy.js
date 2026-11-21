// P-11: politika e vetme e vendimit për SDK-në, web-in dhe CLI-në (në Rust:
// crates/zilja-cli/src/policy.rs). Funksion i pastër: s'flet me rrjetin, merr atë që u mblodh dhe
// kthen vendimin. Të dy implementimet testohen me të njëjtët vektorë (test/policy-vectors.json).
//
// Katër pyetje të ndara, që ndërfaqja s'duhet t'i përziejë:
//   proof      - a përputhet dëftesa me root-in?            ("valid" | "invalid")
//   root       - pse i besojmë root-it?                      ("single" | "agreement" | "quorum" | "user")
//   freshness  - a është ky gjendja aktuale?                 ("fresh" | "stale" | "rollback" | "ahead" | "unknown" | "unverifiable" | "unchecked" | "historical")
//   data       - a përputhen të dhënat e RPC-së me dëftesën? ("match" | "mismatch" | "unavailable" | "unchecked")
// Një dëftesë e vlefshme NUK do të thotë "kjo është gjendja aktuale e konsensusit": kjo kërkon edhe
// besim të mjaftueshëm te root-i dhe freski.

/** Nivelet e besimit te root-i. "user" = root-in e dha vetë përdoruesi (spirancë e tij). */
export const TRUST = Object.freeze({ single: 0, agreement: 1, quorum: 2, user: 3 });

/** Parazgjedhjet e përbashkëta. */
// Parazgjedhja "single": një zile mjafton (dëftesa verifikohet gjithmonë). "agreement" dhe "quorum"
// janë zgjedhje për më shumë besim te root-i.
export const DEFAULTS = Object.freeze({ minTrust: "single", minAgree: 2, maxSlotLagWithRpc: 150 });

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
 * @param {{slot: number|null, complete: boolean|null}|null} i.anchor A14: referenca e përdoruesit
 *   (root-i i saj përdoret për dëftesën). `slot` / `complete` = null kur referenca është vetëm hash:
 *   atëherë vërtetohet vetëm përfshirja te ai root, jo sloti, freskia apo plotësia.
 * @param {{pubkey: string, slot: number, root: string, complete: boolean, verdict: "exists"|"does_not_exist"|"invalid"}} i.primary
 * @param {{ok: boolean, slot?: number, root?: string, complete?: boolean}[]} i.others
 * @param {{root: string, reached: boolean}[]|null} i.quorum grupet sipas root-it (null = s'u kontrollua)
 * @param {boolean|null} i.quorumEpochOk tabela e stake-ut i përket epokës së slotit
 * @param {number|null} i.finalizedSlot
 * @param {number|null} i.highestSeen
 * @param {{status: "match"|"mismatch"|"unavailable"}|null} i.data A13: null = kontrolli s'u kërkua
 */
export function decide(i) {
  const p = i.policy;
  const a = i.anchor ?? null;
  const errors = configErrors({ ...p, endpoints: i.endpoints, quorumEnabled: i.quorumEnabled, rootOverride: a != null });
  const warnings = [];
  const r = i.primary;
  // A14: me referencë, plotësia vjen nga referenca, jo nga serveri.
  const complete = a ? a.complete === true : r?.complete === true;
  const checks = { proof: "invalid", completeness: complete ? "complete" : "partial", root: "single", freshness: "unchecked", data: "unchecked" };

  // Përgjigja duhet të jetë për llogarinë dhe slotin e kërkuar.
  const rootOk = typeof r?.root === "string" && /^[0-9a-f]{64}$/.test(r.root);
  if (!r || !rootOk || !Number.isSafeInteger(r.slot) || r.slot < 0) errors.push("bad_response");
  else if (r.pubkey !== i.request.pubkey) errors.push("wrong_account");
  else if (i.request.slot != null && r.slot > i.request.slot) errors.push("wrong_slot");
  else if (a?.slot != null && r.slot !== a.slot) errors.push("anchor_slot_mismatch");

  // 1. Dëftesa.
  if (r?.verdict === "exists" || r?.verdict === "does_not_exist") checks.proof = "valid";
  else errors.push("proof_invalid");
  // "Nuk ekziston" ka kuptim vetëm nga një pemë që mbulon çdo llogari.
  if (r?.verdict === "does_not_exist" && !complete) errors.push("absence_incomplete");

  // 2. Besimi te root-i.
  let trust = "single";
  let agreeing = 1;
  if (a) trust = "user";
  else if (r) {
    const conflicts = [];
    for (const o of i.others ?? []) {
      if (!o?.ok || o.slot !== r.slot) continue;
      if (o.root === r.root) {
        // A14: për mungesë numërohen vetëm burimet që pohojnë edhe ato pemë të plotë.
        if (r.verdict === "does_not_exist" && o.complete !== true) warnings.push("agreeing_incomplete");
        else agreeing++;
      } else conflicts.push(o.root);
    }
    if (i.endpoints >= 2 && agreeing >= p.minAgree) trust = "agreement";
    let quorumRoot = null;
    if (i.quorum) {
      // G25: kuorumi numërohet vetëm kur epoka u konfirmua (`true`); `null`/mungesë = s'dihet, pra jo.
      if (i.quorumEpochOk !== true) warnings.push("quorum_epoch_unsupported");
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
  // A14: me referencë vetëm-hash, sloti i serverit s'është i vërtetuar: freskia s'mund të provohet.
  const slotTrusted = !(a && a.slot == null);
  if (i.request.slot != null) checks.freshness = "historical";
  else if (!slotTrusted) {
    checks.freshness = "unverifiable";
    if (p.maxSlotLag != null || p.minSlot != null) errors.push("freshness_unverifiable");
  } else if (r && Number.isSafeInteger(r.slot)) {
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
      } else if (r.slot > i.finalizedSlot) {
        // A12: përpara referencës: finaliteti i slotit s'konfirmohet nga referenca e zgjedhur.
        checks.freshness = "ahead";
        errors.push("ahead_of_reference");
      } else if (i.finalizedSlot - r.slot > p.maxSlotLag) {
        checks.freshness = "stale";
        errors.push("stale");
      } else checks.freshness = "fresh";
    }
  }

  // 4. Të dhënat nga RPC-ja (A13: kur u kërkuan, vetëm përputhja jep sukses).
  if (i.data) {
    const st = i.data.status;
    checks.data = st === "match" ? "match" : st === "mismatch" ? "mismatch" : "unavailable";
    if (st === "mismatch") errors.push("data_mismatch");
    else if (st !== "match") errors.push("data_unavailable");
  }

  const invalid = ["bad_response", "wrong_account", "wrong_slot", "anchor_slot_mismatch", "proof_invalid"].some((e) => errors.includes(e));
  const conflict = ["root_conflict", "quorum_other_root", "quorum_split"].some((e) => errors.includes(e));
  const verdict = invalid ? "invalid" : conflict ? "conflict" : r.verdict;
  const ok = errors.length === 0;
  // Kufiri i rollback-ut rritet vetëm me një slot të vërtetuar, aktual dhe jo përpara referencës.
  const raiseFloor = ok && slotTrusted && (checks.freshness === "fresh" || checks.freshness === "unchecked") ? r.slot : null;
  return { ok, verdict, trust, agreeing, checks, errors, warnings: [...new Set(warnings)], raiseFloor };
}
