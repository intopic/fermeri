// ZiljaClient: një thirrje e vetme për kuletat: verifyAccount(pubkey). Vendimi merret nga
// politika e përbashkët (policy.js), e njëjta me web-in dhe CLI-në (P-11).
import { verifyAccount, dataMatches, normalizeAccount, b58decode, bytesToHex, toU64 } from "./verifier.js";
import { verifyQuorum, stakeTableFromVoteAccounts } from "./attest.js";
import { decide, configErrors, DEFAULTS, TRUST } from "./policy.js";
import { parseJsonExact } from "./json.js";

export { TRUST };

/**
 * A17: lexon trupin e përgjigjes deri në `max` bajte; më shumë = gabim (pa e mbajtur të gjithë në RAM
 * kur `fetch` jep stream). Kthen `null` kur përgjigjja s'ka as `body`, as `text` (mock të thjeshta).
 */
async function readLimited(r, max, what) {
  const cl = Number(r.headers?.get?.("content-length"));
  if (Number.isFinite(cl) && cl > max) throw new Error(`${what}: përgjigje ${cl} B, mbi kufirin ${max} B`);
  if (r.body && typeof r.body.getReader === "function") {
    const rd = r.body.getReader();
    const parts = [];
    let n = 0;
    for (;;) {
      const { done, value } = await rd.read();
      if (done) break;
      n += value.length;
      if (n > max) {
        await rd.cancel().catch(() => {});
        throw new Error(`${what}: përgjigje mbi kufirin ${max} B`);
      }
      parts.push(value);
    }
    const all = new Uint8Array(n);
    let o = 0;
    for (const p of parts) {
      all.set(p, o);
      o += p.length;
    }
    return new TextDecoder().decode(all);
  }
  if (typeof r.text === "function") {
    const t = await r.text();
    if (t.length > max) throw new Error(`${what}: përgjigje mbi kufirin ${max} B`);
    return t;
  }
  return null;
}

/** A11: u64 i saktë nga JSON-i (shih `rpc`): BigInt, tekst dhjetor ose Number i sigurt; ndryshe gabim. */
function exactU64(v) {
  if (typeof v === "bigint") return v;
  if (typeof v === "string" && /^(0|[1-9][0-9]{0,19})$/.test(v)) return BigInt(v);
  if (typeof v === "number" && Number.isSafeInteger(v) && v >= 0) return BigInt(v);
  throw new Error(`numër i pasaktë ose jashtë u64: ${v}`);
}

