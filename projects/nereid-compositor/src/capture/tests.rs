use super::*;

#[test]
fn rgba_copy_respects_offset_stride_channels_and_opaque_alpha() {
    let src = [255, 0, 7, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let mut dst = [0xcc; 28];
    let spec = BufferData {
        offset: 3,
        width: 2,
        height: 2,
        stride: 12,
        format: wl_shm::Format::Xrgb8888,
    };
    unsafe {
        copy_rgba(&src, 2, 2, dst.as_mut_ptr(), dst.len(), spec).unwrap();
    }
    assert_eq!(&dst[3..11], &[7, 0, 255, 255, 3, 2, 1, 255]);
    assert_eq!(&dst[15..23], &[7, 6, 5, 255, 11, 10, 9, 255]);
    for i in [0, 1, 2, 11, 12, 13, 14, 23, 24, 25, 26, 27] {
        assert_eq!(dst[i], 0xcc);
    }
}

#[test]
fn invalid_layout_never_writes_client_memory() {
    let base = BufferData {
        offset: 0,
        width: 2,
        height: 2,
        stride: 8,
        format: wl_shm::Format::Xrgb8888,
    };
    let invalid = [
        BufferData { offset: -1, ..base },
        BufferData { stride: 7, ..base },
        BufferData { width: 1, ..base },
        BufferData { height: 3, ..base },
        BufferData {
            offset: i32::MAX,
            ..base
        },
        BufferData {
            stride: i32::MAX,
            ..base
        },
        BufferData {
            format: wl_shm::Format::Argb8888,
            ..base
        },
    ];
    for spec in invalid {
        let mut dst = [0xcc; 16];
        assert!(unsafe { copy_rgba(&[0; 16], 2, 2, dst.as_mut_ptr(), dst.len(), spec) }.is_err());
        assert_eq!(dst, [0xcc; 16]);
    }
    let mut dst = [0xcc; 16];
    assert!(unsafe { copy_rgba(&[0; 15], 2, 2, dst.as_mut_ptr(), dst.len(), base) }.is_err());
    assert!(unsafe { copy_rgba(&[0; 16], 2, 2, dst.as_mut_ptr(), 15, base) }.is_err());
    assert_eq!(dst, [0xcc; 16]);
}
