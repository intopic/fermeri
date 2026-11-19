// Logjika e banorit, e përbashkët për faqen demo dhe për Node (web/check.mjs).
// P-11: vendimi merret nga i njëjti kod si SDK-ja (ZiljaClient + policy.js), ndaj i njëjti rast jep
// të njëjtin vendim në web, SDK dhe CLI. Kjo skedë vetëm i paraqet kontrollet veç e veç.
import { ZiljaClient } from "../sdk/js/src/index.js";

const TRUST_TEXT = {
  single: "nga një zile e vetme: i besohet asaj",
  agreement: "disa zile të pavarura dhanë të njëjtin root",
  quorum: "validatorët me > 2/3 e stake-ut e firmosën",
  user: "root-i që dhe ti",
};
const FRESH_TEXT = {
  fresh: "e freskët: afër slotit të finalizuar të RPC-së",
  stale: "E VJETËR: larg slotit të finalizuar; s'është gjendja aktuale",
  rollback: "KTHIM PAS: më e vjetër se një përgjigje e mëparshme",
  unknown: "e panjohur: sloti i finalizuar s'u mor",
  unchecked: "s'u kontrollua (jep një RPC Solana)",
  historical: "slot i kërkuar (historik), jo gjendja aktuale",
};

// Mbrojtja nga kthimi pas për gjithë seancën e faqes.
let highestSeen = null;

/**
 * @param {object} o
 * @param {string} o.ziljaUrl
 * @param {string} o.pubkey
 * @param {string} [o.solanaRpc]
 * @param {string} [o.rootOverride]
 * @param {string} [o.secondZilja]
 * @param {"single"|"agreement"|"quorum"} [o.minTrust] parazgjedhja "agreement", si SDK-ja
 * @returns {Promise<{ok: boolean, trust: string|null, verdict: string, checks: object|null, errors: string[], steps: {ok: boolean|null, text: string}[], result: object|null}>}
 */
export async function checkAccount({ ziljaUrl, pubkey, solanaRpc, rootOverride, secondZilja, minTrust }) {
  const steps = [];
  let client;
  try {
    client = new ZiljaClient({ endpoints: [ziljaUrl, secondZilja].filter(Boolean), solanaRpc, root: rootOverride || undefined, minTrust });
  } catch (e) {
    const errors = e.message.match(/config_[a-z_]+/g) ?? ["config"];
    steps.push({ ok: false, text: `Konfigurimi: ${e.message}. Shto një zile të dytë, jep root-in, ose zgjidh nivelin "single" me vetëdije.` });
    return { ok: false, trust: null, verdict: "invalid", checks: null, errors, steps, result: null };
  }
  client.highestSeen = highestSeen;
  const r = await client.verifyAccount(pubkey);
  if (r.ok && r.checks.freshness !== "historical") highestSeen = client.highestSeen;
  const c = r.checks;
  steps.push({ ok: null, text: `Zilja: slot ${r.slot}${c.completeness === "complete" ? "" : " (pema nuk mbulon çdo llogari)"}` });
  steps.push({
    ok: c.proof === "valid",
    text:
      c.proof !== "valid"
        ? "Dëftesa NUK përputhet me root-in: dikush po gënjen"
        : r.account
          ? `Dëftesa e vlefshme: llogaria ekziston, ${r.account.lamports} lamports (${Number(r.account.lamports) / 1e9} SOL)`
          : r.errors.includes("absence_incomplete")
            ? "Dëftesa thotë 'nuk ekziston', por pema s'mbulon çdo llogari: s'pranohet"
            : "Dëftesa e vlefshme: llogaria NUK ekziston në këtë root",
  });
  steps.push({
    ok: r.errors.includes("trust_below_min") || r.verdict === "conflict" ? false : r.trust === "single" ? null : true,
    text: r.verdict === "conflict" ? "Root-i: zilet japin root-e të ndryshme për të njëjtin slot" : `Root-i: ${TRUST_TEXT[r.trust]}`,
  });
  steps.push({ ok: c.freshness === "fresh" ? true : ["stale", "rollback", "unknown"].includes(c.freshness) ? false : null, text: `Freskia: ${FRESH_TEXT[c.freshness]}` });
  if (c.data !== "unchecked") steps.push({ ok: c.data === "match", text: c.data === "match" ? "Të dhënat nga RPC-ja përputhen me vulën" : "RPC-ja jep të dhëna të tjera nga vula" });
  return { ok: r.ok, trust: r.trust, verdict: r.verdict, checks: c, errors: r.errors, steps, result: r };
}
