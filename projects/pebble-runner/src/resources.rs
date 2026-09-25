//! Immutable resource-pack snapshots and byte views that retain their owner.
use std::ops::{Deref, Range};
use std::sync::Arc;

const HEADER_SIZE: usize = 12;
const ENTRY_SIZE: usize = 16;
const MAX_ENTRIES: usize = 256;
const CONTENT_START: usize = HEADER_SIZE + ENTRY_SIZE * MAX_ENTRIES;

pub struct ResourcePack(Arc<[u8]>);

pub struct ResourceData {
    pack: Arc<[u8]>,
    range: Range<usize>,
}

impl Deref for ResourceData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.pack[self.range.clone()]
    }
}

impl ResourcePack {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }

    pub fn get(&self, id: u32) -> Option<ResourceData> {
        let pack = &self.0;
        if pack.len() < HEADER_SIZE {
            return None;
        }
        let count = u32::from_le_bytes(pack[..4].try_into().ok()?) as usize;
        for index in 0..count.min(MAX_ENTRIES) {
            let start = HEADER_SIZE + index * ENTRY_SIZE;
            let entry = pack.get(start..start + ENTRY_SIZE)?;
            let entry_id = u32::from_le_bytes(entry[..4].try_into().ok()?);
            if entry_id != id {
                continue;
            }
            let offset = u32::from_le_bytes(entry[4..8].try_into().ok()?) as usize;
            let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let Some(start) = CONTENT_START.checked_add(offset) else {
                continue;
            };
            let Some(end) = start.checked_add(length) else {
                continue;
            };
            if end <= pack.len() {
                return Some(ResourceData {
                    pack: Arc::clone(pack),
                    range: start..end,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(data: &[u8]) -> ResourcePack {
        let mut bytes = vec![0; CONTENT_START];
        bytes[..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&7u32.to_le_bytes());
        bytes[HEADER_SIZE + 8..HEADER_SIZE + 12]
            .copy_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        ResourcePack::new(bytes)
    }

    #[test]
    fn views_retain_bytes_after_pack_is_replaced_or_dropped() {
        let mut current = pack(b"first app");
        let previous = current.get(7).unwrap();
        current = pack(b"next app");
        let next = current.get(7).unwrap();
        drop(current);
        assert_eq!(&*previous, b"first app");
        assert_eq!(&*next, b"next app");
    }

    #[test]
    fn repeated_views_share_pack_storage() {
        let pack = pack(b"shared");
        let a = pack.get(7).unwrap();
        let b = pack.get(7).unwrap();
        assert!(Arc::ptr_eq(&a.pack, &b.pack));
        assert!(pack.get(8).is_none());
    }

    #[test]
    fn empty_resource_and_incomplete_table_are_handled() {
        assert!(pack(b"").get(7).unwrap().is_empty());
        assert!(ResourcePack::new(vec![]).get(7).is_none());
        let mut header = vec![0; HEADER_SIZE];
        header[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(ResourcePack::new(header).get(7).is_none());
    }
}
