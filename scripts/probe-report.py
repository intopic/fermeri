#!/usr/bin/env python3
"""Lexon skedarin e sondës (ZILJA_PROBE) dhe u përgjigjet pyetjeve të hapura të auditimit.

Për çdo nisje të validatorit (çdo `on_load`):
  - a erdhi `end_of_startup`, dhe me sa llogari snapshot-i;
  - sa banka kishin shkrime llogarish PA asnjë status banke (ripërsëritje gjatë nisjes, F-01);
  - nëse u thirr `on_unload` para ndalimit.
"""
import re
import sys

runs = []
cur = None
for line in open(sys.argv[1], encoding="utf-8"):
    parts = line.split(" ", 1)
    if len(parts) < 2:
        continue
    body = parts[1].strip()
    if body.startswith("on_load"):
        cur = {"end_of_startup": None, "acct_banks": {}, "status_banks": set(), "unload": False, "marks": []}
        runs.append(cur)
        continue
    if body.startswith("MARK"):
        (cur["marks"] if cur else []).append(body)
        continue
    if cur is None:
        continue
    if body.startswith("end_of_startup"):
        cur["end_of_startup"] = body.split("=", 1)[1]
    elif body.startswith("on_unload"):
        cur["unload"] = True
    elif body.startswith("account"):
        m = re.search(r"slot=(\d+) bank=(\d+)", body)
        cur["acct_banks"][int(m.group(2))] = int(m.group(1))
    elif body.startswith("bank_status"):
        m = re.search(r"bank=(\d+)", body)
        cur["status_banks"].add(int(m.group(1)))

for i, r in enumerate(runs, 1):
    no_status = sorted(s for b, s in r["acct_banks"].items() if b not in r["status_banks"])
    print(f"nisja {i}:")
    print(f"  end_of_startup: {'po, snapshot_accounts=' + r['end_of_startup'] if r['end_of_startup'] is not None else 'JO'}")
    print(f"  banka me shkrime llogarish: {len(r['acct_banks'])}, prej tyre PA asnjë status banke: {len(no_status)}"
          + (f" (slotet {no_status[0]}..{no_status[-1]})" if no_status else ""))
    print(f"  on_unload u thirr: {'po' if r['unload'] else 'JO'}")
