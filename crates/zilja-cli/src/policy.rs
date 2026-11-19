//! P-11: politika e vetme e vendimit, pasqyrë e saktë e `sdk/js/src/policy.js`. Funksion i pastër:
//! merr atë që u mblodh nga rrjeti dhe kthen vendimin. Të dy implementimet testohen me të njëjtët
//! vektorë (`sdk/js/test/policy-vectors.json`), pra SDK, web dhe CLI s'mund të ndahen pa u vënë re.
//!
//! Katër pyetje të ndara:
//!   proof      - a përputhet dëftesa me root-in?
//!   root       - pse i besojmë root-it? (single | agreement | quorum | user)
//!   freshness  - a është kjo gjendja aktuale?
//!   data       - a përputhen të dhënat e RPC-së me dëftesën?

/// Niveli i besimit te root-i (renditja ka rëndësi).
pub fn trust_rank(t: &str) -> Option<u8> {
    match t {
        "single" => Some(0),
        "agreement" => Some(1),
        "quorum" => Some(2),
        "user" => Some(3),
        _ => None,
    }
}

pub const DEFAULT_MIN_TRUST: &str = "agreement";
pub const DEFAULT_MIN_AGREE: u64 = 2;
pub const DEFAULT_MAX_SLOT_LAG_WITH_RPC: u64 = 150;

#[derive(Clone, Debug)]
pub struct Policy {
    pub min_trust: String,
    pub min_agree: i64,
    pub max_slot_lag: Option<u64>,
    pub min_slot: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Primary {
    pub pubkey: String,
    /// `None` = mungon ose s'është numër i plotë jonegativ.
    pub slot: Option<u64>,
    pub root: String,
    pub complete: bool,
    /// "exists" | "does_not_exist" | "invalid"
    pub verdict: String,
}

#[derive(Clone, Debug)]
pub struct Other {
    pub ok: bool,
    pub slot: Option<u64>,
    pub root: Option<String>,
    /// A14: burimi pohon pemë të plotë (`getZiljaRoot.complete`).
    pub complete: bool,
}

/// A14: referenca e përdoruesit. `slot` / `complete` = `None` kur referenca është vetëm hash.
#[derive(Clone, Debug)]
pub struct Anchor {
    pub slot: Option<u64>,
    pub complete: Option<bool>,
}

/// A13: rezultati i kontrollit të kërkuar të të dhënave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataStatus {
    Match,
    Mismatch,
    Unavailable,
}

#[derive(Clone, Debug)]
pub struct QuorumGroup {
    pub root: String,
    pub reached: bool,
}

#[derive(Clone, Debug)]
pub struct Input {
    pub policy: Policy,
    pub request_pubkey: String,
    pub request_slot: Option<u64>,
    pub endpoints: usize,
    pub quorum_enabled: bool,
    pub anchor: Option<Anchor>,
    pub primary: Option<Primary>,
    pub others: Vec<Other>,
    /// `None` = kuorumi s'u kontrollua.
    pub quorum: Option<Vec<QuorumGroup>>,
    pub quorum_epoch_ok: Option<bool>,
    pub finalized_slot: Option<u64>,
    pub highest_seen: Option<u64>,
    /// `None` = kontrolli s'u kërkua.
    pub data: Option<DataStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checks {
    pub proof: &'static str,
    pub completeness: &'static str,
    pub root: &'static str,
    pub freshness: &'static str,
    pub data: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub ok: bool,
    pub verdict: String,
    pub trust: &'static str,
    pub agreeing: u64,
    pub checks: Checks,
    pub errors: Vec<&'static str>,
    pub warnings: Vec<&'static str>,
    /// Sloti me të cilin lejohet të rritet kufiri i rollback-ut (vetëm kur ok, i vërtetuar, aktual).
    pub raise_floor: Option<u64>,
}

/// Gabimet e konfigurimit; bosh = në rregull. `min_agree` s'ulet kurrë vetë sipas numrit të zileve.
pub fn config_errors(p: &Policy, endpoints: usize, quorum_enabled: bool, root_override: bool) -> Vec<&'static str> {
    let mut e = Vec::new();
    if !matches!(p.min_trust.as_str(), "single" | "agreement" | "quorum") {
        e.push("config_min_trust");
    }
    let short = p.min_trust == "agreement" && !root_override && (endpoints as i64) < p.min_agree;
    if p.min_agree < 2 || short {
        e.push("config_min_agree");
    }
    if p.min_trust == "quorum" && !root_override && !quorum_enabled {
        e.push("config_quorum");
    }
    e
}

