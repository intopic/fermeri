// R5-4: VerifiableConnection (web3.js) dhe createVerifiedRpc (kit) kthejnë vetëm llogari të
// verifikuara: dëftesa + root-i (2 zile dakord) + bajtet e RPC-së = dataHash i vulosur.
import { readFileSync } from "node:fs";
import { VerifiableConnection, createVerifiedRpc, VerificationError, parseTokenAccount, parseMint, bytesToBase64, ZiljaClient } from "../src/index.js";

const fx = JSON.parse(readFileSync(new URL("./vectors.json", import.meta.url), "utf8")).connection;
let fail = 0;
const expect = (name, cond, extra = "") => {
  if (!cond) {
    fail++;
    console.error(`❌ ${name} ${extra}`);
  } else console.log(`✅ ${name}`);
};

/** Dy zile (dëftesa të vërteta nga fixture) + një RPC Solana me bajtet; `tamper` e prish RPC-në. */
const calls = [];
function world({ tamper = null, ziljaRoot = fx.root, noContextSecond = false } = {}) {
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
    else if (method === "getMultipleAccounts") {
      calls.push(params[0].length);
      const second = calls.length === 2;
      result = params[0].length > 100 ? undefined : { ...(noContextSecond && second ? {} : { context: { slot: 100 } }), value: params[0].map(info) };
    }
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
  // G24: konfigurimi respektohet: opsionet e panjohura dhe minContextSlot mbi slotin hedhin gabim.
  const rejects = async (p) => { try { await p; return false; } catch { return true; } };
  const ok = await rpc.getAccountInfo(fx.ata, { commitment: "confirmed", encoding: "base64", minContextSlot: a.context.slot }).send();
  expect("G24: konfigurim i pranuar (commitment, base64, minContextSlot = sloti)", ok.value.lamports === 2_039_280n);
  expect("G24: dataSlice → gabim", await rejects(rpc.getAccountInfo(fx.ata, { dataSlice: { offset: 0, length: 8 } }).send()));
  expect("G24: encoding jsonParsed → gabim", await rejects(rpc.getMultipleAccounts([fx.ata], { encoding: "jsonParsed" }).send()));
  expect("G24: minContextSlot mbi slotin → gabim", await rejects(rpc.getBalance(fx.ata, { minContextSlot: a.context.slot + 1n }).send()));
  expect("G24: getBalance me encoding → gabim", await rejects(rpc.getBalance(fx.ata, { encoding: "base64" }).send()));
}

// R5 (rishikimi): copa ≤ 100 te getMultipleAccounts; lista bosh; base64 pa Buffer dhe për të dhëna të mëdha.
{
  const conn = new VerifiableConnection({ zilja: world() });
  calls.length = 0;
  const list = Array.from({ length: 150 }, (_, i) => (i % 2 ? fx.ata : fx.wallet));
  const many = await conn.getMultipleAccountsInfo(list);
  expect("150 llogari → getMultipleAccounts në copa ≤ 100", many.length === 150 && many.every(Boolean) && calls.length === 2 && calls.every((n) => n <= 100), JSON.stringify(calls));
  expect("getMultipleAccountsInfo([]) → []", Array.isArray(await conn.getMultipleAccountsInfo([])) && (await conn.getMultipleAccountsInfo([])).length === 0);
  const kitEmpty = await createVerifiedRpc(world()).getMultipleAccounts([]).send();
  expect("kit getMultipleAccounts([]) → []", kitEmpty.value.length === 0);
  const big = new Uint8Array(3_000_000).map((_, i) => i * 7);
  const withBuffer = bytesToBase64(big);
  const saved = globalThis.Buffer;
  globalThis.Buffer = undefined;
  let noBuffer;
  try { noBuffer = bytesToBase64(big); } finally { globalThis.Buffer = saved; }
  expect("bytesToBase64 pa Buffer, 3 MB, pa spread", noBuffer === withBuffer);
}

