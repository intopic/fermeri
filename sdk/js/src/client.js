// ZiljaClient: një thirrje e vetme për kuletat: verifyAccount(pubkey).
import { verifyAccount, dataMatches, normalizeAccount, b58decode, bytesToHex } from "./verifier.js";
import { verifyQuorum, stakeTableFromVoteAccounts } from "./attest.js";

/** Nivelet e besimit te root-i, nga më i dobëti te më i forti. */
export const TRUST = Object.freeze({ single: 0, agreement: 1, quorum: 2 });

function b64(s) {
  const bin = atob(s);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

export class ZiljaClient {
  /**
   * @param {object} o
   * @param {string[]} o.endpoints adresat e zileve (të paktën një; më shumë = më shumë besim)
   * @param {string} [o.solanaRpc] RPC Solana për të krahasuar të dhënat dhe për tabelën e stake-ut
   * @param {number} [o.minAgree] sa zile duhet të japin të njëjtin root për nivelin "agreement" (parazgjedhja: 2)
   * @param {boolean} [o.quorum] kontrollo edhe firmat e validatorëve (kërkon solanaRpc)
   * @param {string} [o.genesisHash] hash-i i genesis-it (base58 ose hex) i zinxhirit që pret kuleta;
   *   pa të, merret nga solanaRpc (dhe i besohet asaj)
   * @param {"single"|"agreement"|"quorum"} [o.minTrust] nën këtë nivel, `ok` del false (parazgjedhja: "agreement", L-01)
   * @param {number} [o.timeoutMs] parazgjedhja: 10000
   * @param {typeof fetch} [o.fetch]
   */
  constructor(o) {
    if (!o?.endpoints?.length) throw new Error("duhet të paktën një adresë zileje");
    // L-04: e njëjta adresë e dhënë dy herë nuk numërohet si dy zile.
    this.endpoints = [...new Set(o.endpoints.map((e) => String(e).trim().replace(/\/+$/, "")))];
    this.solanaRpc = o.solanaRpc;
    this.minAgree = o.minAgree ?? 2;
    this.quorum = o.quorum ?? false;
    this.genesisHash = o.genesisHash;
    this.minTrust = o.minTrust ?? "agreement";
    this.timeoutMs = o.timeoutMs ?? 10_000;
    this.fetch = o.fetch ?? globalThis.fetch.bind(globalThis);
    if (this.quorum && !this.solanaRpc) throw new Error("quorum kërkon solanaRpc (për tabelën e stake-ut)");
    if (!(this.minTrust in TRUST)) throw new Error(`minTrust i panjohur: ${this.minTrust}`);
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
      const j = await r.json();
      if (j.error) throw new Error(`${method}: ${j.error.message}`);
      return j.result;
    } finally {
      clearTimeout(t);
    }
  }

  /**
   * Verifikon një llogari.
   * @param {string} pubkey base58
   * @param {{slot?: number}} [opts] slot i kaluar (vetëm zile me pemë në disk)
   */
  async verifyAccount(pubkey, opts = {}) {
    const warnings = [];
    const [primary, ...others] = this.endpoints;
    const params = opts.slot != null ? [pubkey, { slot: opts.slot }] : [pubkey];
    const r = await this.rpc(primary, "getAccountProof", params);
    const result = {
      ok: false,
      verdict: "invalid",
      slot: r?.slot,
      root: r?.root,
      account: null,
      trust: "single",
      agreeing: 1,
      endpoints: this.endpoints.length,
      quorum: null,
      data: null,
      warnings,
    };
    const bad = (why) => {
      warnings.push(why);
      result.verdict = "invalid";
      return result;
    };

    // 0) Përgjigja duhet të jetë për llogarinë dhe slotin që kërkuam (L-04, L-05).
    if (!r || typeof r !== "object" || !Number.isSafeInteger(r.slot) || r.slot < 0) return bad(`${primary}: përgjigje pa slot të vlefshëm`);
    if (r.pubkey !== pubkey) return bad(`${primary}: dëftesa është për një llogari tjetër`);
    if (opts.slot != null && r.slot > opts.slot) return bad(`${primary}: dëftesa është për slotin ${r.slot}, pas slotit të kërkuar ${opts.slot}`);
    const slot = r.slot;

    // 1) Dëftesa kundër root-it të zilës së parë. Vlerat kontrollohen rreptë (F-03) dhe
    //    kthehen ato të kontrolluarat, jo përgjigja e papërpunuar.
    let account = null;
    if (r.account != null) {
      try {
        account = normalizeAccount(r.account);
      } catch (e) {
        return bad(`${primary}: llogari e pavlefshme në përgjigje (${e.message})`);
      }
    }
    const verdict = await verifyAccount(r.root, pubkey, account, r.proof);
    result.verdict = verdict;
    result.account = verdict === "exists" ? account : null;
    if (verdict === "invalid") return bad(`zilja ${primary} dha një dëftesë që nuk përputhet me root-in e vet`);
    if (r.complete !== true) {
      warnings.push("pema e kësaj zileje nuk mbulon ende çdo llogari (nisi pa snapshot)");
      // L-02: "nuk ekziston" nga një pemë jo e plotë s'do të thotë asgjë.
      if (verdict === "does_not_exist") {
        warnings.push("dëftesa 'nuk ekziston' nga një pemë jo e plotë nuk pranohet");
        return result;
      }
    }

    // 2) Zilet e tjera, paralelisht (Faster-3): a japin të njëjtin root për të njëjtin slot (Simpler-5)?
    const answers = await Promise.allSettled(others.map((ep) => this.rpc(ep, "getZiljaRoot", [slot])));
    let disagree = 0;
    answers.forEach((a, i) => {
      const ep = others[i];
      if (a.status === "rejected") warnings.push(`${ep}: ${a.reason?.message ?? a.reason}`);
      else if (!a.value || a.value.slot !== slot) warnings.push(`${ep}: s'mund të krahasohet në slotin ${slot}`);
      else if (a.value.root === r.root) result.agreeing++;
      else {
        disagree++;
        warnings.push(`${ep} jep root TJETËR për slotin ${slot}`);
      }
    });
    if (disagree > 0) {
      result.verdict = "invalid";
      result.account = null;
      return result;
    }
    if (this.endpoints.length > 1 && result.agreeing >= Math.min(this.minAgree, this.endpoints.length)) {
      result.trust = "agreement";
    }

    // 3) Firmat e validatorëve (> 2/3 e stake-ut).
    if (this.quorum) {
      try {
        const va = await this.rpc(this.solanaRpc, "getVoteAccounts", [{ commitment: "finalized" }]);
        const table = stakeTableFromVoteAccounts(va);
        let g = this.genesisHash;
        if (g == null) {
          g = await this.rpc(this.solanaRpc, "getGenesisHash");
          warnings.push("hash-i i genesis-it u mor nga solanaRpc; jepe me genesisHash për ta fiksuar");
        }
        const genesisHex = /^[0-9a-fA-F]{64}$/.test(g) ? g.toLowerCase() : bytesToHex(b58decode(g));
        const got = await Promise.allSettled(this.endpoints.map((ep) => this.rpc(ep, "getZiljaAttestation", [slot])));
        const atts = got
          .filter((a) => a.status === "fulfilled" && a.value?.root === r.root && a.value?.slot === slot)
          .map((a) => ({ validator: a.value.validator, signer: a.value.signer, authorization: a.value.authorization, signature: a.value.signature }));
        const q = await verifyQuorum(genesisHex, slot, r.root, atts, table);
        const pct = q.totalStake > 0n ? Number((q.attestedStake * 10000n) / q.totalStake) / 100 : 0;
        result.quorum = { reached: q.reached, stakePercent: pct, validSignatures: q.validSignatures };
        if (q.reached) result.trust = "quorum";
        warnings.push("tabela e stake-ut u mor nga solanaRpc dhe i besohet asaj");
      } catch (e) {
        warnings.push(`kuorumi s'u kontrollua: ${e.message}`);
      }
    }

    // 4) Të dhënat nga RPC-ja e zakonshme: përputhen me vulën? (L-07) Kthehen bajtet e verifikuara.
    if (this.solanaRpc && result.account && opts.slot == null) {
      try {
        const info = await this.rpc(this.solanaRpc, "getAccountInfo", [pubkey, { encoding: "base64", commitment: "finalized" }]);
        const rpcSlot = info?.context?.slot;
        if (!info?.value) {
          result.data = { checked: true, matches: false, rpcSlot, bytes: null };
        } else {
          const bytes = b64(info.value.data[0]);
          const same = (await dataMatches(result.account, bytes)) && BigInt(info.value.lamports) === result.account.lamports;
          result.data = { checked: true, matches: same, rpcSlot, bytes: same ? bytes : null };
          if (!same) warnings.push(`RPC-ja (slot ${rpcSlot}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit ${slot}, ose RPC-ja gënjen`);
        }
      } catch (e) {
        warnings.push(`të dhënat s'u krahasuan: ${e.message}`);
      }
    }

    if (result.trust === "single" && this.endpoints.length === 1) {
      warnings.push("vetëm një zile: root-i i besohet asaj; shto zile të tjera ose quorum për më shumë besim");
    }
    result.ok = TRUST[result.trust] >= TRUST[this.minTrust];
    return result;
  }
}
