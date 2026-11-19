//! `zilja-quorum`: a e kanë firmosur pleqtë ziles?
//!
//! ```text
//! zilja-quorum <SOLANA_RPC_URL> <ZILJA_URL> [ZILJA_URL ...] [--genesis <hash base58>]
//! ```
//!
//! Pa `--genesis`, hash-i i genesis-it merret nga `getGenesisHash` i RPC-së.
//!
//! 1. Merr tabelën e stake-ut nga `getVoteAccounts` (sot nga RPC; në versionin e
//!    protokollit, nga vetë zinxhiri).
//! 2. Pyet çdo zile për root-in e firmosur në të njëjtin slot.
//! 3. Kontrollon firmat dhe që validatorët që firmosën kanë > 2/3 e stake-ut.
//!
//! Kodi i daljes: 0 = kuorumi u arrit, 1 = nuk u arrit, 2 = gabim.

use serde_json::json;
use std::process::exit;
use zilja_cli::{genesis, quorum_by_root, rpc, stake_table};

fn run() -> Result<bool, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let genesis_arg = args.iter().position(|a| a == "--genesis").map(|i| {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        args.drain(i..(i + 2).min(args.len()));
        v
    });
    if args.len() < 2 {
        return Err("përdorimi: zilja-quorum <SOLANA_RPC_URL> <ZILJA_URL> [ZILJA_URL ...] [--genesis <hash>]".into());
    }
    let (sol, ziljas) = (&args[0], &args[1..]);

    let table = stake_table(sol)?;
    let g = genesis(sol, genesis_arg.as_deref())?;
    println!("genesis: {}", bs58::encode(g).into_string());
    println!("stake total: {} lamports, {} validatorë", table.total(), table.entries.len());

    // Sloti i përbashkët: më i vogli nga slotet e fundit të firmosura.
    let mut latest = Vec::new();
    for z in ziljas {
        match rpc(z, "getZiljaAttestation", json!([])).map(|a| a["slot"].as_u64()) {
            Ok(Some(s)) => latest.push(s),
            Ok(None) => println!("  {z}: përgjigje pa slot, s'numërohet"),
            Err(e) => println!("  {z}: {e}"),
        }
    }
    let slot = *latest.iter().min().ok_or("asnjë zile s'dha firmë")?;
    println!("sloti i krahasimit: {slot}");

    // P-11: çdo root kontrollohet (64 hex) para përdorimit; firmat grupohen sipas root-it dhe
    // kuorumi vlerësohet për secilin veç. Dy root-e me kuorum = ndarje, dështim.
    let q = quorum_by_root(ziljas, slot, &g, &table);
    for (z, e) in &q.skipped {
        println!("  {z}: s'numërohet ({e})");
    }
    if q.groups.len() > 1 {
        println!("⚠️  zilet japin {} root-e të ndryshme për slotin {slot}", q.groups.len());
    }
    let mut reached = Vec::new();
    for (root, rep) in &q.groups {
        let pct = if rep.total_stake > 0 { rep.attested_stake as f64 * 100.0 / rep.total_stake as f64 } else { 0.0 };
        println!("root {root}");
        println!("  firma të vlefshme: {}, të refuzuara: {}, stake që firmosi: {pct:.2}%", rep.valid_signatures, rep.rejected);
        if rep.reached {
            reached.push(root);
        }
    }
    println!("⚠️  tabela e stake-ut është e epokës aktuale dhe u mor nga {sol}: i besohet atij");
    match reached.len() {
        1 => {
            println!("✅ kuorumi > 2/3 u arrit për root {}", reached[0]);
            Ok(true)
        }
        0 => {
            println!("❌ kuorumi nuk u arrit");
            Ok(false)
        }
        n => {
            println!("❌ kuorumi u arrit për {n} root-e të ndryshme: tabela e stake-ut ose firmat janë të dyshimta");
            Ok(false)
        }
    }
}

fn main() {
    match run() {
        Ok(true) => exit(0),
        Ok(false) => exit(1),
        Err(e) => {
            eprintln!("gabim: {e}");
            exit(2)
        }
    }
}
