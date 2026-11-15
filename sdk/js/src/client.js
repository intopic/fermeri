// ZiljaClient: një thirrje e vetme për kuletat: verifyAccount(pubkey).
import { verifyAccount, dataMatches } from "./verifier.js";
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
   * @param {"single"|"agreement"|"quorum"} [o.minTrust] nën këtë nivel, `ok` del false (parazgjedhja: "single")
   * @param {number} [o.timeoutMs] parazgjedhja: 10000
   * @param {typeof fetch} [o.fetch]
   */
  constructor(o) {
    if (!o?.endpoints?.length) throw new Error("duhet të paktën një adresë zileje");
    this.endpoints = o.endpoints;
    this.solanaRpc = o.solanaRpc;
    this.minAgree = o.minAgree ?? 2;
    this.quorum = o.quorum ?? false;
    this.minTrust = o.minTrust ?? "single";
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
    const slot = r.slot;

    // 1) Dëftesa kundër root-it të zilës së parë.
    const verdict = await verifyAccount(r.root, pubkey, r.account, r.proof);
    const result = {
      ok: false,
      verdict,
      slot,
      root: r.root,
      account: r.account
        ? {
            lamports: BigInt(r.account.lamports),
            owner: r.account.owner,
            executable: r.account.executable,
            dataLen: Number(r.account.dataLen),
            dataHash: r.account.dataHash,
          }
        : null,
      trust: "single",
      agreeing: 1,
      endpoints: this.endpoints.length,
      quorum: null,
      data: null,
      warnings,
    };
    if (verdict === "invalid") {
      warnings.push(`zilja ${primary} dha një dëftesë që nuk përputhet me root-in e vet`);
      return result;
    }
    if (!r.complete) warnings.push("pema e kësaj zileje nuk mbulon ende çdo llogari (nisi pa snapshot)");

    // 2) Zilet e tjera: a japin të njëjtin root për të njëjtin slot?
    let disagree = 0;
    for (const ep of others) {
      try {
        const o = await this.rpc(ep, "getAccountProof", [pubkey, { slot }]).catch(async () => {
          // Zile me pemë në RAM: ka vetëm slotin e fundit.
          const z = await this.rpc(ep, "getZiljaRoot");
          return z.slot === slot ? z : null;
        });
        if (!o) {
          warnings.push(`${ep}: s'mund të krahasohet në slotin ${slot}`);
        } else if (o.root === r.root) {
          result.agreeing++;
        } else {
          disagree++;
          warnings.push(`${ep} jep root TJETËR për slotin ${slot}`);
        }
      } catch (e) {
        warnings.push(`${ep}: ${e.message}`);
      }
    }
    if (disagree > 0) {
      result.verdict = "invalid";
      return result;
    }
    if (result.agreeing >= Math.min(this.minAgree, this.endpoints.length) && this.endpoints.length > 1) {
      result.trust = "agreement";
    }

    // 3) Firmat e validatorëve (> 2/3 e stake-ut).
    if (this.quorum) {
      try {
        const va = await this.rpc(this.solanaRpc, "getVoteAccounts", [{ commitment: "finalized" }]);
        const table = stakeTableFromVoteAccounts(va);
        const atts = [];
        for (const ep of this.endpoints) {
          try {
            const a = await this.rpc(ep, "getZiljaAttestation", [slot]);
            if (a.root === r.root) atts.push({ validator: a.validator, signature: a.signature });
          } catch {
            // kjo zile s'firmos ose s'e ka këtë slot
          }
        }
        const q = await verifyQuorum(slot, r.root, atts, table);
        const pct = q.totalStake > 0n ? Number((q.attestedStake * 10000n) / q.totalStake) / 100 : 0;
        result.quorum = { reached: q.reached, stakePercent: pct, validSignatures: q.validSignatures };
        if (q.reached) result.trust = "quorum";
        warnings.push("tabela e stake-ut u mor nga solanaRpc dhe i besohet asaj");
      } catch (e) {
        warnings.push(`kuorumi s'u kontrollua: ${e.message}`);
      }
    }

    // 4) Të dhënat nga RPC-ja e zakonshme përputhen me vulën?
    if (this.solanaRpc && result.account && opts.slot == null) {
      try {
        const info = await this.rpc(this.solanaRpc, "getAccountInfo", [pubkey, { encoding: "base64", commitment: "finalized" }]);
        if (!info.value) {
          result.data = { checked: true, matches: false, rpcSlot: info.context.slot };
        } else {
          const same =
            (await dataMatches(r.account, b64(info.value.data[0]))) && BigInt(info.value.lamports) === result.account.lamports;
          result.data = { checked: true, matches: same, rpcSlot: info.context.slot };
          if (!same) warnings.push(`RPC-ja (slot ${info.context.slot}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit ${slot}, ose RPC-ja gënjen`);
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
