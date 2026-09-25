//! Logical HIDL reply decoding. Binder object traversal remains in libgbinder.
//! Layout recovered from the packaged v1.0 proxy; see docs in task 0071.
pub trait Reader {
    fn word(&mut self) -> Result<u32, String>;
    fn color(&mut self) -> Result<[u8; 20], String>;
    fn at_end(&self) -> bool;
}
#[derive(Debug, PartialEq)]
pub struct Capabilities {
    pub operations: u32,
    pub rgb_bits: [u32; 3],
    pub palette_size: u32,
    /// Meaning of the trailing fields/padding is not yet recovered.
    pub color_tail: [u8; 4],
    pub available_memory: u32,
    pub width: u32,
    pub height: u32,
}
pub fn check_status(value: u32, layer: &str) -> Result<(), String> {
    if value == 0 {
        Ok(())
    } else {
        Err(format!("{layer} status {} (0x{value:08x})", value as i32))
    }
}
pub fn capabilities(reader: &mut impl Reader) -> Result<Capabilities, String> {
    check_status(reader.word()?, "HIDL exception")?;
    check_status(reader.word()?, "Sidekick HAL")?;
    let operations = reader.word()?;
    let color = reader.color()?;
    let words: Vec<_> = color[..16]
        .chunks_exact(4)
        .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
        .collect();
    let result = Capabilities {
        operations,
        rgb_bits: [words[0], words[1], words[2]],
        palette_size: words[3],
        color_tail: color[16..20].try_into().unwrap(),
        available_memory: reader.word()?,
        width: reader.word()?,
        height: reader.word()?,
    };
    if !reader.at_end() {
        return Err("unexpected trailing capability data".into());
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Clone)]
    enum Item {
        Word(u32),
        Buffer(Vec<u8>),
    }
    struct Fixture(VecDeque<Item>);
    impl Reader for Fixture {
        fn word(&mut self) -> Result<u32, String> {
            match self.0.pop_front() {
                Some(Item::Word(v)) => Ok(v),
                _ => Err("missing word".into()),
            }
        }
        fn color(&mut self) -> Result<[u8; 20], String> {
            match self.0.pop_front() {
                Some(Item::Buffer(v)) => v.try_into().map_err(|_| "bad color size".into()),
                _ => Err("missing color".into()),
            }
        }
        fn at_end(&self) -> bool {
            self.0.is_empty()
        }
    }
    fn reply() -> Vec<Item> {
        let mut color = Vec::new();
        for n in [3u32, 3, 2, 256, 1] {
            color.extend(n.to_le_bytes());
        }
        vec![
            Item::Word(0),
            Item::Word(0),
            Item::Word(0x100000b),
            Item::Buffer(color),
            Item::Word(65536),
            Item::Word(416),
            Item::Word(416),
        ]
    }
    #[test]
    fn decodes_known_layout_and_preserves_unknown_color_fields() {
        let c = capabilities(&mut Fixture(reply().into())).unwrap();
        assert_eq!(
            c,
            Capabilities {
                operations: 0x100000b,
                rgb_bits: [3, 3, 2],
                palette_size: 256,
                color_tail: [1, 0, 0, 0],
                available_memory: 65536,
                width: 416,
                height: 416
            }
        );
    }
    #[test]
    fn rejects_every_truncation_and_wrong_buffer_size() {
        let r = reply();
        for n in 0..r.len() {
            assert!(capabilities(&mut Fixture(r[..n].to_vec().into())).is_err());
        }
        for size in [0, 19, 21, 24] {
            let mut r = reply();
            r[3] = Item::Buffer(vec![0; size]);
            assert!(capabilities(&mut Fixture(r.into())).is_err());
        }
        let mut r = reply();
        r.push(Item::Word(0));
        assert!(capabilities(&mut Fixture(r.into())).is_err());
    }
    #[test]
    fn distinguishes_transport_exception_from_hal_failure() {
        for (index, expected) in [(0, "HIDL exception"), (1, "Sidekick HAL")] {
            let mut r = reply();
            r[index] = Item::Word(u32::MAX);
            assert!(capabilities(&mut Fixture(r.into()))
                .unwrap_err()
                .contains(expected));
        }
    }
}
