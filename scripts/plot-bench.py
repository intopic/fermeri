#!/usr/bin/env python3
"""Grafikët e `zilja-snapshot-bench`.

    python3 scripts/plot-bench.py <dosja-me-results.csv>   # ose rruga e results.csv

Lexon results.csv, timeline.csv dhe summary.json nga e njëjta dosje dhe shkruan:
  ms_slot.png  — ms/slot në kohë për çdo profil, me vijat p50/p99 dhe pikat e checkpoint-eve
  ram.png      — RSS (GB) nga fillimi i leximit të snapshot-it deri në fund, me fazat
  cpu_io.png   — CPU% dhe disk I/O MB/s për çdo profil
  phases.png   — ku shkon koha e slotit (leximi, pema, faqet, pwrite, fsync, log), p50 dhe p99 (R8)
Kërkon matplotlib (pip install matplotlib).
"""
import csv
import json
import os
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402


def load(path):
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def pct(v, q):
    v = sorted(v)
    return v[round((len(v) - 1) * q)] if v else 0.0


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    arg = sys.argv[1]
    d = os.path.dirname(arg) if arg.endswith(".csv") else arg
    results = load(arg if arg.endswith(".csv") else os.path.join(d, "results.csv"))
    timeline = load(os.path.join(d, "timeline.csv")) if os.path.exists(os.path.join(d, "timeline.csv")) else []
    summary = json.load(open(os.path.join(d, "summary.json"))) if os.path.exists(os.path.join(d, "summary.json")) else {}
    title = f"{summary.get('accounts', '?'):,} llogari, sloti {summary.get('snapshot_slot', '?')}" if summary else ""

    profiles = []
    for r in results:
        if r["profile"] not in profiles:
            profiles.append(r["profile"])

    # 1. ms/slot
    fig, axes = plt.subplots(len(profiles), 1, figsize=(11, 3.2 * len(profiles)), squeeze=False)
    for ax, p in zip(axes[:, 0], profiles):
        rows = [r for r in results if r["profile"] == p]
        xs = [int(r["slot"]) for r in rows]
        ys = [float(r["total_ms"]) for r in rows]
        ax.plot(xs, ys, lw=0.8, label="total ms/slot")
        ax.plot(xs, [float(r["prepare_ms"]) for r in rows], lw=0.6, alpha=0.6, label="prepare_slot")
        if "read_ms" in rows[0]:
            ax.plot(xs, [float(r["read_ms"]) for r in rows], lw=0.6, alpha=0.8, label="leximi i faqeve")
        for q, ls in ((0.5, "--"), (0.99, ":")):
            ax.axhline(pct(ys, q), ls=ls, color="k", lw=0.8, label=f"p{int(q * 100)} = {pct(ys, q):.1f} ms")
        ck = [(int(r["slot"]), float(r["total_ms"])) for r in rows if r["checkpoint"] == "1"]
        if ck:
            ax.scatter(*zip(*ck), color="red", s=18, zorder=3, label="checkpoint")
        ax.set_title(f"{int(p):,} ndryshime/slot")
        ax.set_ylabel("ms")
        ax.legend(fontsize=7, loc="upper right")
    axes[-1, 0].set_xlabel("slot")
    fig.suptitle(f"Zbatimi i slotit — {title}")
    fig.tight_layout()
    fig.savefig(os.path.join(d, "ms_slot.png"), dpi=130)

    # 2. RAM nga fillimi
    if timeline:
        fig, ax = plt.subplots(figsize=(11, 3.5))
        t = [float(r["t_s"]) / 60 for r in timeline]
        ax.plot(t, [float(r["rss_mb"]) / 1024 for r in timeline], label="RSS")
        ax.plot(t, [float(r["hwm_mb"]) / 1024 for r in timeline], ls="--", lw=0.8, label="maksimumi (HWM)")
        last = None
        for x, r in zip(t, timeline):
            if r["phase"] != last:
                ax.axvline(x, color="gray", lw=0.5)
                ax.text(x, ax.get_ylim()[1] * 0.02, " " + r["phase"], fontsize=7, rotation=90, va="bottom")
                last = r["phase"]
        ax.set_xlabel("minuta")
        ax.set_ylabel("GB")
        ax.set_title(f"RAM — {title}")
        ax.legend(fontsize=8)
        fig.tight_layout()
        fig.savefig(os.path.join(d, "ram.png"), dpi=130)

    # 3. CPU dhe I/O për profil
    fig, (a1, a2) = plt.subplots(1, 2, figsize=(11, 3.5))
    for p in profiles:
        rows = [r for r in results if r["profile"] == p]
        xs = range(len(rows))
        a1.plot(xs, [float(r["cpu_pct"]) for r in rows], lw=0.7, label=f"{int(p):,}/slot")
        a2.plot(xs, [float(r["write_mb_s"]) for r in rows], lw=0.7, label=f"shkrim {int(p):,}/slot")
        a2.plot(xs, [float(r["read_mb_s"]) for r in rows], lw=0.7, ls="--", label=f"lexim {int(p):,}/slot")
    a1.set_title("CPU % (100% = një bërthamë)")
    a2.set_title("Disk MB/s")
    for a in (a1, a2):
        a.set_xlabel("slot (brenda profilit)")
        a.legend(fontsize=7)
    fig.suptitle(title)
    fig.tight_layout()
    fig.savefig(os.path.join(d, "cpu_io.png"), dpi=130)
    names = ["ms_slot.png", "ram.png", "cpu_io.png"]

    # 4. Fazat e slotit (R8): p50 dhe p99 për çdo fazë dhe profil.
    cols = [("read_ms", "leximi"), ("tree_ms", "pema/hash"), ("build_ms", "faqet"), ("pwrite_ms", "pwrite"), ("fsync_ms", "fsync"), ("log_ms", "log+ckpt")]
    if results and all(c in results[0] for c, _ in cols):
        fig, axs = plt.subplots(1, 2, figsize=(11, 3.8), sharey=False)
        for ax, q in zip(axs, (0.5, 0.99)):
            bottom = [0.0] * len(profiles)
            for c, lab in cols:
                vals = [pct([float(r[c]) for r in results if r["profile"] == p], q) for p in profiles]
                ax.bar([f"{int(p):,}" for p in profiles], vals, bottom=bottom, label=lab)
                bottom = [b + v for b, v in zip(bottom, vals)]
            ax.set_title(f"p{int(q * 100)} e çdo faze (ms)")
            ax.set_xlabel("ndryshime/slot")
        axs[0].set_ylabel("ms")
        axs[1].legend(fontsize=7)
        fig.suptitle(f"Ku shkon koha e slotit — {title}")
        fig.tight_layout()
        fig.savefig(os.path.join(d, "phases.png"), dpi=130)
        names.append("phases.png")
    # 5. RPC (R9): kërkesa/s dhe p99 sipas numrit të klientëve.
    sp = os.path.join(d, "summary.json")
    rpc = json.load(open(sp)).get("rpc") if os.path.isfile(sp) else None
    if rpc:
        lv = [l for l in rpc["levels"] if not l["one_ip"]]
        if lv:
            fig, ax = plt.subplots(figsize=(7, 3.8))
            xs = [l["clients"] for l in lv]
            ax.plot(xs, [l["proofs_per_second"] for l in lv], "o-", label="dëftesa/s")
            ax.plot(xs, [l["requests_per_second"] for l in lv], "s--", label="kërkesa/s")
            ax.set_xscale("log", base=2)
            ax.set_xlabel("klientë njëkohësisht")
            ax.set_ylabel("në sekondë")
            ax2 = ax.twinx()
            ax2.plot(xs, [l["latency_ms"]["p99"] for l in lv], "^:", color="tab:red", label="p99 ms")
            ax2.set_ylabel("p99 ms", color="tab:red")
            ax.legend(loc="upper left", fontsize=8)
            ax.set_title(f"Dëftesa nga shumë klientë — {title}")
            fig.tight_layout()
            fig.savefig(os.path.join(d, "rpc.png"), dpi=130)
            names.append("rpc.png")
    print("u shkruan:", ", ".join(os.path.join(d, n) for n in names))


if __name__ == "__main__":
    main()
