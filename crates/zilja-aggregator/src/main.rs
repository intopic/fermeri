//! `zilja-aggregator`: shërbim i pabesuar që mbledh firmat e zileve (R5-3).
//!
//! ```text
//! zilja-aggregator --bind 0.0.0.0:7800 [--metrics-bind 127.0.0.1:7801] --zilja http://z1:7799 --zilja http://z2:7799 ...
//! zilja-aggregator --bind 0.0.0.0:7800 --ziljas-file zilet.txt     # një URL për rresht
//! ```
//!
//! Klienti: `new ZiljaClient({ ..., quorum: true, aggregators: ["https://agg.example"] })`. Klienti
//! i verifikon vetë të gjitha firmat; aggregator-it s'i besohet asgjë.

use std::sync::Arc;
use zilja_aggregator::{Aggregator, HttpFetch};
use zilja_node::rpc::{serve_handler, HttpLimits, Routes};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = || -> Result<(), String> {
        let mut bind = "127.0.0.1:7800".to_string();
        let mut metrics_bind = None;
        let mut ziljas = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut val = || it.next().cloned().ok_or(format!("{a} kërkon një vlerë"));
            match a.as_str() {
                "--bind" => bind = val()?,
                "--metrics-bind" => metrics_bind = Some(val()?),
                "--zilja" => ziljas.push(val()?),
                "--ziljas-file" => {
                    let p = val()?;
                    let text = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
                    ziljas.extend(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from));
                }
                _ => return Err(format!("argument i panjohur {a:?}; shih kokën e src/main.rs")),
            }
        }
        ziljas.sort();
        ziljas.dedup();
        if ziljas.is_empty() {
            return Err("duhet të paktën një --zilja ose --ziljas-file".into());
        }
        let agg = Arc::new(Aggregator::new(ziljas, HttpFetch));
        let a2 = agg.clone();
        std::thread::spawn(move || loop {
            a2.learn_tip();
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        let _main = serve_handler(&bind, agg.clone(), Routes { jsonrpc: true, metrics: false, cors: true }, HttpLimits::default()).map_err(|e| format!("{bind}: {e}"))?;
        let _metrics = match &metrics_bind {
            Some(m) => Some(serve_handler(m, agg.clone(), Routes { jsonrpc: false, metrics: true, cors: false }, HttpLimits::default()).map_err(|e| format!("{m}: {e}"))?),
            None => None,
        };
        eprintln!("zilja-aggregator: http://{bind} ({} zile)", agg_count(&agg));
        loop {
            std::thread::park();
        }
    };
    if let Err(e) = run() {
        eprintln!("gabim: {e}");
        std::process::exit(2);
    }
}

fn agg_count(a: &Aggregator<HttpFetch>) -> String {
    a.metrics_line()
}