// G02: kit getMultipleAccounts mbi 256 adresa s'pohon një slot të përbashkët që s'e ka.
{
  let threw = false;
  try { await createVerifiedRpc(world()).getMultipleAccounts(Array.from({ length: 257 }, () => fx.wallet)).send(); } catch (e) { threw = /256/.test(e.message); }
  expect("G02: kit getMultipleAccounts(257) → gabim i qartë", threw);
}

// G01: kit getBalance me onUnverified "null": dështimi → null, mungesa e provuar → 0n.
{
  const bad = await createVerifiedRpc(world({ ziljaRoot: "ab".repeat(32) }), { onUnverified: "null" }).getBalance(fx.wallet).send();
  expect("G01: kit getBalance i paverifikuar → null (jo 0n)", bad.value === null, String(bad.value));
  const gone = await createVerifiedRpc(world(), { onUnverified: "null" }).getBalance(fx.missing).send();
  expect("G01: kit getBalance i llogarisë që s'ekziston → 0n", gone.value === 0n, String(gone.value));
}

// L5 / G30: një copë e getMultipleAccounts pa `context.slot` s'e bën rpcSlot NaN, dhe s'merr slotin e
// një cope tjetër: llogaritë e saj kanë rpcSlot null, ato të copës së parë 100.
{
  calls.length = 0;
  const list = Array.from({ length: 150 }, (_, i) => (i % 2 ? fx.ata : fx.wallet));
  const r = await new ZiljaClient({ ...world({ noContextSecond: true }), minTrust: "agreement" }).verifyAccounts(list);
  const slots = r.map((x) => x.data && x.data.rpcSlot);
  expect("G30: rpcSlot sipas copës (100 për të parën, null për copën pa context)", slots.slice(0, 100).every((s) => s === 100) && slots.slice(100).every((s) => s === null), JSON.stringify([slots[0], slots[99], slots[100], slots[149]]));
}

// G12: vetëm llogari tokeni / mint të vërteta lexohen si të tilla.
{
  const acct = (len, kind) => {
    const d = new Uint8Array(len);
    d.fill(7, 0, 64);
    d[64] = 5; // amount = 5
    d[108] = 1; // initialized
    if (len > 165) d[165] = kind;
    return d;
  };
  const mint = (len, kind) => {
    const d = new Uint8Array(len);
    d[36] = 9; // supply = 9
    d[44] = 6; // decimals
    d[45] = 1;
    if (len > 165) d[165] = kind;
    return d;
  };
  const throws = (f) => { try { f(); return false; } catch { return true; } };
  expect("G12: llogari 165 bajte", parseTokenAccount(acct(165)).amount === 5n);
  expect("G12: Token-2022 me zgjerime (AccountType 2)", parseTokenAccount(acct(200, 2)).amount === 5n);
  expect("G12: 200 bajte me AccountType 1 (mint) ≠ llogari", throws(() => parseTokenAccount(acct(200, 1))));
  expect("G12: Multisig (355 bajte) ≠ llogari", throws(() => parseTokenAccount(acct(355, 2))));
  expect("G12: 170 bajte pa AccountType ≠ llogari", throws(() => parseTokenAccount(acct(170, 0))));
  const badState = acct(165); badState[108] = 3;
  expect("G12: gjendje e panjohur → gabim", throws(() => parseTokenAccount(badState)));
  const badTag = acct(165); badTag[72] = 2;
  expect("G12: etiketë COption 2 → gabim", throws(() => parseTokenAccount(badTag)));
  expect("G12: mint 82 bajte", parseMint(mint(82)).decimals === 6 && parseMint(mint(82)).supply === 9n);
  expect("G12: mint Token-2022 (AccountType 1)", parseMint(mint(250, 1)).decimals === 6);
  expect("G12: llogari 165 bajte ≠ mint", throws(() => parseMint(acct(165))));
  expect("G12: 100 bajte ≠ mint", throws(() => parseMint(mint(100))));
  const badInit = mint(82); badInit[45] = 2;
  expect("G12: is_initialized 2 → gabim", throws(() => parseMint(badInit)));
}

if (fail) {
  console.error(`❌ ${fail} teste të lidhjes dështuan`);
  process.exit(1);
}
console.log("✅ lidhja e verifikuar: të gjitha kalojnë");
