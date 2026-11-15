//! Mat kohën e përgjigjes së `getAccountProof` mbi HTTP, me një pemë të madhe.
//!
//! `cargo run -p zilja-bench --release --bin zilja-rpc-bench -- [llogari] [kërkesa]`

use rand::{rngs::StdRng, Rng, SeedableRng};
use serde_json::{json, Value};
use std::time::Instant;
use zilja_node::{rpc, Shared};
use zilja_tree::{StateTree, Tree};
use zilja_verifier::{AccountMeta, Key};

/// Klient HTTP minimal: e gjithë kërkesa në një write, TCP_NODELAY, `Connection: close`
/// (që matja të mos përfshijë vonesën 40 ms të Nagle/delayed-ACK të disa klientëve).
fn post(addr: &str, body: &str) -> Value {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_nodelay(true).unwrap();
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let json = out.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str(json).unwrap()
}

fn main() {
    let args: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    let n = *args.first().unwrap_or(&1_000_000);
    let reqs = *args.get(1).unwrap_or(&2_000);
    let mut rng = StdRng::seed_from_u64(7);
    let keys: Vec<Key> = (0..n).map(|_| rng.gen()).collect();
    let tree = Tree::from_accounts(
        keys.iter()
            .map(|k| (*k, Some(AccountMeta { lamports: rng.gen_range(1..1 << 40), data_len: 165, ..Default::default() })))
            .collect(),
    );
    let shared = Shared::default();
    {
        let mut st = shared.state.write().unwrap();
        let mut mt = zilja_tree::MemTree::new();
        mt.tree = tree;
        mt.apply_slot(1, vec![], None).unwrap();
        st.tree = Box::new(mt);
        st.slot = 1;
        st.ready = true;
        st.complete = true;
    }
    let bind = "127.0.0.1:7798";
    rpc::serve(bind, shared).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut lat = Vec::with_capacity(reqs);
    let mut bytes = Vec::with_capacity(reqs);
    for i in 0..reqs {
        // Gjysma ekzistuese, gjysma që s'ekzistojnë.
        let k: Key = if i % 2 == 0 { keys[rng.gen_range(0..n)] } else { rng.gen() };
        let body = json!({"jsonrpc":"2.0","id":i,"method":"getAccountProof","params":[bs58::encode(k).into_string()]});
        let t = Instant::now();
        let resp = post(bind, &body.to_string());
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
        bytes.push(resp["result"]["proofBytes"].as_u64().unwrap() as f64);
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    bytes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |v: &Vec<f64>, q: f64| v[((v.len() - 1) as f64 * q) as usize];
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "accounts": n, "requests": reqs,
            "latency_ms_p50": p(&lat, 0.5), "latency_ms_p99": p(&lat, 0.99), "latency_ms_max": p(&lat, 1.0),
            "proof_bytes_p50": p(&bytes, 0.5), "proof_bytes_max": p(&bytes, 1.0),
        }))
        .unwrap()
    );
}
