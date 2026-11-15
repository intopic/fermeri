//! Rregullat e formatit në rrjet (JSON), të përbashkëta për Rust, WASM dhe JS (F-03, F-08).
//!
//! - `lamports` dhe `dataLen` janë tekst dhjetor në [0, 2^64): pa shenjë, pa hapësira,
//!   pa zero në fillim. Një numër JSON pranohet vetëm kur është ≤ 2^53 - 1, sepse JS e
//!   rrumbullakon çdo numër më të madh para se ta shohë verifikuesi.
//! - `executable` është boolean JSON i vërtetë.

/// Numri më i madh që JS e mban saktë.
pub const MAX_SAFE_JSON_INT: u64 = (1 << 53) - 1;

/// u64 i rreptë nga tekst dhjetor.
pub fn parse_u64_dec(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_decimal() {
        assert_eq!(parse_u64_dec("0"), Some(0));
        assert_eq!(parse_u64_dec("18446744073709551615"), Some(u64::MAX));
        for bad in ["18446744073709551616", "-1", "+1", " 1", "1 ", "01", "", "1e3", "0x10", "1.0"] {
            assert_eq!(parse_u64_dec(bad), None, "{bad}");
        }
    }
}
