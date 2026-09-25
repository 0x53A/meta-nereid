//! Host objects owned by one guest session. Metadata never lives in guest memory.
use std::alloc::Layout;
use std::sync::Mutex;

struct Allocation {
    address: usize,
    generation: u64,
    layout: Layout,
    drop_value: unsafe fn(usize),
}
static OBJECTS: Mutex<Vec<Allocation>> = Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

unsafe fn drop_value<T>(address: usize) {
    std::ptr::drop_in_place(address as *mut T);
}

pub fn new<T>(value: T) -> *mut T {
    let ptr = Box::into_raw(Box::new(value));
    register(ptr, Layout::new::<T>());
    ptr
}

pub fn register<T>(ptr: *mut T, layout: Layout) {
    OBJECTS.lock().unwrap().push(Allocation {
        address: ptr as usize,
        generation: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        layout,
        drop_value: drop_value::<T>,
    });
}

pub fn generation<T>(ptr: *const T) -> Option<u64> {
    OBJECTS
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.address == ptr as usize)
        .map(|a| a.generation)
}

pub fn release<T>(ptr: *mut T) {
    let allocation = {
        let mut objects = OBJECTS.lock().unwrap();
        objects
            .iter()
            .position(|a| a.address == ptr as usize)
            .map(|i| objects.swap_remove(i))
    };
    if let Some(a) = allocation {
        dispose(a);
    }
}

fn dispose(a: Allocation) {
    unsafe {
        (a.drop_value)(a.address);
        if a.layout.size() != 0 {
            std::alloc::dealloc(a.address as *mut u8, a.layout);
        }
    }
}

/// Only after execution has ended and all callback registries have been cleared.
pub fn clear() {
    let objects = std::mem::take(&mut *OBJECTS.lock().unwrap());
    for a in objects {
        dispose(a);
    }
}
