// R5-4: VerifiableConnection (web3.js) dhe createVerifiedRpc (kit) kthejnë vetëm llogari të
// verifikuara: dëftesa + root-i (2 zile dakord) + bajtet e RPC-së = dataHash i vulosur.
import { readFileSync } from "node:fs";
import { VerifiableConnection, createVerifiedRpc, VerificationError, parseTokenAccount } from "../src/index.js";

const fx = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")).connection;
let fail = 0;
const expect = (name, cond, extra = "") => {
  if (!cond) {
    fail++;
    console.error(`❌ ${name} ${extra}`);
  } else console.log(`✅ ${name}`);
};

/** Dy zile (dëftesa të vërteta nga fixture) + një RPC Solana me bajtet; `tamper` e prish RPC-në. */
function world({ tamper = null, ziljaRoot = fx.root } = {}) {
  const ziles = ["http://z1", "http://z2"];
  const data = new Map(fx.data.map((d) => [d.pubkey, d]));
  const info = (pk) => {
    const d = data.get(pk);
    if (!d) return null;
    const base64 = tamper === pk ? Buffer.from([...Buffer.from(d.base64, "base64"), 0]).toString("base64") : d.base64;
    return { lamports: d.lamports, owner: d.owner, executable: d.executable, data: [base64, "base64"], rentEpoch: 0 };
  };
  const fetch = async (url, req) => {
    const { method, params } = JSON.parse(req.body);
    let result;
    if (ziles.includes(url)) {
      if (method === "getAccountProof") {
        const s = fx.single.find((x) => x.pubkey === params[0]);
        result = { slot: 100, root: ziljaRoot, complete: true, pubkey: s.pubkey, account: s.account, proof: s.proof };
      } else if (method === "getMultipleAccountProofs") {
        const m = fx.multi.find((x) => JSON.stringify(x.pubkeys) === JSON.stringify(params[0]));
        result = m ? { slot: 100, root: ziljaRoot, complete: true, accounts: m.accounts, proof: m.proof } : undefined;
      } else if (method === "getZiljaRoot") result = { slot: 100, root: fx.root, complete: true };
    } else if (method === "getAccountInfo") result = { context: { slot: 100 }, value: info(params[0]) };
    else if (method === "getMultipleAccounts") result = { context: { slot: 100 }, value: params[0].map(info) };
    else if (method === "getSlot") result = 100;
    if (result === undefined) return { json: async () => ({ error: { message: `s'ka ${method} ${JSON.stringify(params)}` } }) };
    return { json: async () => ({ result }) };
  };
  return { endpoints: ziles, solanaRpc: "http://sol", fetch };
}

class FakePublicKey {
  constructor(s) { this.s = s; }
  toBase58() { return this.s; }
}

// web3.js
{
  const conn = new VerifiableConnection({ zilja: world(), PublicKey: FakePublicKey });
  const w = await conn.getAccountInfo(new FakePublicKey(fx.wallet));
  expect("getAccountInfo: llogari sistemi e verifikuar", w && w.lamports === 2_500_000_000 && w.owner instanceof FakePublicKey && w.data.length === 0, JSON.stringify(w));
  expect("getAccountInfo: llogari që s'ekziston → null", (await conn.getAccountInfo(fx.missing)) === null);
  expect("getBalance", (await conn.getBalance(fx.wallet)) === 2_500_000_000);
  const many = await conn.getMultipleAccountsInfo([fx.wallet, fx.ata, fx.missing]);
  expect("getMultipleAccountsInfo (një multi-proof)", many.length === 3 && many[0].lamports === 2_500_000_000 && many[1].data.length === 165 && many[2] === null, JSON.stringify(many.map((x) => x && x.lamports)));
  const ta = await conn.getTokenAccount(fx.ata);
  expect("getTokenAccount: mint, owner, amount nga bajtet e verifikuara", ta.mint === fx.mint && ta.owner === fx.wallet && ta.amount === 1_234_567_890n && ta.state === "initialized", JSON.stringify(ta, (_, v) => (typeof v === "bigint" ? v.toString() : v)));
  const bal = await conn.getTokenAccountBalance(fx.ata);
  expect("getTokenAccountBalance: decimals nga mint-i i verifikuar", bal.value.amount === "1234567890" && bal.value.decimals === 6 && bal.value.uiAmountString === "1234.56789", JSON.stringify(bal));
  let notToken = false;
  try { await conn.getTokenAccount(fx.wallet); } catch { notToken = true; }
  expect("getTokenAccount mbi llogari jo-token → gabim", notToken);
}

// RPC-ja gënjen bajtet → s'kthehet asgjë (hidhet VerificationError).
{
  const conn = new VerifiableConnection({ zilja: world({ tamper: fx.ata }) });
  let err = null;
  try { await conn.getAccountInfo(fx.ata); } catch (e) { err = e; }
  expect("bajte të ndryshuara nga RPC-ja → VerificationError", err instanceof VerificationError && err.results?.[0]?.checks?.data === "mismatch", String(err));
  const soft = new VerifiableConnection({ zilja: world({ tamper: fx.ata }), onUnverified: "null" });
  expect("onUnverified: null → null, jo të dhëna të paverifikuara", (await soft.getAccountInfo(fx.ata)) === null);
}

// Zilja gënjen root-in (zilja e dytë jep tjetër) → s'ka marrëveshje → gabim.
{
  const conn = new VerifiableConnection({ zilja: world({ ziljaRoot: "ab".repeat(32) }) });
  let threw = false;
  try { await conn.getAccountInfo(fx.wallet); } catch (e) { threw = e instanceof VerificationError; }
  expect("root i rremë nga zilja e parë → VerificationError", threw);
}

// Pa solanaRpc s'ka si të verifikohen bajtet → konstruktori refuzon.
{
  let threw = false;
  try { new VerifiableConnection({ zilja: { endpoints: ["http://z1", "http://z2"] } }); } catch { threw = true; }
  expect("VerifiableConnection pa solanaRpc → gabim", threw);
}

// kit
{
  const rpc = createVerifiedRpc(world());
  const a = await rpc.getAccountInfo(fx.ata).send();
  expect("kit getAccountInfo: lamports bigint, data base64", a.value.lamports === 2_039_280n && a.value.data[1] === "base64" && parseTokenAccount(new Uint8Array(Buffer.from(a.value.data[0], "base64"))).amount === 1_234_567_890n);
  const m = await rpc.getMultipleAccounts([fx.ata, fx.mint]).send();
  expect("kit getMultipleAccounts", m.value.length === 2 && m.value[1].space === 82n);
  const b = await rpc.getBalance(fx.missing).send();
  expect("kit getBalance i llogarisë që s'ekziston → 0n", b.value === 0n);
}

if (fail) {
  console.error(`❌ ${fail} teste të lidhjes dështuan`);
  process.exit(1);
}
console.log("✅ lidhja e verifikuar: të gjitha kalojnë");
