#!/usr/bin/env python3
"""Tabela krahasuese e rasteve të `scripts/bench-matrix.sh`.

    python3 scripts/bench-table.py <dosja-e-matrix-it> > RESULTS.md

Lexon <dosja>/*/summary.json dhe shkruan Markdown: makina, një rresht për rast (ndërtimi), një
rresht për rast × profil (slotet), dëftesat dhe paralajmërimet. Kolonat shpjegohen në fund.
"""
import json
import os
import sys


def f(v, d=1):
    if v is None:
        return "–"
    if isinstance(v, (int,)) and not isinstance(v, bool):
        return f"{v:,}"
    return f"{v:,.{d}f}"


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    root = sys.argv[1]
    cases = []
    for n in sorted(os.listdir(root)):
        p = os.path.join(root, n, "summary.json")
        if os.path.isfile(p):
            cases.append((n, json.load(open(p))))
    failed = sorted(n for n in os.listdir(root)
                    if os.path.isfile(os.path.join(root, n, "bench.log")) and not os.path.isfile(os.path.join(root, n, "summary.json")))
    # G15: summary.json me rpc_error = ndërtimi mbaroi, por faza RPC dështoi: rasti s'është i mbaruar.
    rpc_failed = [(n, s["rpc_error"]) for n, s in cases if s.get("rpc_error")]
    out = []
    w = out.append
    w(f"# Rezultatet e bench-it: {os.path.basename(os.path.abspath(root))}\n")
    m = os.path.join(root, "machine.txt")
    if os.path.isfile(m):
        w("## Makina\n\n```text\n" + open(m).read().strip() + "\n```\n")
    if not cases:
        w("S'ka asnjë rast të mbaruar.\n")
    else:
        w("## Ndërtimi i pemës\n")
        w("| rasti | cache | mostra | llogari | të dhëna GB | leximi s | ndërtimi s | RAM max MB | pages.dat GB | B/llogari | pages/RAM |")
        w("|---|---|---|---|---|---|---|---|---|---|---|")
        for n, s in cases:
            p = s.get("params", {})
            i, b, r = s.get("ingest", {}), s.get("build", {}), s.get("ram", {})
            w(f"| {n} | {p.get('cold', 'none')} / d{s.get('cache_depth')} | {f(p.get('sample'))} | {f(s.get('accounts'))} | {f(i.get('data_gb'))} | "
              f"{f(i.get('stage_seconds'), 0)} | {f(b.get('finish_seconds'), 0)} | {f(b.get('hwm_mb'), 0)} | {f(b.get('pages_gb'), 2)} | "
              f"{f(b.get('disk_bytes_per_account'), 0)} | {f(r.get('pages_gb_vs_mem_total'), 2)} |")
        w("\n## Slotet (ms për slot)\n")
        w("| rasti | ndryshime/slot | p50 | p99 | max | p99 pa ckpt | ckpt n | ckpt max | leximi p50 | leximi p99 | fsync p50 | fsync p99 | faqe/slot | MB/slot nga disku | cache % | RAM max MB |")
        w("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for n, s in cases:
            hwm = s.get("after_slots", {}).get("hwm_mb")
            for k, v in sorted(s.get("slots", {}).items(), key=lambda kv: int(kv[0])):
                t, ck, nk = v["total_ms"], v.get("checkpoint_ms", {}), v.get("total_ms_without_checkpoints", {})
                rd, fs = v.get("read_ms", {}), v.get("fsync_ms", {})
                w(f"| {n} | {int(k):,} | {f(t['p50'])} | {f(t['p99'])} | {f(t['max'])} | {f(nk.get('p99'))} | {ck.get('n', 0)} | {f(ck.get('max'))} | "
                  f"{f(rd.get('p50'))} | {f(rd.get('p99'))} | {f(fs.get('p50'))} | {f(fs.get('p99'))} | "
                  f"{f(v.get('disk_reads_per_slot', {}).get('mean'), 0)} | {f(v.get('device_read_mb_per_slot', {}).get('mean'), 2)} | "
                  f"{f(v.get('os_cache_hit_pct'), 0)} | {f(hwm, 0)} |")
        w("\n## Dëftesat\n")
        w("| rasti | n | të pavlefshme | aktuale p50 ms | aktuale p99 ms | historike p50 ms | historike p99 ms | madhësia p50 B |")
        w("|---|---|---|---|---|---|---|---|")
        for n, s in cases:
            p = s.get("proofs")
            if not p:
                continue
            c, h = p.get("current_ms", {}), p.get("historic_ms", {})
            w(f"| {n} | {f(p['n'])} | {p['invalid']} | {f(c.get('p50'), 2)} | {f(c.get('p99'), 2)} | {f(h.get('p50'), 2)} | {f(h.get('p99'), 2)} | {f(p['bytes']['p50'], 0)} |")
        if any(s.get("rpc") for _, s in cases):
            w("\n## RPC: dëftesa nga shumë klientë njëkohësisht (me slote në sfond)\n")
            w("| rasti | klientë | kërkesa/s | dëftesa/s | p50 ms | p99 ms | max ms | multi p99 ms | gabime % | busy | refuzuara | porta | slote vonë | slot p99 ms | kyçi p99 ms | CPU % | verifikuar | pavlefshme |")
            w("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
            for n, s in cases:
                r = s.get("rpc")
                if not r:
                    continue
                for l in r["levels"]:
                    lat, wr = l["latency_ms"], l["writer"]
                    w(f"| {n} | {l['clients']}{' (1 IP)' if l['one_ip'] else ''} | {f(l['requests_per_second'], 0)} | {f(l['proofs_per_second'], 0)} | "
                      f"{f(lat.get('p50'), 2)} | {f(lat.get('p99'), 2)} | {f(lat.get('max'), 1)} | {f(l['by_kind']['multi'].get('p99'), 2)} | "
                      f"{f(l['error_pct'], 2)} | {l['busy_503']} | {l.get('refused', 0)} | {l['client_out_of_ports']} | {wr['late']}/{wr['slots']} | "
                      f"{f(wr['total_ms'].get('p99'), 1)} | {f(wr['write_hold_ms'].get('p99'), 2)} | {f(l['cpu_pct'], 0)} | {l['verified']} | {l['invalid']} |")
    if any(s.get("batch") for _, s in cases):
        w("\n## Grupimi i sloteve: një përditësim çdo K slote\n")
        w("| rasti | K | llogari unike/grup | grupi p50 ms | grupi p99 ms | grupi max ms | p99 pa ckpt | ms/slot |")
        w("|---|---|---|---|---|---|---|---|")
        for n, s in cases:
            for k, v in sorted((s.get("batch") or {}).items(), key=lambda kv: int(kv[0])):
                g, nk = v["group_ms"], v.get("group_ms_without_checkpoints", {})
                w(f"| {n} | {k} | {int(v['unique_updates_per_group']):,} | {f(g.get('p50'))} | {f(g.get('p99'))} | {f(g.get('max'))} | {f(nk.get('p99'))} | {f(v['ms_per_slot_mean'])} |")
    warns = [(n, x) for n, s in cases for x in s.get("warnings", [])]
    if warns or failed or rpc_failed:
        w("\n## Paralajmërime\n")
        for n, x in warns:
            w(f"- **{n}**: {x}")
        for n in failed:
            w(f"- **{n}**: DËSHTOI ose s'mbaroi (shiko {n}/bench.log)")
        for n, e in rpc_failed:
            w(f"- **{n}**: faza RPC DËSHTOI ({e}); numrat e ndërtimit dhe sloteve vlejnë, ata të RPC-së mungojnë")
    w("""
## Si lexohen kolonat

- **cache**: `none` = ngrohtë (pages.dat në cache-in e sistemit); `profile` = cache bosh në fillim të
  çdo profili, pastaj ngrohet; `slot` = cache bosh para çdo sloti (rasti më i keq). `dN` = cache_depth.
- **pages/RAM**: madhësia e pages.dat kundrejt RAM-it. Mbi 1 do të thotë që edhe rasti ngrohtë lexon nga disku.
- **p99 pa ckpt**: p99 pa slotet me checkpoint (`meta.ckpt`, çdo 256 slote). Checkpoint-et janë te **ckpt max**:
  me pak checkpoint-e për profil ato s'duken te p99.
- **leximi**: leximi i faqeve në fillim të slotit (nivel pas niveli, paralelisht). Kjo është pjesa që do të
  përfitonte nga io_uring; nëse leximi te rastet cold është i vogël kundrejt totalit, io_uring s'ka vlerë.
- **faqe/slot**: faqe të kërkuara nga disku (`pread`); **MB/slot nga disku**: bajte të lexuara realisht nga
  pajisja. **cache %** ≈ 1 − MB / (faqe × 4 KB): afër 100% = nga RAM-i, afër 0% = nga NVMe. Është i përafërt
  (disa faqe ndajnë të njëjtin bllok 4 KB; readahead-i i kernelit e ul).
- **RPC**: serveri i vërtetë i zilës (8 punëtorë, afati 5 s, pa keep-alive), me pemën e këtij rasti, ndërsa
  zbatohet një slot çdo 400 ms me 5 000 ndryshime. Përzierja: 60% dëftesa për llogari ekzistuese, 10% që
  s'ekzistojnë, 15% historike, 15% multi-proof me 10 llogari. **dëftesa/s** numëron çdo llogari të multi-proof-it.
  **slote vonë**: slote që zgjatën më shumë se ritmi (zilja mbetet prapa nën ngarkesë). **kyçi p99**: sa i
  mban sloti kërkesat në pritje (kyçi i shkrimit). **(1 IP)**: kufiri i prodhimit për një adresë (2 lidhje
  njëkohësisht): kështu sillet sot një klient i madh pas një IP-je të vetme. **porta** > 0: klientët e testit
  mbaruan portat e makinës (s'është faji i zilës). Klientët dhe serveri ndajnë CPU-në e së njëjtës makinë,
  ndaj kërkesa/s është kufiri i poshtëm i asaj që jep zilja me klientë nga rrjeti.
- **Grupimi** (`BATCH`): një përditësim i vetëm me U llogari unike çdo K slote. U vjen nga matja e Mainnet-it
  (llogaritë unike të K sloteve bashkë: 1:1502, 2:2340, 4:3827, 8:6134). **ms/slot** = koha mesatare e grupit / K;
  krahasoje me kohën e slotit të rrjetit (~200 ms).
- Ndryshimet janë sintetike dhe uniforme mbi `mostra` llogari (75% ndryshime, 15% të reja, 10% fshirje), jo
  trafik real: në Mainnet disa llogari janë shumë të nxehta, kështu që rastet cold janë kufiri i sipërm.
""")
    print("\n".join(out))


if __name__ == "__main__":
    main()
