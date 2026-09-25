//! Bounded guest arena with out-of-band allocation sizes and reusable holes.
use std::collections::BTreeMap;
use std::sync::Mutex;
const SIZE: usize = 512 * 1024;
struct Heap {
    base: usize,
    blocks: BTreeMap<usize, usize>,
}
static HEAP: Mutex<Heap> = Mutex::new(Heap {
    base: 0,
    blocks: BTreeMap::new(),
});
impl Heap {
    fn alloc(&mut self, size: usize) -> *mut u8 {
        let Some(size) = size
            .checked_add(7)
            .map(|n| n & !7)
            .filter(|&n| n > 0 && n <= SIZE)
        else {
            return std::ptr::null_mut();
        };
        if self.base == 0 {
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return std::ptr::null_mut();
            }
            self.base = ptr as usize;
        }
        let mut offset = 0;
        for (&start, &len) in &self.blocks {
            if start - offset >= size {
                break;
            }
            offset = start + len;
        }
        if offset + size > SIZE {
            return std::ptr::null_mut();
        }
        self.blocks.insert(offset, size);
        let ptr = (self.base + offset) as *mut u8;
        unsafe {
            ptr.write_bytes(0, size);
        }
        ptr
    }
}
pub extern "C" fn malloc(size: usize) -> *mut u8 {
    HEAP.lock().unwrap().alloc(size)
}
pub extern "C" fn calloc(n: usize, size: usize) -> *mut u8 {
    n.checked_mul(size)
        .map(|size| malloc(size))
        .unwrap_or(std::ptr::null_mut())
}
pub extern "C" fn free(ptr: *mut u8) {
    free_if_owned(ptr);
}
pub fn free_if_owned(ptr: *mut u8) -> bool {
    let mut heap = HEAP.lock().unwrap();
    let addr = ptr as usize;
    if heap.base != 0 && addr >= heap.base && addr - heap.base < SIZE {
        let offset = addr - heap.base;
        heap.blocks.remove(&offset);
        true
    } else {
        false
    }
}
pub extern "C" fn realloc(ptr: *mut u8, size: usize) -> *mut u8 {
    if ptr.is_null() {
        return malloc(size);
    }
    if size == 0 {
        free(ptr);
        return std::ptr::null_mut();
    }
    let mut heap = HEAP.lock().unwrap();
    let Some(offset) = (ptr as usize).checked_sub(heap.base) else {
        return std::ptr::null_mut();
    };
    let Some(&old_size) = heap.blocks.get(&offset) else {
        return std::ptr::null_mut();
    };
    if size <= old_size {
        return ptr;
    }
    let new = heap.alloc(size);
    if !new.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(ptr, new, old_size.min(size));
        }
        heap.blocks.remove(&offset);
    }
    new
}
pub fn reset() {
    HEAP.lock().unwrap().blocks.clear();
}
