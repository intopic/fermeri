//! R5-2: multi-proof-i jep të njëjtin vendim si dëftesat e veçanta, me më pak bajte; çdo ndryshim
//! i dëftesës ose i pretendimit e bën të pavlefshme.

use rand::{rngs::StdRng, Rng, SeedableRng};
use zilja_tree::Tree;
use zilja_verifier::multi::{verify_accounts, MultiProof};
use zilja_verifier::{tree_key, verify_account, AccountMeta, Key, Verdict, EMPTY};

fn meta(rng: &mut StdRng) -> AccountMeta {
    AccountMeta { lamports: rng.gen_range(1..1 << 40), owner: rng.gen(), executable: false, data_len: rng.gen_range(0..200), data_hash: rng.gen() }
}

struct World {
    tree: Tree,
    accounts: Vec<(Key, AccountMeta)>,
}

fn world(rng: &mut StdRng, n: usize) -> World {
    let accounts: Vec<(Key, AccountMeta)> = (0..n).map(|_| (rng.gen(), meta(rng))).collect();
    let tree = Tree::from_accounts(accounts.iter().map(|(pk, m)| (tree_key(pk), Some(m.clone()))).collect());
    World { tree, accounts }
}

fn multi_for(w: &World, pks: &[Key]) -> (MultiProof, usize) {
    let entries: Vec<_> = pks.iter().map(|pk| (tree_key(pk), w.tree.prove(&tree_key(pk)).0)).collect();
    let single_bytes: usize = entries.iter().map(|(_, p)| p.to_bytes().len()).sum();
    (MultiProof::from_proofs(&entries).unwrap().1, single_bytes)
}

fn claims<'a>(w: &'a World, pks: &[Key]) -> Vec<(Key, Option<&'a AccountMeta>)> {
    pks.iter().map(|pk| (*pk, w.accounts.iter().find(|(k, _)| k == pk).map(|(_, m)| m))).collect()
}

fn pick(rng: &mut StdRng, w: &World, n: usize) -> Vec<Key> {
    (0..n).map(|_| if !w.accounts.is_empty() && rng.gen_bool(0.75) { w.accounts[rng.gen_range(0..w.accounts.len())].0 } else { rng.gen() }).collect()
}

#[test]
fn multi_proof_agrees_with_single_proofs() {
    let mut rng = StdRng::seed_from_u64(52);
    for n in [0usize, 1, 2, 3, 17, 300, 5000] {
        let w = world(&mut rng, n);
        let root = w.tree.root();
        for k in [1usize, 2, 5, 20, 64] {
            let pks = pick(&mut rng, &w, k);
            let (mp, _) = multi_for(&w, &pks);
            let c = claims(&w, &pks);
            let got = verify_accounts(&root, &c, &mp);
            for ((pk, m), v) in c.iter().zip(&got) {
                let single = verify_account(&root, pk, *m, &w.tree.prove(&tree_key(pk)).0);
                assert_eq!(*v, single, "n {n} k {k}");
                assert_ne!(*v, Verdict::Invalid, "n {n} k {k}");
            }
            // Kodimi kanonik: bajtet → dëftesë → të njëjtat bajte.
            let b = mp.to_bytes();
            let back = MultiProof::from_bytes(&b).unwrap();
            assert_eq!(back, mp);
            assert_eq!(back.to_bytes(), b);
        }
    }
}

