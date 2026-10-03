//! Small checked wrapper around libsodium sealed boxes and guarded key memory.
use crate::Error;
use libsodium_sys as sodium;
use std::{ptr::NonNull, sync::OnceLock};
use zeroize::Zeroizing;

fn init() -> Result<(), Error> {
    static READY: OnceLock<bool> = OnceLock::new();
    if *READY.get_or_init(|| unsafe { sodium::sodium_init() >= 0 }) {
        Ok(())
    } else {
        Err("Crypto initialization failed".into())
    }
}
pub struct Secret {
    memory: NonNull<u8>,
    pub public: [u8; 32],
}
// Allocation belongs solely to Secret. No access outside &self methods; the
// service serializes use, and the pointer never escapes this module.
unsafe impl Send for Secret {}
impl Secret {
    pub fn generate() -> Result<Self, Error> {
        init()?;
        let memory = NonNull::new(unsafe { sodium::sodium_malloc(32) }.cast::<u8>())
            .ok_or("Key allocation failed")?;
        let mut key = Self {
            memory,
            public: [0; 32],
        };
        // sodium_malloc's implicit mlock is best effort; enforce it explicitly.
        if unsafe { sodium::sodium_mlock(key.memory.as_ptr().cast(), 32) } != 0 {
            return Err("Cannot lock secret key memory".into());
        }
        if unsafe { sodium::crypto_box_keypair(key.public.as_mut_ptr(), key.memory.as_ptr()) } != 0
        {
            return Err("Key generation failed".into());
        }
        Ok(key)
    }
    pub fn unseal(&self, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
        if ciphertext.len() < 48 {
            return Err("Invalid sealed box".into());
        }
        let mut plain = Zeroizing::new(vec![0; ciphertext.len() - 48]);
        let rc = unsafe {
            sodium::crypto_box_seal_open(
                plain.as_mut_ptr(),
                ciphertext.as_ptr(),
                ciphertext.len() as u64,
                self.public.as_ptr(),
                self.memory.as_ptr(),
            )
        };
        if rc != 0 {
            return Err("Invalid sealed box".into());
        }
        Ok(plain)
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        // sodium_free wipes, unlocks and releases the guarded allocation.
        unsafe { sodium::sodium_free(self.memory.as_ptr().cast()) }
    }
}
pub fn seal(public: &[u8; 32], plain: &[u8]) -> Result<Vec<u8>, Error> {
    init()?;
    let mut out = vec![0; plain.len() + 48];
    if unsafe {
        sodium::crypto_box_seal(
            out.as_mut_ptr(),
            plain.as_ptr(),
            plain.len() as u64,
            public.as_ptr(),
        )
    } != 0
    {
        return Err("Encryption failed".into());
    }
    Ok(out)
}
