//! Composite keys: a tuple of [`Datum`]s encoded so that comparing the bytes
//! gives the same answer as comparing the tuples, part by part.
//!
//! ```text
//! integer  01, then the value with its sign bit flipped, 8 bytes big-endian
//! text     02, then the UTF-8 with each 00 written 00 FF, then a 00
//! ```
//!
//! The string layout is the one in FoundationDB's tuple layer. A tuple's
//! encoding is a prefix of the encoding of every longer tuple that starts
//! with it, so a leading part of a key selects one contiguous range.

use std::ops::Bound;

use crate::table::Datum;

const TAG_INTEGER: u8 = 0x01;
const TAG_TEXT: u8 = 0x02;

/// The bytes of a composite key.
pub fn encode_key(parts: &[Datum]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        match part {
            Datum::Integer(n) => {
                out.push(TAG_INTEGER);
                let flipped = (*n as u64) ^ (1 << 63);
                out.extend_from_slice(&flipped.to_be_bytes());
            }
            Datum::Text(s) => {
                out.push(TAG_TEXT);
                for &b in s.as_bytes() {
                    out.push(b);
                    if b == 0 {
                        out.push(0xff);
                    }
                }
                out.push(0);
            }
        }
    }
    out
}

/// The tuple in the bytes of a composite key.
pub fn decode_key(mut bytes: &[u8]) -> Result<Vec<Datum>, String> {
    let mut parts = Vec::new();
    while let Some((&tag, rest)) = bytes.split_first() {
        match tag {
            TAG_INTEGER => {
                let (n, rest) = rest
                    .split_first_chunk::<8>()
                    .ok_or("an integer is cut short")?;
                parts.push(Datum::Integer((u64::from_be_bytes(*n) ^ (1 << 63)) as i64));
                bytes = rest;
            }
            TAG_TEXT => {
                let mut text = Vec::new();
                let mut at = 0;
                loop {
                    match (rest.get(at), rest.get(at + 1)) {
                        (None, _) => return Err("a string has no terminator".into()),
                        (Some(0), Some(0xff)) => {
                            text.push(0);
                            at += 2;
                        }
                        (Some(0), _) => {
                            at += 1;
                            break;
                        }
                        (Some(&b), _) => {
                            text.push(b);
                            at += 1;
                        }
                    }
                }
                parts.push(Datum::Text(
                    String::from_utf8(text).map_err(|_| "a string isn't UTF-8")?,
                ));
                bytes = &rest[at..];
            }
            _ => return Err(format!("unknown tag {tag:#04x}")),
        }
    }
    Ok(parts)
}

/// The range of keys that extend the tuple encoded in `prefix`, itself
/// included. After a complete part the next byte is a tag, so it's below
/// `FF`; an escaped `00` inside a longer string is followed by `FF`, so a
/// different string that merely starts with the same bytes is out of range.
pub fn prefix_range(prefix: &[u8]) -> (Bound<Vec<u8>>, Bound<Vec<u8>>) {
    let mut end = prefix.to_vec();
    end.push(0xff);
    (Bound::Included(prefix.to_vec()), Bound::Excluded(end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::RangeBounds;

    fn text(s: &str) -> Datum {
        Datum::Text(s.into())
    }

    /// Every one- and two-part tuple over awkward values.
    fn tuples() -> Vec<Vec<Datum>> {
        let mut values: Vec<Datum> = [i64::MIN, -256, -1, 0, 1, 42, 100, 255, 256, i64::MAX]
            .map(Datum::Integer)
            .to_vec();
        for s in [
            "", "\0", "\0\0", "\u{1}", "a", "a\0", "a\0b", "a\u{1}", "al", "alice", "b", "ñ",
        ] {
            values.push(text(s));
        }
        let mut out = vec![vec![]];
        for a in &values {
            out.push(vec![a.clone()]);
            for b in &values {
                out.push(vec![a.clone(), b.clone()]);
            }
        }
        out
    }

    #[test]
    fn byte_order_is_tuple_order() {
        let all = tuples();
        for a in &all {
            for b in &all {
                assert_eq!(encode_key(a).cmp(&encode_key(b)), a.cmp(b), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn keys_round_trip() {
        for t in tuples() {
            assert_eq!(decode_key(&encode_key(&t)).unwrap(), t);
        }
    }

    #[test]
    fn a_leading_part_selects_exactly_the_tuples_that_start_with_it() {
        let all = tuples();
        for p in all
            .iter()
            .filter(|t| t.len() == 1)
        {
            let encoded = encode_key(p);
            let (low, high) = prefix_range(&encoded);
            for t in &all {
                let k = encode_key(t);
                let bounds = (
                    low.as_ref()
                        .map(Vec::as_slice),
                    high.as_ref()
                        .map(Vec::as_slice),
                );
                let in_range = bounds.contains(k.as_slice());
                assert_eq!(in_range, t.first() == p.first(), "{p:?} {t:?}");
            }
        }
    }

    #[test]
    fn decode_rejects_damaged_keys() {
        assert!(decode_key(&[0x01, 0, 0]).is_err());
        assert!(decode_key(&[0x02, b'a']).is_err());
        assert!(decode_key(&[0x07]).is_err());
    }

    #[test]
    fn a_string_with_an_escaped_zero_is_outside_the_prefix_of_its_start() {
        let (low, high) = prefix_range(&encode_key(&[text("a")]));
        let bounds = (
            low.as_ref()
                .map(Vec::as_slice),
            high.as_ref()
                .map(Vec::as_slice),
        );
        assert!(!bounds.contains(encode_key(&[text("a\0")]).as_slice()));
        assert!(bounds.contains(encode_key(&[text("a"), text("b")]).as_slice()));
    }
}