function b64(s) {
  const bin = atob(s);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

export class ZiljaClient {
  /**
   * @param {object} o
   * @param {string[]} o.endpoints adresat e zileve (të paktën një; më shumë = më shumë besim)
   * @param {string} [o.solanaRpc] RPC Solana: freskia (sloti i finalizuar), të dhënat, stake-u
   * @param {number} [o.minAgree] sa zile duhet të japin të njëjtin root për "agreement" (parazgjedhja 2).
   *   S'ulet kurrë vetë: më pak zile se kaq me minTrust "agreement" = gabim konfigurimi.
   * @param {boolean} [o.quorum] kontrollo edhe firmat e validatorëve (kërkon solanaRpc)
   * @param {string} [o.genesisHash] genesis-i (base58 ose hex); pa të merret nga solanaRpc
   * @param {"single"|"agreement"|"quorum"} [o.minTrust] parazgjedhja "agreement" (L-01)
   * @param {number|null} [o.maxSlotLag] sa slote pas slotit të finalizuar të solanaRpc pranohet
   *   gjendja si "aktuale" (parazgjedhja 150 kur ka solanaRpc; null = pa kontroll)
   * @param {number} [o.minSlot] slot minimal i pranuar
   * @param {string|{root: string, slot?: number, complete?: boolean}} [o.root] A14: referenca që përdoruesi e
   *   njeh vetë (besimi "user"). Vetëm hash: vërteton përfshirjen te ai root, por jo slotin, freskinë apo
   *   mungesën. Me `slot` (dhe `complete: true` për mungesë): dëftesa duhet të jetë për atë slot.
   * @param {boolean} [o.compareData] krahaso të dhënat me solanaRpc (parazgjedhja: po, kur ka solanaRpc)
   * @param {number} [o.timeoutMs] parazgjedhja: 10000
   * @param {typeof fetch} [o.fetch]
   */
  constructor(o) {
    if (!o?.endpoints?.length) throw new Error("duhet të paktën një adresë zileje");
    // L-04: e njëjta adresë e dhënë dy herë nuk numërohet si dy zile.
    this.endpoints = [...new Set(o.endpoints.map((e) => String(e).trim().replace(/\/+$/, "")))];
    this.solanaRpc = o.solanaRpc;
    this.minAgree = o.minAgree ?? DEFAULTS.minAgree;
    this.quorum = o.quorum ?? false;
    this.genesisHash = o.genesisHash;
    /** R5-1: revokime shtesë (p.sh. të publikuara nga validatori); bashkohen me ato të zileve. */
    this.revocations = o.revocations ?? [];
    this.minTrust = o.minTrust ?? DEFAULTS.minTrust;
    this.maxSlotLag = o.maxSlotLag !== undefined ? o.maxSlotLag : this.solanaRpc ? DEFAULTS.maxSlotLagWithRpc : null;
    this.minSlot = o.minSlot ?? null;
    const ref = o.root == null ? null : typeof o.root === "object" ? o.root : { root: o.root };
    this.rootOverride = ref ? String(ref.root ?? "").trim().toLowerCase() : null;
    this.anchor = ref ? { slot: ref.slot ?? null, complete: ref.complete ?? null } : null;
    if (this.anchor?.slot != null && !Number.isSafeInteger(this.anchor.slot)) throw new Error("root.slot duhet numër i plotë");
    this.compareData = o.compareData ?? true;
    this.timeoutMs = o.timeoutMs ?? 10_000;
    // A17: kufijtë e përgjigjeve (bajte). `timeoutMs` mbulon edhe leximin e trupit.
    this.maxResponseBytes = { zilja: 1 << 20, solana: 64 << 20, ...(o.maxResponseBytes ?? {}) };
    this.fetch = o.fetch ?? globalThis.fetch.bind(globalThis);
    // P-11: mbrojtja nga kthimi te një gjendje më e vjetër (për këtë klient).
    this.highestSeen = null;
    if (this.quorum && !this.solanaRpc) throw new Error("quorum kërkon solanaRpc (për tabelën e stake-ut)");
    if (this.rootOverride != null && !/^[0-9a-f]{64}$/.test(this.rootOverride)) throw new Error("root duhet 64 shifra hex");
    const ce = configErrors({ minTrust: this.minTrust, minAgree: this.minAgree, endpoints: this.endpoints.length, quorumEnabled: this.quorum, rootOverride: this.rootOverride != null });
    if (ce.length) throw new Error(`konfigurim i pavlefshëm: ${ce.join(", ")} (minTrust ${this.minTrust}, minAgree ${this.minAgree}, ${this.endpoints.length} zile)`);
  }

  async rpc(url, method, params = []) {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), this.timeoutMs);
    try {
      const r = await this.fetch(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
        signal: ctl.signal,
      });
      // A17: përgjigja lexohet me kufi madhësie (zilja: dëftesa të vogla; solanaRpc: tabela e stake-ut).
      // A11: numrat e plotë mbi 2^53 - 1 (stake, lamports) lexohen pa humbje si tekst.
      const max = url === this.solanaRpc ? this.maxResponseBytes.solana : this.maxResponseBytes.zilja;
      const text = await readLimited(r, max, `${method} nga ${url}`);
      const j = text != null ? parseJsonExact(text) : await r.json();
      if (j.error) throw new Error(`${method}: ${j.error.message}`);
      return j.result;
    } finally {
      clearTimeout(t);
    }
  }

  /**
   * Verifikon një llogari. `ok` = dëftesa e vlefshme + besim te root-i ≥ minTrust + freski +
   * (kur u kontrolluan) të dhëna që përputhen. `checks` i jep secilën veç.
   * @param {string} pubkey base58
   * @param {{slot?: number}} [opts] slot i kaluar (vetëm zile me pemë në disk)
   */
  async verifyAccount(pubkey, opts = {}) {
    const warnings = [];
    const [primary, ...others] = this.endpoints;
    const reqSlot = opts.slot ?? null;
    const r = await this.rpc(primary, "getAccountProof", reqSlot != null ? [pubkey, { slot: reqSlot }] : [pubkey]);
    const slot = Number.isSafeInteger(r?.slot) ? r.slot : null;
    const rootHex = this.rootOverride ?? r?.root;

    // 1. Dëftesa. Vlerat kontrollohen rreptë (F-03) dhe kthehen ato të kontrolluarat.
    let account = null;
    let verdict = "invalid";
    try {
      if (r?.account != null) account = normalizeAccount(r.account);
      verdict = await verifyAccount(rootHex, pubkey, account, r?.proof);
    } catch (e) {
      warnings.push(`${primary}: përgjigje e pavlefshme (${e.message})`);
    }
    if (verdict === "invalid") warnings.push(`zilja ${primary} dha një dëftesë që nuk përputhet me root-in`);

    // 2. Zilet e tjera, paralelisht: root-i në të njëjtin slot.
    let otherAnswers = [];
    if (slot != null && this.rootOverride == null) {
      const got = await Promise.allSettled(others.map((ep) => this.rpc(ep, "getZiljaRoot", [slot])));
      otherAnswers = got.map((a, i) => {
        if (a.status === "rejected") {
          warnings.push(`${others[i]}: ${a.reason?.message ?? a.reason}`);
          return { ok: false };
        }
        if (a.value?.root !== r.root) warnings.push(`${others[i]} jep root tjetër për slotin ${slot}`);
        return { ok: true, slot: a.value?.slot, root: a.value?.root, complete: a.value?.complete === true };
      });
    }

    // 3. Kuorumi: firmat grupohen sipas root-it; tabela e stake-ut duhet të jetë e epokës së slotit.
    let quorum = null;
    let quorumEpochOk = null;
    let quorumReport = null;
    if (this.quorum && slot != null && this.rootOverride == null) {
      try {
        // A10: pesha është pamja e RPC-së (getVoteAccounts), jo tabela e konsensusit e epokës. Vlen
        // vetëm për një slot brenda epokës së saj (të dy kufijtë), dhe vetëm kur epoka s'ndryshoi gjatë
        // leximit (getEpochInfo para dhe pas).
        const fin = [{ commitment: "finalized" }];
        const e1 = await this.rpc(this.solanaRpc, "getEpochInfo", fin);
        const va = await this.rpc(this.solanaRpc, "getVoteAccounts", fin);
        const e2 = await this.rpc(this.solanaRpc, "getEpochInfo", fin);
        const start = toU64(e1?.absoluteSlot) - toU64(e1?.slotIndex);
        const end = start + toU64(e1?.slotsInEpoch) - 1n;
        const coherent = e1?.epoch === e2?.epoch;
        quorumEpochOk = coherent && BigInt(slot) >= start && BigInt(slot) <= end;
        if (!coherent) warnings.push("epoka ndryshoi gjatë leximit të tabelës së stake-ut: kuorumi s'numërohet");
        else if (!quorumEpochOk) warnings.push(`sloti ${slot} s'është në epokën e tabelës së stake-ut (${start}–${end}): kuorumi s'numërohet`);
        const table = stakeTableFromVoteAccounts(va);
        let g = this.genesisHash;
        if (g == null) {
          g = await this.rpc(this.solanaRpc, "getGenesisHash");
          warnings.push("hash-i i genesis-it u mor nga solanaRpc; jepe me genesisHash për ta fiksuar");
        }
        const genesisHex = /^[0-9a-fA-F]{64}$/.test(g) ? g.toLowerCase() : bytesToHex(b58decode(g));
        const got = await Promise.allSettled(this.endpoints.map((ep) => this.rpc(ep, "getZiljaAttestation", [slot])));
        // R5-1: revokimet nga cilado zile (secila verifikohet te verifyQuorum) + ato të dhëna nga thirrësi.
        const revs = [...(this.revocations ?? [])];
        for (const x of await Promise.allSettled(this.endpoints.map((ep) => this.rpc(ep, "getZiljaRevocations", [])))) {
          if (x.status === "fulfilled" && Array.isArray(x.value)) revs.push(...x.value.filter((r) => r && typeof r === "object"));
        }
        const byRoot = new Map();
        for (const a of got) {
          const x = a.status === "fulfilled" ? a.value : null;
          if (!x || x.slot !== slot || typeof x.root !== "string" || !/^[0-9a-f]{64}$/.test(x.root)) continue;
          if (!byRoot.has(x.root)) byRoot.set(x.root, []);
          byRoot.get(x.root).push({ validator: x.validator, signer: x.signer, authorization: x.authorization, signature: x.signature });
        }
        quorum = [];
        for (const [root, atts] of byRoot) {
          const q = await verifyQuorum(genesisHex, toU64(e1.epoch), slot, root, atts, table, revs);
          quorum.push({ root, reached: q.reached });
          if (root === r.root) {
            const pct = q.totalStake > 0n ? Number((q.attestedStake * 10000n) / q.totalStake) / 100 : 0;
            quorumReport = { reached: q.reached, stakePercent: pct, validSignatures: q.validSignatures };
          }
        }
        warnings.push("pesha e kuorumit është pamja e solanaRpc (getVoteAccounts), jo tabela e konsensusit e epokës; i besohet asaj");
      } catch (e) {
        warnings.push(`kuorumi s'u kontrollua: ${e.message}`);
      }
    }

    // 4. Freskia: sloti i finalizuar nga solanaRpc.
    let finalizedSlot = null;
    if (this.solanaRpc && reqSlot == null && this.maxSlotLag != null) {
      try {
        finalizedSlot = await this.rpc(this.solanaRpc, "getSlot", [{ commitment: "finalized" }]);
      } catch (e) {
        warnings.push(`sloti i finalizuar s'u mor: ${e.message}`);
      }
    }

    // 5. Të dhënat nga RPC-ja (L-07, A13). Kur kërkohen, vetëm përputhja jep sukses: një RPC që s'përgjigjet
    // jep "unavailable", jo "unchecked". Krahasohen lamports, owner, executable dhe bajtet (dataLen + hash).
    // Kthehen bajtet e verifikuara.
    let data = null;
    let dataReport = null;
    if (this.solanaRpc && this.compareData && verdict === "exists" && reqSlot == null) {
      try {
        const info = await this.rpc(this.solanaRpc, "getAccountInfo", [pubkey, { encoding: "base64", commitment: "finalized" }]);
        const rpcSlot = info?.context?.slot;
        let same = false;
        let bytes = null;
        if (info?.value) {
          bytes = b64(info.value.data[0]);
          same =
            (await dataMatches(account, bytes)) &&
            exactU64(info.value.lamports) === account.lamports &&
            info.value.owner === account.owner &&
            info.value.executable === account.executable;
        }
        data = { status: same ? "match" : "mismatch" };
        dataReport = { checked: true, matches: same, rpcSlot, bytes: same ? bytes : null };
        if (!same) warnings.push(`RPC-ja (slot ${rpcSlot}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit ${slot}, ose RPC-ja gënjen`);
      } catch (e) {
        data = { status: "unavailable" };
        dataReport = { checked: false, matches: false, rpcSlot: null, bytes: null };
        warnings.push(`të dhënat s'u krahasuan: ${e.message}`);
      }
    }

    const d = decide({
      policy: { minTrust: this.minTrust, minAgree: this.minAgree, maxSlotLag: this.maxSlotLag, minSlot: this.minSlot },
      request: { pubkey, slot: reqSlot },
      endpoints: this.endpoints.length,
      quorumEnabled: this.quorum,
      anchor: this.anchor,
      primary: r && typeof r === "object" ? { pubkey: r.pubkey, slot: r.slot, root: rootHex, complete: r.complete === true, verdict } : null,
      others: otherAnswers,
      quorum,
      quorumEpochOk,
      finalizedSlot: Number.isSafeInteger(finalizedSlot) ? finalizedSlot : null,
      highestSeen: this.highestSeen,
      data,
    });
    if (d.raiseFloor != null) this.highestSeen = Math.max(this.highestSeen ?? 0, d.raiseFloor);
    if (d.trust === "single" && this.endpoints.length === 1 && this.rootOverride == null) {
      warnings.push("vetëm një zile: root-i i besohet asaj; shto zile të tjera ose quorum për më shumë besim");
    }
    return {
      ok: d.ok,
      verdict: d.verdict,
      slot: r?.slot,
      root: rootHex,
      account: d.checks.proof === "valid" && verdict === "exists" ? account : null,
      trust: d.trust,
      agreeing: d.agreeing,
      endpoints: this.endpoints.length,
      quorum: quorumReport,
      data: dataReport,
      checks: d.checks,
      errors: d.errors,
      warnings: [...warnings, ...d.warnings],
    };
  }
}