#[test]
fn any_change_to_proof_or_claim_is_invalid() {
    let mut rng = StdRng::seed_from_u64(53);
    let w = world(&mut rng, 3000);
    let root = w.tree.root();
    for _ in 0..40 {
        let k = rng.gen_range(2..30);
        let pks = pick(&mut rng, &w, k);
        let (mp, _) = multi_for(&w, &pks);
        let c = claims(&w, &pks);
        let all_invalid = |p: &MultiProof, c: &[(Key, Option<&AccountMeta>)]| verify_accounts(&root, c, p).iter().all(|v| *v == Verdict::Invalid);
        // Një vëlla i ndryshuar, i hequr ose i shtuar.
        if !mp.siblings.is_empty() {
            let mut p = mp.clone();
            let i = rng.gen_range(0..p.siblings.len());
            p.siblings[i][rng.gen_range(0..32)] ^= 1 << rng.gen_range(0..8);
            assert!(all_invalid(&p, &c));
            let mut p = mp.clone();
            p.siblings.remove(i);
            assert!(all_invalid(&p, &c));
        }
        let mut p = mp.clone();
        p.siblings.push([9; 32]);
        assert!(all_invalid(&p, &c));
        // Thellësia e një përfundimi.
        let mut p = mp.clone();
        let t = rng.gen_range(0..p.terminals.len());
        p.terminals[t].0 = p.terminals[t].0.wrapping_add(1);
        assert!(all_invalid(&p, &c));
        // Një pretendim i gabuar (meta tjetër, ose "s'ekziston" për një që ekziston) prish vetëm atë.
        let j = rng.gen_range(0..c.len());
        let other = meta(&mut rng);
        let mut bad = c.clone();
        bad[j].1 = match c[j].1 {
            Some(_) => None,
            None => Some(&other),
        };
        let v = verify_accounts(&root, &bad, &mp);
        for (x, (orig, now)) in c.iter().zip(&bad).enumerate() {
            if orig.0 == bad[j].0 {
                assert_eq!(v[x], Verdict::Invalid, "pretendimi i ndryshuar");
            } else {
                let _ = now;
                assert_ne!(v[x], Verdict::Invalid);
            }
        }
        // Root tjetër.
        assert!(verify_accounts(&[1; 32], &c, &mp).iter().all(|v| *v == Verdict::Invalid));
        // Një çelës më pak ose më shumë se dëftesa.
        if c.len() > 1 {
            let unique: std::collections::BTreeSet<_> = pks.iter().collect();
            if unique.len() > 1 {
                let keep: Vec<_> = c.iter().filter(|x| x.0 != c[0].0).cloned().collect();
                assert!(all_invalid(&mp, &keep));
            }
        }
        let mut more = c.clone();
        let extra: Key = rng.gen();
        more.push((extra, None));
        assert!(all_invalid(&mp, &more));
    }
}

#[test]
fn non_canonical_encodings_are_refused() {
    let mut rng = StdRng::seed_from_u64(54);
    let w = world(&mut rng, 500);
    let pks = pick(&mut rng, &w, 6);
    let (mp, _) = multi_for(&w, &pks);
    let b = mp.to_bytes();
    // Bajte të tepërta, version tjetër, cungim.
    let mut x = b.clone();
    x.push(0);
    assert!(MultiProof::from_bytes(&x).is_none());
    let mut x = b.clone();
    x[0] = 2;
    assert!(MultiProof::from_bytes(&x).is_none());
    for cut in [1, 2, 5, b.len() - 1] {
        assert!(MultiProof::from_bytes(&b[..cut]).is_none());
    }
    // Vëlla bosh i koduar si jo-bosh.
    let mut p = mp.clone();
    p.siblings.push(EMPTY);
    let mut enc = p.to_bytes();
    let n = p.siblings.len();
    let bm_at = enc.len() - (p.siblings.iter().filter(|s| **s != EMPTY).count() * 32) - n.div_ceil(8);
    enc[bm_at + (n - 1) / 8] |= 1 << (7 - ((n - 1) % 8));
    enc.extend_from_slice(&EMPTY);
    assert!(MultiProof::from_bytes(&enc).is_none());
    // Zero çelësa.
    assert!(MultiProof::from_bytes(&[0x11, 0, 0, 0, 0, 0, 0]).is_none());
}

/// Matja e madhësisë (shfaqet me `--nocapture`): multi-proof kundrejt dëftesave të veçanta.
#[test]
fn size_saving_is_measured() {
    let mut rng = StdRng::seed_from_u64(55);
    let w = world(&mut rng, 200_000);
    for k in [2usize, 10, 50] {
        let mut tot_single = 0;
        let mut tot_multi = 0;
        for _ in 0..20 {
            let pks: Vec<Key> = (0..k).map(|_| w.accounts[rng.gen_range(0..w.accounts.len())].0).collect();
            let (mp, single) = multi_for(&w, &pks);
            tot_single += single;
            tot_multi += mp.to_bytes().len();
        }
        let saving = 100.0 * (1.0 - tot_multi as f64 / tot_single as f64);
        println!("200k llogari, {k} çelësa të rastit: {} B të veçanta → {} B multi ({saving:.0}% më pak)", tot_single / 20, tot_multi / 20);
        assert!(tot_multi < tot_single || k < 2);
    }
}
