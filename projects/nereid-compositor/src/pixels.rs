//! Owned copies of client pixels; never retain references into mutable client SHM.
pub struct SurfaceBuffer {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub swap_rb: bool,
    pub has_alpha: bool,
}

impl SurfaceBuffer {
    /// The mapping must be readable for `pool_len` bytes during this call.
    pub unsafe fn copy_from_pool(
        ptr: *const u8,
        pool_len: usize,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        swap_rb: bool,
        has_alpha: bool,
    ) -> Option<Self> {
        let offset = usize::try_from(offset).ok()?;
        let width = u32::try_from(width).ok().filter(|n| *n > 0)?;
        let height = u32::try_from(height).ok().filter(|n| *n > 0)?;
        let stride = u32::try_from(stride).ok()?;
        let row_len = width.checked_mul(4)?;
        if stride < row_len {
            return None;
        }
        let len = (stride as usize)
            .checked_mul((height - 1) as usize)?
            .checked_add(row_len as usize)?;
        if offset.checked_add(len)? > pool_len {
            return None;
        }
        let mut data = vec![0; len];
        unsafe {
            std::ptr::copy_nonoverlapping(ptr.add(offset), data.as_mut_ptr(), len);
        }
        Some(Self {
            data,
            width,
            height,
            stride,
            swap_rb,
            has_alpha,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn honors_offset_and_last_row_extent() {
        let pool = [9, 9, 9, 9, 0, 255, 0, 255, 7, 7, 7, 7, 255, 0, 0, 255];
        let b = unsafe {
            SurfaceBuffer::copy_from_pool(pool.as_ptr(), pool.len(), 4, 1, 2, 8, true, true)
        }
        .unwrap();
        assert_eq!(b.data, pool[4..]);
        assert_eq!(b.data.len(), 12);
    }
    #[test]
    fn rejects_invalid_extents() {
        let pool = [0; 8];
        for (offset, w, h, stride) in [
            (5, 1, 1, 4),
            (-1, 1, 1, 4),
            (0, 3, 1, 4),
            (0, 1, 0, 4),
            (0, i32::MAX, 2, i32::MAX),
        ] {
            assert!(
                unsafe {
                    SurfaceBuffer::copy_from_pool(
                        pool.as_ptr(),
                        pool.len(),
                        offset,
                        w,
                        h,
                        stride,
                        true,
                        true,
                    )
                }
                .is_none()
            );
        }
    }
}
