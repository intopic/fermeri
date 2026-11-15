---
simd: 'XXXX'
title: Global Account State Commitment with Inclusion Proofs ("Zilja")
authors:
  - intopic
category: Standard
type: Core
status: Idea
created: 2026-11-15
---

> **Draft.** Shkruar për diskutim në forumin e Solanës. Numrat vijnë nga
> `docs/RAPORTI.md` dhe janë matur në një makinë të vogël, jo në mainnet.

## Summary

Commit, for every rooted slot, a single 32-byte root of a canonical sparse
Merkle tree over **all** accounts, and include it in the data validators vote
on. Any client can then verify any account's state (or its absence) with a
~1 KB proof against a root signed by >2/3 of stake, without trusting an RPC
provider.

## Motivation

Today a wallet or a bridge learns an account's balance from an RPC node and
has to trust it. Solana's accounts lattice hash commits to the full state and
updates it cheaply, but it is a homomorphic set hash: it **cannot produce a
proof for a single account**. Light clients, trust-minimized bridges and
verifiable RPC all need per-account inclusion and exclusion proofs.

## New Terminology

- **Zilja root**: the root of the account state tree at a rooted slot.
- **Account proof**: a Merkle path from an account leaf to the Zilja root.

## Detailed Design

### Tree

Defined in [`SPEC.md`](SPEC.md):

- `value_hash = SHA256("zilja/account/v1" ‖ lamports ‖ owner ‖ executable ‖ data_len ‖ SHA256(data))`
- `leaf = SHA256(0x00 ‖ pubkey ‖ value_hash)`, `internal = SHA256(0x01 ‖ left ‖ right)`, `empty = 0^32`
- A binary sparse Merkle tree keyed by pubkey, in compact canonical form: a subtree holding one account is that leaf. Accounts with zero lamports are absent.

Committing `SHA256(data)` rather than `data` keeps proofs small for large
accounts. The client fetches the data from any source and checks the hash.

### Commitment

Phase A (no consensus change; implemented in this repo): validators run the
tree as a Geyser plugin and sign `"zilja/attest/v1" ‖ slot ‖ root` with their
identity key. Clients check that the signers hold >2/3 of epoch stake.

Phase B (this proposal): the Zilja root is mixed into the bank hash, so
existing votes, or Alpenglow certificates, attest to it with no extra
signatures. A feature gate activates it.

### RPC

`getAccountProof(pubkey) → {slot, root, account, proof}` and
`getZiljaRoot() → {slot, root}`, as in `SPEC.md`.

## Alternatives Considered

- **Lattice hash only**: cheap to update, but no per-account proofs.
- **Recompute a full Merkle tree per slot**: too slow. Incremental batch updates are required, and they are what this design uses.
- **Verkle trees**: smaller proofs, but heavier cryptography for every client and slower updates.
- **ZK proofs of state**: strongest, but proving cost at Solana's throughput is not practical today. The Zilja root is a natural public input for future ZK light clients.

## Impact

- **Validators**: extra CPU to hash the changed paths each slot (measured at ~270k account updates/s on 2 cores, and it parallelizes) and extra memory for the tree (~300–370 B/account in RAM; a disk-backed tree is needed for mainnet scale).
- **Wallets and bridges**: can verify state with a 5 KB JS or 64 KB WASM verifier.
- **RPC providers**: can offer verifiable responses.

## Security Considerations

- Domain separation between leaves and internal nodes prevents second-preimage confusion.
- The proof parser rejects trailing bytes, depth > 256, and leaves outside the key's path.
- Before Phase B, the root's trust comes from Phase A signatures and the stake table source. The stake table must come from a trusted checkpoint, as in other light-client designs.

## Backwards Compatibility

Phase A has none. Phase B changes the bank hash and needs a feature gate, plus
an implementation in every validator client (Agave, Firedancer).

## Open Questions

1. The per-slot write volume on mainnet, and the CPU it costs a full validator.
2. Whether to commit every slot or every N slots, to cut cost.
3. The hash function: SHA-256 (hardware support everywhere) versus Blake3.
4. The disk-backed tree design, and the startup time from a full snapshot.
