//! Lower-case hex, for keys and digests in JSON and file names.

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(super::encode(&bytes), "0001abff");
        assert_eq!(super::decode("0001abff").unwrap(), bytes);
        assert!(super::decode("abc").is_none());
        assert!(super::decode("zz").is_none());
    }
}
