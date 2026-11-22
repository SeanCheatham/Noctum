#[inline(never)]
pub fn is_rotation(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    if a.is_empty() {
        return true;
    }
    for shift in 0..a.len() {
        let rotated: Vec<u8> = a.iter().skip(shift).chain(a.iter().take(shift)).copied().collect();
        if rotated == b {
            return true;
        }
    }
    false
}

#[test]
fn encode_batch() {
    assert!(is_rotation(b"", b""));
    assert!(is_rotation(b"abcd", b"cdab"));
    assert!(!is_rotation(b"abcd", b"abdc"));
    assert!(!is_rotation(b"a", b""));
    for shift in 0..32 {
        let input: Vec<u8> = (0..32).collect();
        let rotated: Vec<u8> = input[shift..].iter().chain(input[..shift].iter()).copied().collect();
        assert!(is_rotation(&input, &rotated));
    }
}