fn is_root_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn decide(i: &Input) -> Decision {
    let p = &i.policy;
    let a = i.anchor.as_ref();
    let mut errors = config_errors(p, i.endpoints, i.quorum_enabled, a.is_some());
    let mut warnings: Vec<&'static str> = Vec::new();
    let r = i.primary.as_ref();
    let complete = match a {
        Some(a) => a.complete == Some(true),
        None => r.is_some_and(|r| r.complete),
    };
    let mut checks = Checks {
        proof: "invalid",
        completeness: if complete { "complete" } else { "partial" },
        root: "single",
        freshness: "unchecked",
        data: "unchecked",
    };

    // Përgjigja duhet të jetë për llogarinë dhe slotin e kërkuar.
    match r {
        Some(r) if is_root_hex(&r.root) && r.slot.is_some() => {
            if r.pubkey != i.request_pubkey {
                errors.push("wrong_account");
            } else if i.request_slot.is_some_and(|req| r.slot.unwrap() > req) {
                errors.push("wrong_slot");
            } else if a.and_then(|a| a.slot).is_some_and(|s| r.slot != Some(s)) {
                errors.push("anchor_slot_mismatch");
            }
        }
        _ => errors.push("bad_response"),
    }

    // 1. Dëftesa.
    let rv = r.map(|r| r.verdict.as_str()).unwrap_or("invalid");
    if rv == "exists" || rv == "does_not_exist" {
        checks.proof = "valid";
    } else {
        errors.push("proof_invalid");
    }
    if rv == "does_not_exist" && !complete {
        errors.push("absence_incomplete");
    }

    // 2. Besimi te root-i.
    let mut trust = "single";
    let mut agreeing = 1u64;
    if a.is_some() {
        trust = "user";
    } else if let Some(r) = r {
        let mut conflicts = 0;
        for o in &i.others {
            if !o.ok || o.slot.is_none() || o.slot != r.slot {
                continue;
            }
            if o.root.as_deref() == Some(r.root.as_str()) {
                if rv == "does_not_exist" && !o.complete {
                    warnings.push("agreeing_incomplete");
                } else {
                    agreeing += 1;
                }
            } else {
                conflicts += 1;
            }
        }
        if i.endpoints >= 2 && agreeing as i64 >= p.min_agree {
            trust = "agreement";
        }
        let mut quorum_root: Option<&str> = None;
        if let Some(groups) = &i.quorum {
            if i.quorum_epoch_ok == Some(false) {
                warnings.push("quorum_epoch_unsupported");
            } else {
                let mut reached: Vec<&str> = Vec::new();
                for g in groups.iter().filter(|g| g.reached) {
                    if !reached.contains(&g.root.as_str()) {
                        reached.push(&g.root);
                    }
                }
                if reached.len() > 1 {
                    errors.push("quorum_split");
                } else if reached.len() == 1 {
                    quorum_root = Some(reached[0]);
                }
            }
        }
        match quorum_root {
            Some(q) if q == r.root => {
                trust = "quorum";
                if conflicts > 0 {
                    warnings.push("conflict_outvoted_by_quorum");
                }
            }
            Some(_) => errors.push("quorum_other_root"),
            None if conflicts > 0 => errors.push("root_conflict"),
            None => {}
        }
    }
    checks.root = trust;
    if trust_rank(trust).unwrap() < trust_rank(&p.min_trust).unwrap_or(99) {
        errors.push("trust_below_min");
    }

    // 3. Freskia. A14: me referencë vetëm-hash, sloti i serverit s'është i vërtetuar.
    let slot_trusted = !a.is_some_and(|a| a.slot.is_none());
    if i.request_slot.is_some() {
        checks.freshness = "historical";
    } else if !slot_trusted {
        checks.freshness = "unverifiable";
        if p.max_slot_lag.is_some() || p.min_slot.is_some() {
            errors.push("freshness_unverifiable");
        }
    } else if let Some(slot) = r.and_then(|r| r.slot) {
        if i.highest_seen.is_some_and(|h| slot < h) {
            checks.freshness = "rollback";
            errors.push("rollback");
        } else if p.min_slot.is_some_and(|m| slot < m) {
            checks.freshness = "stale";
            errors.push("below_min_slot");
        } else if let Some(lag) = p.max_slot_lag {
            match i.finalized_slot {
                None => {
                    checks.freshness = "unknown";
                    errors.push("freshness_unknown");
                }
                // A12: përpara referencës: finaliteti s'konfirmohet.
                Some(f) if slot > f => {
                    checks.freshness = "ahead";
                    errors.push("ahead_of_reference");
                }
                Some(f) if f - slot > lag => {
                    checks.freshness = "stale";
                    errors.push("stale");
                }
                Some(_) => checks.freshness = "fresh",
            }
        }
    }

    // 4. Të dhënat (A13: kur u kërkuan, vetëm përputhja jep sukses).
    match i.data {
        None => {}
        Some(DataStatus::Match) => checks.data = "match",
        Some(DataStatus::Mismatch) => {
            checks.data = "mismatch";
            errors.push("data_mismatch");
        }
        Some(DataStatus::Unavailable) => {
            checks.data = "unavailable";
            errors.push("data_unavailable");
        }
    }

    let has = |set: &[&str]| set.iter().any(|e| errors.contains(e));
    let verdict = if has(&["bad_response", "wrong_account", "wrong_slot", "anchor_slot_mismatch", "proof_invalid"]) {
        "invalid".to_string()
    } else if has(&["root_conflict", "quorum_other_root", "quorum_split"]) {
        "conflict".to_string()
    } else {
        rv.to_string()
    };
    let ok = errors.is_empty();
    let raise_floor = if ok && slot_trusted && matches!(checks.freshness, "fresh" | "unchecked") { r.and_then(|r| r.slot) } else { None };
    let mut w: Vec<&'static str> = Vec::new();
    for x in warnings {
        if !w.contains(&x) {
            w.push(x);
        }
    }
    Decision { ok, verdict, trust, agreeing, checks, errors, warnings: w, raise_floor }
}

