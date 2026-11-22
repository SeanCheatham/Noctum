#[inline(never)]
pub fn hex_encode(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(input.len() * 2);
    for &byte in input {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[test]
fn encode_batch() {
    assert_eq!(hex_encode(&[]), "");
    assert_eq!(hex_encode(&[0, 15, 16, 128, 255]), "000f1080ff");
    for seed in 0..32u8 {
        let input = [seed; 64];
        assert_eq!(hex_encode(&input), format!("{seed:02x}").repeat(64));
    }
}
