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
    warns = [(n, x) for n, s in cases for x in s.get("warnings", [])]
    if warns or failed:
        w("\n## Paralajmërime\n")
        for n, x in warns:
            w(f"- **{n}**: {x}")
        for n in failed:
            w(f"- **{n}**: DËSHTOI ose s'mbaroi (shiko {n}/bench.log)")
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
- Ndryshimet janë sintetike dhe uniforme mbi `mostra` llogari (75% ndryshime, 15% të reja, 10% fshirje), jo
  trafik real: në Mainnet disa llogari janë shumë të nxehta, kështu që rastet cold janë kufiri i sipërm.
""")
    print("\n".join(out))


if __name__ == "__main__":
    main()
