// Logjika e banorit, e përbashkët për faqen demo dhe për Node (web/check.mjs).
import { verifyAccount, dataMatches } from "../sdk/js/src/index.js";

async function rpc(url, method, params = []) {
  const r = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
  });
  const j = await r.json();
  if (j.error) throw new Error(`${method}: ${j.error.message}`);
  return j.result;
}

function b64(s) {
  const bin = atob(s);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

/**
 * `ok` = dëftesa përputhet me root-in. `trust` tregon nga erdhi root-i: "self" (i njëjti
 * server, pra i besohet atij), "second" (një zile e dytë dha të njëjtin), "user" (e dhe ti).
 * @returns {Promise<{ok: boolean, trust: "self"|"second"|"user", steps: {ok: boolean|null, text: string}[], result: object}>}
 */
export async function checkAccount({ ziljaUrl, pubkey, solanaRpc, rootOverride, secondZilja }) {
  const steps = [];
  const r = await rpc(ziljaUrl, "getAccountProof", [pubkey]);
  steps.push({ ok: null, text: `Zilja: slot ${r.slot}, dëftesë ${r.proofBytes} bajt${r.complete ? "" : " (pema nuk mbulon ende çdo llogari)"}` });

  let root = r.root;
  let trust = "self";
  if (rootOverride) {
    trust = "user";
    root = rootOverride.trim().toLowerCase();
    steps.push({ ok: null, text: "Root-i: ai që dhe ti" });
  } else if (secondZilja) {
    const r2 = await rpc(secondZilja, "getZiljaRoot", [r.slot]).catch(() => null);
    if (r2 && r2.slot === r.slot) {
      const same = r2.root === r.root;
      steps.push({ ok: same, text: same ? "Zilja e dytë jep të njëjtin root për këtë slot" : "Zilja e dytë jep root TJETËR për të njëjtin slot" });
      if (!same) return { ok: false, trust, steps, result: r };
      trust = "second";
    } else {
      steps.push({ ok: null, text: `Zilja e dytë s'ka root për slotin ${r.slot}, s'mund të krahasohet` });
    }
  } else {
    steps.push({ ok: null, text: "Root-i: nga i njëjti server (ende pa firmë validatorësh)" });
  }

  const verdict = await verifyAccount(root, pubkey, r.account, r.proof);
  const ok = verdict !== "invalid";
  steps.push({
    ok,
    text:
      verdict === "exists"
        ? `Llogaria ekziston: ${Number(r.account.lamports) / 1e9} SOL`
        : verdict === "does_not_exist"
          ? "Llogaria NUK ekziston në këtë root"
          : "Dëftesa NUK përputhet me root-in: dikush po gënjen",
  });

  if (ok && r.account && solanaRpc) {
    const info = await rpc(solanaRpc, "getAccountInfo", [pubkey, { encoding: "base64", commitment: "finalized" }]);
    if (!info.value) {
      steps.push({ ok: false, text: `RPC-ja (slot ${info.context.slot}) nuk e gjen llogarinë` });
    } else {
      const same = (await dataMatches(r.account, b64(info.value.data[0]))) && BigInt(info.value.lamports) === BigInt(r.account.lamports);
      steps.push({
        ok: same ? true : null,
        text: same
          ? `Të dhënat nga RPC-ja (slot ${info.context.slot}) përputhen me vulën`
          : `RPC-ja (slot ${info.context.slot}) jep të dhëna të tjera: ose llogaria ndryshoi pas slotit ${r.slot}, ose RPC-ja gënjen`,
      });
    }
  }
  return { ok, trust, steps, result: r };
}
