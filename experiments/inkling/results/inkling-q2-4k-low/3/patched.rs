//! The number `equal_pairs` returns is what the tests check.

/// Number of ordered pairs `(i, j)` with `data[i] == data[j]`, including `i == j`.
#[inline(never)]
pub fn equal_pairs(data: &[u8]) -> u64 {
    let mut freq = [0u64; 256];
    for &v in data {
        freq[v as usize] = freq[v as usize].wrapping_add(1);
    }
    let mut count = 0u64;
    for &f in &freq {
        count = count.wrapping_add(f.wrapping_mul(f));
    }
    count
}

#[cfg(test)]
fn pattern(seed: u8) -> [u8; 64] {
    let mut data = [0u8; 64];
    let mut index = 0;
    while index < data.len() {
        data[index] = seed.wrapping_mul(index as u8).wrapping_add(3);
        index += 1;
    }
    data
}

#[test]
fn encode_batch() {
    let mut total = 0u64;
    for seed in 0..32u8 {
        let data = pattern(seed);
        total = total.wrapping_add(equal_pairs(&data));
    }
    assert_eq!(total, 6400);
}

#[test]
fn tiny_assert() {
    assert_eq!(equal_pairs(b""), 0);
    assert_eq!(equal_pairs(b"a"), 1);
    assert_eq!(equal_pairs(b"aa"), 4);
}
