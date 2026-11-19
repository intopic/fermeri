// Banori në Node, me të njëjtin kod si faqja demo.
//   node web/check.mjs <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>] [--also <ZILJA_URL>] [--min-trust single|agreement|quorum]
import { checkAccount } from "./check-core.js";

const args = process.argv.slice(2);
const flag = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args.splice(i, 2)[1] : undefined;
};
const rootOverride = flag("--root");
const secondZilja = flag("--also");
const minTrust = flag("--min-trust");
const [ziljaUrl, pubkey, solanaRpc] = args;
if (!ziljaUrl || !pubkey) {
  console.error("përdorimi: node web/check.mjs <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>]");
  process.exit(2);
}
try {
  const { ok, steps } = await checkAccount({ ziljaUrl, pubkey, solanaRpc, rootOverride, secondZilja, minTrust });
  for (const s of steps) console.log(`${s.ok === true ? "✅" : s.ok === false ? "❌" : "•"} ${s.text}`);
  process.exit(ok ? 0 : 1);
} catch (e) {
  console.error("gabim:", e.message);
  process.exit(2);
}
