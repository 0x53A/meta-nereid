//! Experimental upload bindings based on the capability probe.
//! Minimal dynamic bindings to libgbinder 1.1.47's public ABI.
//! Reader size and function declarations checked against the pinned headers:
//! https://github.com/mer-hybris/libgbinder/tree/1.1.47/include
use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr;

type Ptr = *mut c_void;
#[repr(C)]
pub struct RawReader {
    data: [*const c_void; 6],
}
macro_rules! api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        pub struct Api { $(pub $name: $ty,)* _library: Library }
        impl Api {
            pub fn load() -> Result<Self,String> {
                // No arbitrary library path or transaction override in the CLI.
                let library = unsafe { Library::new("libgbinder.so.1") }.map_err(|e|format!("load libgbinder.so.1: {e}"))?;
                unsafe { Self::from_library(library) }
            }
            unsafe fn from_library(library: Library) -> Result<Self,String> {
                Ok(Self { $($name: *library.get::<$ty>(concat!(stringify!($name),"\0").as_bytes()).map_err(|e|e.to_string())?,)* _library: library })
            }
        }
    }
}
#[repr(C)]
pub struct RawWriter { pub data: [*const c_void; 4] }
#[repr(C)]
pub struct Parent { pub index: u32, pub offset: u32 }
api! {
    gbinder_local_request_init_writer: unsafe extern "C" fn(Ptr,*mut RawWriter),
    gbinder_writer_append_int32: unsafe extern "C" fn(*mut RawWriter,u32),
    gbinder_writer_append_buffer_object: unsafe extern "C" fn(*mut RawWriter,*const c_void,usize)->u32,
    gbinder_writer_append_buffer_object_with_parent: unsafe extern "C" fn(*mut RawWriter,*const c_void,usize,*const Parent)->u32,
    gbinder_writer_append_hidl_vec: unsafe extern "C" fn(*mut RawWriter,*const c_void,u32,u32),
    gbinder_servicemanager_new: unsafe extern "C" fn(*const c_char)->Ptr,
    gbinder_servicemanager_unref: unsafe extern "C" fn(Ptr),
    gbinder_servicemanager_is_present: unsafe extern "C" fn(Ptr)->c_int,
    gbinder_servicemanager_list_sync: unsafe extern "C" fn(Ptr)->*mut *mut c_char,
    gbinder_servicemanager_get_service_sync: unsafe extern "C" fn(Ptr,*const c_char,*mut c_int)->Ptr,
    gbinder_client_new: unsafe extern "C" fn(Ptr,*const c_char)->Ptr,
    gbinder_client_unref: unsafe extern "C" fn(Ptr),
    gbinder_client_new_request: unsafe extern "C" fn(Ptr)->Ptr,
    gbinder_client_transact_sync_reply: unsafe extern "C" fn(Ptr,u32,Ptr,*mut c_int)->Ptr,
    gbinder_local_request_unref: unsafe extern "C" fn(Ptr),
    gbinder_remote_reply_unref: unsafe extern "C" fn(Ptr),
    gbinder_remote_reply_init_reader: unsafe extern "C" fn(Ptr,*mut RawReader),
    gbinder_reader_read_uint32: unsafe extern "C" fn(*mut RawReader,*mut u32)->c_int,
    gbinder_reader_read_hidl_struct1: unsafe extern "C" fn(*mut RawReader,usize)->*const c_void,
    gbinder_reader_read_hidl_string_vec: unsafe extern "C" fn(*mut RawReader)->*mut *mut c_char,
    gbinder_reader_at_end: unsafe extern "C" fn(*const RawReader)->c_int,
    g_strfreev: unsafe extern "C" fn(*mut *mut c_char),
}
pub struct Handle<'a> {
    pub ptr: Ptr,
    free: unsafe extern "C" fn(Ptr),
    _api: &'a Api,
}
impl<'a> Handle<'a> {
    pub fn new(
        api: &'a Api,
        ptr: Ptr,
        free: unsafe extern "C" fn(Ptr),
        name: &str,
    ) -> Result<Self, String> {
        if ptr.is_null() {
            Err(format!("could not create {name}"))
        } else {
            Ok(Self {
                ptr,
                free,
                _api: api,
            })
        }
    }
}
impl Drop for Handle<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.free)(self.ptr);
        }
    }
}
impl Api {
    /// Copies and releases a GLib-owned string vector even on validation failure.
    pub unsafe fn strings(&self, raw: *mut *mut c_char) -> Result<Vec<String>, String> {
        if raw.is_null() {
            return Err("missing string vector".into());
        }
        let result = (|| {
            let mut out = Vec::new();
            for i in 0..4096 {
                let p = *raw.add(i);
                if p.is_null() {
                    return Ok(out);
                }
                let s = CStr::from_ptr(p)
                    .to_str()
                    .map_err(|_| "non-UTF8 service/interface name")?;
                if s.len() > 1024 {
                    return Err("oversized service/interface name".into());
                }
                out.push(s.to_owned());
            }
            Err("too many service/interface names".into())
        })();
        (self.g_strfreev)(raw);
        result
    }
    pub fn reader<'a>(&'a self, reply: &'a Handle<'a>) -> ReplyReader<'a> {
        let mut raw = RawReader {
            data: [ptr::null(); 6],
        };
        unsafe {
            (self.gbinder_remote_reply_init_reader)(reply.ptr, &mut raw);
        }
        ReplyReader {
            raw,
            api: self,
            _reply: reply,
        }
    }
}
pub struct ReplyReader<'a> {
    raw: RawReader,
    api: &'a Api,
    _reply: &'a Handle<'a>,
}
impl ReplyReader<'_> {
    pub fn strings(&mut self) -> Result<Vec<String>, String> {
        unsafe {
            self.api
                .strings((self.api.gbinder_reader_read_hidl_string_vec)(
                    &mut self.raw,
                ))
        }
    }
}
impl crate::decode::Reader for ReplyReader<'_> {
    fn word(&mut self) -> Result<u32, String> {
        let mut v = 0;
        if unsafe { (self.api.gbinder_reader_read_uint32)(&mut self.raw, &mut v) } == 0 {
            Err("missing reply word".into())
        } else {
            Ok(v)
        }
    }
    fn color(&mut self) -> Result<[u8; 20], String> {
        let p = unsafe { (self.api.gbinder_reader_read_hidl_struct1)(&mut self.raw, 20) };
        if p.is_null() {
            return Err(
                "missing or incorrectly sized ColorCapability buffer (expected 20 bytes)".into(),
            );
        }
        let mut b = [0; 20];
        unsafe {
            ptr::copy_nonoverlapping(p.cast::<u8>(), b.as_mut_ptr(), b.len());
        }
        Ok(b)
    }
    fn at_end(&self) -> bool {
        unsafe { (self.api.gbinder_reader_at_end)(&self.raw) != 0 }
    }
}
