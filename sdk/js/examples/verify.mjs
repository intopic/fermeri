#!/usr/bin/env node
// Shembull: verifiko një llogari me zilja-verify.
//
//   node examples/verify.mjs <pubkey> --zilja http://127.0.0.1:7799 [--zilja URL2] \
//        [--rpc http://127.0.0.1:8899] [--quorum] [--min-trust single|agreement|quorum] [--slot N]
//
// Kodi i daljes: 0 = ok, 1 = jo ok, 2 = gabim.
import { ZiljaClient } from "../src/index.js";

const args = process.argv.slice(2);
const endpoints = [];
let pubkey, solanaRpc, quorum = false, minTrust, slot;
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  if (a === "--zilja") endpoints.push(args[++i]);
  else if (a === "--rpc") solanaRpc = args[++i];
  else if (a === "--quorum") quorum = true;
  else if (a === "--min-trust") minTrust = args[++i];
  else if (a === "--slot") slot = Number(args[++i]);
  else pubkey = a;
}
if (!pubkey || endpoints.length === 0) {
  console.error("përdorimi: node examples/verify.mjs <pubkey> --zilja URL [--zilja URL2] [--rpc URL] [--quorum] [--min-trust X] [--slot N]");
  process.exit(2);
}

try {
  const z = new ZiljaClient({ endpoints, solanaRpc, quorum, minTrust });
  const r = await z.verifyAccount(pubkey, { slot });
  const icon = r.ok ? "✅" : "❌";
  const what =
    r.verdict === "exists"
      ? `ekziston: ${r.account.lamports} lamports (${Number(r.account.lamports) / 1e9} SOL)`
      : r.verdict === "does_not_exist"
        ? "nuk ekziston"
        : "dëftesë e pavlefshme";
  console.log(`${icon} ${what}`);
  console.log(`   slot ${r.slot}, besimi: ${r.trust} (${r.agreeing}/${r.endpoints} zile dakord)`);
  if (r.quorum) console.log(`   kuorumi: ${r.quorum.reached ? "po" : "jo"}, ${r.quorum.stakePercent}% e stake-ut`);
  if (r.data) console.log(`   të dhënat nga RPC-ja: ${r.data.matches ? "përputhen" : "NUK përputhen"} (slot ${r.data.rpcSlot})`);
  for (const w of r.warnings) console.log(`   ⚠️  ${w}`);
  process.exit(r.ok ? 0 : 1);
} catch (e) {
  console.error("gabim:", e.message);
  process.exit(2);
}
