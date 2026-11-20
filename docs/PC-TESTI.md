# Testet në PC: plani përfundimtar

Plani i vendosur: **Testnet-i i plotë** (10 raste) dhe **Mainnet-i i shkurtuar** (3 rastet kryesore),
rreth 1 ditë gjithsej. PC-ja s'ka nevojë të ketë kodin: e merr skripti.

## A. Përgatitja (një herë, rreth 15 minuta)

Këto 3 hapa i bën personi te PC-ja, sepse kërkojnë të drejta administratori ose fjalëkalimin.

**1. Linux.** Në Windows, në PowerShell si administrator:

```powershell
wsl --install -d Ubuntu-24.04
```

Rinise PC-në. Pastaj krijo `C:\Users\<emri>\.wslconfig` me këtë përmbajtje (për një PC me 64 GB RAM;
lër rreth 8 GB për Windows-in), dhe në PowerShell nis `wsl --shutdown`:

```
[wsl2]
memory=56GB
swap=0
```

`swap=0`: pa swap, një matje që s'nxë në RAM dështon qartë, në vend që numrat të prishen në heshtje
(faza 0 jep KUJDES kur ka swap).

Për Mainnet-in, disku virtual i WSL2 ka si parazgjedhje maksimumin 1 TB, kurse Mainnet-i kërkon
≥ 700 GB të lirë brenda Ubuntu-s. Kontrolloje me `df -h ~`; nëse s'mjafton, zmadhoje në PowerShell
(versioni i ri i WSL-së):

```powershell
wsl --shutdown
wsl --manage Ubuntu-24.04 --resize 2TB
```

Edhe disku i Windows-it ku ndodhet disku virtual (zakonisht C:) duhet ta ketë këtë vend. Faza 0 i
kontrollon të dyja.

Në një PC me Linux, ky hap s'duhet.

**2. Veglat e sistemit** (në Ubuntu, një herë):

```bash
sudo apt update && sudo apt install -y build-essential pkg-config git curl python3-matplotlib bzip2 xz-utils
mkdir -p ~/zilja
```

**3. Lidhja me Claude.** Hap aplikacionin desktop të Claude në atë PC dhe lidh dosjen `~/zilja`.
Në Windows ajo duket si `\\wsl.localhost\Ubuntu-24.04\home\<emri>\zilja`. Pastaj shkruaj një mesazh
në bisedë.

Gjatë testeve PC-ja duhet të jetë e ndezur, në prizë dhe pa gjumë (Windows: Cilësimet → Energjia →
Gjumi: Kurrë). Në WSL2, mbaje dritaren e Ubuntu-s hapur.

## B. Nisja (Claude ose ti)

```bash
cd ~/zilja && git clone https://github.com/intopic/fermeri && cd fermeri
scripts/pc-setup.sh                                   # Rust, Agave 4.3.2, Node.js 22, binarët (pa sudo)
mkdir -p ~/zilja/results && nohup scripts/pc-run-all.sh > ~/zilja/results/run.out 2>&1 &   # të gjitha fazat
```

`pc-setup.sh` instalon çdo gjë te `~/zilja/tools` dhe `~/.cargo`, pa sudo. Nëse mungon ndonjë vegël
e sistemit, ndalet dhe tregon komandën e saktë me `sudo`.

## C. Fazat

| Faza | Çfarë | Koha |
|---|---|---|
| 0 check | a jep PC-ja matje të sakta: disku, RAM-i, WSL2, `--cold` | ~1 min |
| 1 tests | `cargo test --release --workspace` | 10–20 min |
| 2 e2e | plugin-i brenda Agave-s (solana-test-validator), me RAM dhe me disk | 20–40 min |
| 3 testnet | matrix-i i plotë: size 1M/10M/50M/i plotë, cold profile/slot, cache 16/24, seed 2/3; çdo rast me slote, 20 000 dëftesa dhe klientë 1/8/32/128 + një IP | ~6–10 orë |
| 4 mainnet | i shkurtuar: size i plotë (1 024 slote), cold slot dhe cache 24 (512 slote); profilet 1 000 – 60 000 ndryshime/slot (pa 100 000) | ~10–16 orë |
| 5 publish | kopjon rezultatet te `bench-results/` për PR | ~1 min |

- **Gjendja:** `~/zilja/results/STATUS.md` (faza, ora, OK/PJESËRISHT/DËSHTOI). Log-u i çdo faze:
  `~/zilja/results/phaseN-*.log`.
- **Ndërprerje:** e njëjta komandë vazhdon nga faza dhe nga rasti ku mbeti.
- **Vetëm disa faza:** `PHASES="3 4" scripts/pc-run-all.sh`.
- **Hapësira:** para Mainnet-it, skripti kontrollon që ka ≥ 700 GB të lirë. Nëse s'ka, fshin
  snapshot-in e Testnet-it (rezultatet mbeten) dhe, po s'mjaftoi, ndalet me mesazh.

## D. Çfarë del

- `~/zilja/results/testnet/RESULTS.md` dhe `~/zilja/results/mainnet/RESULTS.md`: tabelat e ndërtimit,
  të sloteve, të dëftesave dhe të RPC-së, plus si lexohet secila kolonë.
- Grafikët e çdo rasti: `ms_slot.png`, `phases.png`, `ram.png`, `cpu_io.png`, `rpc.png`.
- `bench-results/<data>-<makina>-<rrjeti>/`: skedarët e vegjël për PR-në në GitHub.

Si lexohen rezultatet: docs/OPERIMI.md, "Seria e plotë në PC".

## E. Kërkesat e PC-së

| | Testnet | Mainnet |
|---|---|---|
| RAM | 32 GB | 64 GB |
| Disk i lirë (NVMe) | 300 GB | 1 TB |

Dosja duhet të jetë brenda Linux-it (`~/`), jo te `/mnt/c`: `check` ndalet në këtë rast.

Faza 0 kontrollon edhe para se të nisë gjithçka:

- disku i lirë ≥ `MAINNET_MIN_FREE_GB` (700 GB) kur faza 4 është në plan, dhe vendi në C: në WSL2;
- që URL-të e snapshot-eve (`TESTNET_URL`, `MAINNET_URL`) ridrejtojnë vërtet te një
  `snapshot-*.tar.zst`. HTTP 429 (kufi shkarkimesh i endpoint-eve publike) jep vetëm KUJDES: shkarkimi
  provon sërish vetë.

Shkarkimi i snapshot-it mund të ndërpritet dhe të rinisë pa rrezik: vazhdon vetëm mbi të njëjtin
snapshot (URL-ja ruhet te `snapshot.url`), dhe nis nga e para nëse rrjeti ka nxjerrë një të ri.

Nëse në fazën 3 ose 4 dështojnë vetëm disa raste, faza shënohet PJESËRISHT te `STATUS.md` dhe nata
vazhdon me fazën tjetër; skripti del në fund me kod 4. Një nisje e re provon vetëm rastet që dështuan.
