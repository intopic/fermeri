//! Rregullat e formatit në rrjet (JSON), të përbashkëta për Rust, WASM dhe JS (F-03, F-08).
//!
//! - `lamports` dhe `dataLen` janë tekst dhjetor në [0, 2^64): pa shenjë, pa hapësira,
//!   pa zero në fillim. Një numër JSON pranohet vetëm kur është ≤ 2^53 - 1, sepse JS e
//!   rrumbullakon çdo numër më të madh para se ta shohë verifikuesi.
//! - `executable` është boolean JSON i vërtetë.
//! - A20: hex-i është vetëm `[0-9a-f]`, me gjatësi çift. Shkronjat e mëdha refuzohen kudo (Rust,
//!   WASM, CLI, JS): një prodhues kanonik s'i nxjerr kurrë, dhe pranimi i tyre do të linte dy
//!   shkrime të ndryshme për të njëjtat bajte.

/// Numri më i madh që JS e mban saktë.
pub const MAX_SAFE_JSON_INT: u64 = (1 << 53) - 1;

/// u64 i rreptë nga tekst dhjetor.
pub fn parse_u64_dec(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

/// A20: hex kanonik: vetëm `[0-9a-f]`, gjatësi çift.
pub fn decode_hex_lower(s: &str) -> Option<Vec<u8>> {
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    b.chunks_exact(2).map(|p| Some(nib(p[0])? << 4 | nib(p[1])?)).collect()
}

/// A20: hex kanonik me saktësisht `N` bajte.
pub fn hex_lower<const N: usize>(s: &str) -> Option<[u8; N]> {
    decode_hex_lower(s)?.try_into().ok()
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

    #[test]
    fn hex_is_lowercase_only() {
        assert_eq!(decode_hex_lower("00ab"), Some(vec![0, 0xab]));
        assert_eq!(hex_lower::<2>("00ab"), Some([0, 0xab]));
        for bad in ["00AB", "0aB0", "abc", "0g", " 00", "00 ", "+0"] {
            assert_eq!(decode_hex_lower(bad), None, "{bad}");
        }
        assert_eq!(hex_lower::<3>("00ab"), None);
    }
}
