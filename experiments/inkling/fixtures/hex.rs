#[inline(never)]
pub fn hex_encode(input: &[u8]) -> String {
    let mut output = String::new();
    for byte in input {
        output.push_str(&format!("{byte:02x}"));
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
