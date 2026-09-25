/// Check the guest's frame contract before copying into a fixed-size image.
pub fn pixels(memory: &[u8], pointer: usize, length: usize, expected: usize) -> Option<&[u8]> {
    if length != expected {
        return None;
    }
    memory.get(pointer..pointer.checked_add(length)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_frame_can_end_at_memory_boundary() {
        let memory: Vec<u8> = (0..32).collect();
        assert_eq!(pixels(&memory, 16, 16, 16), Some(&memory[16..]));
        assert_eq!(pixels(&memory, 0, 16, 16), Some(&memory[..16]));
    }

    #[test]
    fn size_mismatch_and_invalid_ranges_have_no_pixels() {
        let memory = [0; 32];
        for (pointer, length) in [(0, 8), (0, 24), (17, 16), (33, 16), (usize::MAX, 16)] {
            assert_eq!(pixels(&memory, pointer, length, 16), None);
        }
    }
}
