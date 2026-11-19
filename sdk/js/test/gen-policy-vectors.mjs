// Gjeneron test/policy-vectors.json nga politika JS (P-11). Rust (crates/zilja-cli/tests/policy_vectors.rs)
// dhe JS (test/policy.test.mjs) duhet të japin pikërisht këto vendime. CI kontrollon që skedari në
// repo = ai që gjenerohet këtu: çdo ndryshim i politikës kalon nga një diff i dukshëm i vektorëve.
//   node test/gen-policy-vectors.mjs > test/policy-vectors.json
import { decide } from "../src/policy.js";

const R = "ab".repeat(32), R2 = "cd".repeat(32), K = "Acc1111111111111111111111111111111111111111";
const base = (o = {}) => ({
  policy: { minTrust: "agreement", minAgree: 2, maxSlotLag: null, minSlot: null, ...(o.policy ?? {}) },
  request: { pubkey: K, slot: null, ...(o.request ?? {}) },
  endpoints: 2, quorumEnabled: false, anchor: null,
  primary: { pubkey: K, slot: 100, root: R, complete: true, verdict: "exists", ...(o.primary ?? {}) },
  others: [{ ok: true, slot: 100, root: R, complete: true }], quorum: null, quorumEpochOk: null,
  finalizedSlot: null, highestSeen: null, data: null,
  ...Object.fromEntries(Object.entries(o).filter(([k]) => !["policy", "request", "primary"].includes(k))),
});
const single = { policy: { minTrust: "single" }, endpoints: 1, others: [] };
const q = (groups) => ({ policy: { minTrust: "quorum" }, quorumEnabled: true, quorum: groups, quorumEpochOk: true });
const cases = {
  agreement_ok: base(),
  single_endpoint_default_is_config_error: base({ endpoints: 1, others: [] }),
  single_allowed: base(single),
  min_agree_one_rejected: base({ policy: { minAgree: 1 } }),
  root_conflict: base({ others: [{ ok: true, slot: 100, root: R2, complete: true }] }),
  other_slot_ignored: base({ others: [{ ok: true, slot: 99, root: R2, complete: true }] }),
  wrong_account: base({ primary: { pubkey: "Other111111111111111111111111111111111111111" } }),
  wrong_slot: base({ request: { slot: 50 } }),
  historical_slot: base({ request: { slot: 100 } }),
  malformed_root: base({ primary: { root: "ab" } }),
  proof_invalid: base({ primary: { verdict: "invalid" } }),
  absence_complete: base({ primary: { verdict: "does_not_exist" } }),
  absence_incomplete: base({ primary: { verdict: "does_not_exist", complete: false } }),
  // A14: për mungesë, një zile që pajtohet për root-in por s'pohon pemë të plotë s'numërohet.
  absence_agreeing_source_incomplete: base({ primary: { verdict: "does_not_exist" }, others: [{ ok: true, slot: 100, root: R, complete: false }] }),
  stale: base({ policy: { maxSlotLag: 150 }, finalizedSlot: 400 }),
  fresh: base({ policy: { maxSlotLag: 150 }, finalizedSlot: 200 }),
  // A12: sloti mbi referencën e finalizuar s'është "fresh".
  ahead_of_reference: base({ policy: { maxSlotLag: 150 }, finalizedSlot: 99 }),
  freshness_unknown: base({ policy: { maxSlotLag: 150 }, finalizedSlot: null }),
  below_min_slot: base({ policy: { minSlot: 101 } }),
  rollback: base({ highestSeen: 120 }),
  data_mismatch: base({ data: { status: "mismatch" } }),
  data_match: base({ data: { status: "match" } }),
  // A13: kontrolli i kërkuar që s'u krye s'jep sukses.
  data_unavailable: base({ data: { status: "unavailable" } }),
  quorum_ok_outvotes_conflict: base({ ...q([{ root: R, reached: true }, { root: R2, reached: false }]), others: [{ ok: true, slot: 100, root: R2, complete: true }] }),
  quorum_other_root: base(q([{ root: R2, reached: true }])),
  quorum_split: base(q([{ root: R, reached: true }, { root: R2, reached: true }])),
  quorum_wrong_epoch: base({ ...q([{ root: R, reached: true }]), quorumEpochOk: false }),
  quorum_without_rpc_config: base({ policy: { minTrust: "quorum" } }),
  // A14: referenca vetëm-hash vërteton përfshirjen; sloti dhe plotësia e serverit s'i besohen.
  anchor_hash_only_inclusion: base({ endpoints: 1, others: [], anchor: { slot: null, complete: null } }),
  anchor_hash_only_freshness_required: base({ endpoints: 1, others: [], anchor: { slot: null, complete: null }, policy: { maxSlotLag: 150 }, finalizedSlot: 120 }),
  anchor_hash_only_absence: base({ endpoints: 1, others: [], anchor: { slot: null, complete: null }, primary: { verdict: "does_not_exist" } }),
  anchor_slot_complete_absence: base({ endpoints: 1, others: [], anchor: { slot: 100, complete: true }, primary: { verdict: "does_not_exist", complete: false } }),
  anchor_slot_mismatch: base({ endpoints: 1, others: [], anchor: { slot: 99, complete: true } }),
  anchor_slot_fresh: base({ endpoints: 1, others: [], anchor: { slot: 100, complete: null }, policy: { maxSlotLag: 150 }, finalizedSlot: 120 }),
};
const pick = ({ ok, verdict, trust, agreeing, checks, errors, warnings, raiseFloor }) => ({ ok, verdict, trust, agreeing, checks, errors, warnings, raiseFloor });
const out = Object.entries(cases).map(([name, input]) => ({ name, input, expect: pick(decide(input)) }));
process.stdout.write(JSON.stringify(out, null, 1) + "\n");
