// Banori në Node, me të njëjtin kod si faqja demo.
//   node web/check.mjs <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>] [--also <ZILJA_URL>] [--min-trust single|agreement|quorum]
import { checkAccount } from "./check-core.js";

const USAGE = "përdorimi: node web/check.mjs <ZILJA_URL> <pubkey> [SOLANA_RPC_URL] [--root <hex>] [--also <ZILJA_URL>] [--min-trust single|agreement|quorum]";
const usage = (why) => {
  console.error(`${why}\n${USAGE}`);
  process.exit(2);
};

// G23: çdo flamur i panjohur, flamur pa vlerë, flamur i dyfishtë ose argument i tepërt refuzohet,
// që një gabim shtypi (p.sh. --min-turst) të mos e dobësojë kontrollin pa zë.
const FLAGS = new Set(["--root", "--also", "--min-trust"]);
const opts = {};
const pos = [];
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a.startsWith("--")) {
    if (!FLAGS.has(a)) usage(`flamur i panjohur: ${a}`);
    if (a in opts) usage(`${a} u dha dy herë`);
    const v = argv[++i];
    if (v === undefined || v.startsWith("--")) usage(`${a} kërkon një vlerë`);
    opts[a] = v;
  } else pos.push(a);
}
if (pos.length < 2 || pos.length > 3) usage(pos.length < 2 ? "mungon ZILJA_URL ose pubkey" : `argument i tepërt: ${pos.slice(3).join(" ")}`);
const minTrust = opts["--min-trust"];
if (minTrust !== undefined && !["single", "agreement", "quorum"].includes(minTrust)) usage(`--min-trust i pavlefshëm: ${minTrust}`);
const [ziljaUrl, pubkey, solanaRpc] = pos;
try {
  const { ok, steps } = await checkAccount({ ziljaUrl, pubkey, solanaRpc, rootOverride: opts["--root"], secondZilja: opts["--also"], minTrust });
  for (const s of steps) console.log(`${s.ok === true ? "✅" : s.ok === false ? "❌" : "•"} ${s.text}`);
  process.exit(ok ? 0 : 1);
} catch (e) {
  console.error("gabim:", e.message);
  process.exit(2);
}
