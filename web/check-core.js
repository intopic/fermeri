// Logjika e banorit, e përbashkët për faqen demo dhe për Node (web/check.mjs).
// P-11: vendimi merret nga i njëjti kod si SDK-ja (ZiljaClient + policy.js), ndaj i njëjti rast jep
// të njëjtin vendim në web, SDK dhe CLI. Kjo skedë vetëm i paraqet kontrollet veç e veç.
import { ZiljaClient, canonicalEndpoint } from "../sdk/js/src/index.js";

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
  // G19: dy vlerat që SDK-ja jep, por faqja s'i shpjegonte ("undefined").
  ahead: "PËRPARA referencës: sloti është pas atij që dhe ti, s'pranohet",
  unverifiable: "s'mund të vërtetohet: root-i që dhe ti s'ka slot",
};

// Mbrojtja nga kthimi pas për gjithë seancën e faqes. G19: veç për çdo kontekst (zilet + RPC-ja),
// që një zile ose rrjet tjetër (p.sh. devnet pas mainnet-it) s'duket si "kthim pas".
const highestSeen = new Map();
const contextKey = (urls, solanaRpc) => JSON.stringify([urls.map(canonicalEndpoint).sort(), solanaRpc ? canonicalEndpoint(solanaRpc) : null]);

/**
 * @param {object} o
 * @param {string} o.ziljaUrl
 * @param {string} o.pubkey
 * @param {string} [o.solanaRpc]
 * @param {string} [o.rootOverride]
 * @param {string} [o.secondZilja]
 * @param {"single"|"agreement"|"quorum"} [o.minTrust] parazgjedhja "single" (një zile mjafton), si SDK-ja
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
  const ctx = contextKey([ziljaUrl, secondZilja].filter(Boolean), solanaRpc);
  client.highestSeen = highestSeen.get(ctx) ?? null;
  const r = await client.verifyAccount(pubkey);
  if (r.ok && r.checks.freshness !== "historical") highestSeen.set(ctx, client.highestSeen);
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
  steps.push({ ok: c.freshness === "fresh" ? true : ["stale", "rollback", "unknown", "ahead"].includes(c.freshness) ? false : null, text: `Freskia: ${FRESH_TEXT[c.freshness] ?? c.freshness}` });
  if (c.data !== "unchecked") steps.push({ ok: c.data === "match", text: c.data === "match" ? "Të dhënat nga RPC-ja përputhen me vulën" : "RPC-ja jep të dhëna të tjera nga vula" });
  return { ok: r.ok, trust: r.trust, verdict: r.verdict, checks: c, errors: r.errors, steps, result: r };
}