/// Shpjegimi i shkurtër i çdo kodi, për daljen e veglave.
pub fn explain(code: &str) -> &'static str {
    match code {
        "config_min_trust" => "min-trust i panjohur (single | agreement | quorum)",
        "config_min_agree" => "min-agree duhet ≥ 2 dhe aq zile sa ai (shto --also, ose --min-trust single)",
        "config_quorum" => "min-trust quorum kërkon SOLANA_RPC_URL",
        "bad_response" => "përgjigje e pavlefshme nga zilja",
        "wrong_account" => "dëftesa është për një llogari tjetër",
        "wrong_slot" => "dëftesa është për një slot pas të kërkuarit",
        "proof_invalid" => "dëftesa NUK përputhet me root-in",
        "absence_incomplete" => "'nuk ekziston' nga një pemë që s'mbulon çdo llogari",
        "quorum_split" => "kuorumi u arrit për dy root-e të ndryshme",
        "quorum_other_root" => "kuorumi firmosi një root tjetër",
        "root_conflict" => "zilet japin root-e të ndryshme për të njëjtin slot",
        "trust_below_min" => "besimi te root-i më i ulët se min-trust",
        "rollback" => "sloti më i vjetër se një i parë më parë",
        "below_min_slot" => "sloti më i vjetër se min-slot",
        "freshness_unknown" => "freskia s'u kontrollua (sloti i finalizuar s'u mor)",
        "stale" => "gjendje e vjetër: shumë slote pas të finalizuarit",
        "data_mismatch" => "të dhënat e RPC-së NUK përputhen me vulën",
        "data_unavailable" => "krahasimi i kërkuar i të dhënave s'u krye (RPC-ja s'u përgjigj)",
        "ahead_of_reference" => "sloti është përpara slotit të finalizuar të RPC-së: finaliteti s'konfirmohet",
        "anchor_slot_mismatch" => "dëftesa s'është për slotin e referencës",
        "freshness_unverifiable" => "me root vetëm-hash, sloti s'vërtetohet: freskia s'mund të provohet (jep --root-slot)",
        "agreeing_incomplete" => "një zile pajtohet për root-in, por s'pohon pemë të plotë (s'numërohet për mungesë)",
        "quorum_epoch_unsupported" => "sloti në epokë të kaluar: tabela e stake-ut s'vlen për të",
        "conflict_outvoted_by_quorum" => "një zile jep root tjetër, por kuorumi vendosi",
        _ => "",
    }
}
