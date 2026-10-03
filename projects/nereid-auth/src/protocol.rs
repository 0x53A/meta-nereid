//! Sealed boxes, fixed-size plaintext, caller-bound single-use recipient keys.
use crate::sodium::Secret;
use crate::Error;
use rand_core::{OsRng, RngCore};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const TTL: Duration = Duration::from_secs(30);
const PLAIN: usize = 64;
const SEALED: usize = PLAIN + 48;

pub fn valid_pin(pin: &[u8]) -> bool {
    (4..=12).contains(&pin.len()) && pin.iter().all(u8::is_ascii_digit)
}
pub fn seal(id: &[u8], public: &[u8], pin: &[u8]) -> Result<Vec<u8>, Error> {
    if id.len() != 32 || !valid_pin(pin) {
        return Err("Invalid PIN envelope".into());
    }
    let public: [u8; 32] = public.try_into().map_err(|_| "Invalid public key")?;
    let mut payload = Zeroizing::new([0u8; PLAIN]);
    payload[0] = 1;
    payload[1..33].copy_from_slice(id);
    payload[33] = pin.len() as u8;
    payload[34..34 + pin.len()].copy_from_slice(pin);
    crate::sodium::seal(&public, payload.as_ref())
}

/// One fixed-size encrypted management request; action and both PINs are sealed.
pub fn seal_management(
    id: &[u8],
    public: &[u8],
    current: &[u8],
    new_pin: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    if id.len() != 32 || !valid_pin(current) || new_pin.is_some_and(|p| !valid_pin(p)) {
        return Err("Invalid management envelope".into());
    }
    let public: [u8; 32] = public.try_into().map_err(|_| "Invalid public key")?;
    let mut payload = Zeroizing::new([0u8; PLAIN]);
    payload[0] = 2;
    payload[1..33].copy_from_slice(id);
    payload[33] = if new_pin.is_some() { 1 } else { 2 };
    payload[34] = current.len() as u8;
    payload[35] = new_pin.map_or(0, |p| p.len()) as u8;
    payload[36..36 + current.len()].copy_from_slice(current);
    if let Some(new) = new_pin {
        payload[36 + current.len()..36 + current.len() + new.len()].copy_from_slice(new);
    }
    crate::sodium::seal(&public, payload.as_ref())
}

pub struct ManagementRequest {
    pub current: Zeroizing<Vec<u8>>,
    pub new_pin: Option<Zeroizing<Vec<u8>>>,
}

// No Debug/Clone: do not accidentally copy or print secret keys.
struct Pending {
    owner: String,
    id: [u8; 32],
    secret: Secret,
    expires: Instant,
    management: bool,
}
#[derive(Default)]
pub struct Exchange {
    pending: Option<Pending>,
}
impl Exchange {
    pub fn expire(&mut self, now: Instant) {
        if self.pending.as_ref().is_some_and(|p| now >= p.expires) {
            self.pending = None;
        }
    }
    pub fn clear(&mut self) {
        self.pending = None;
    }
    pub fn begin(&mut self, owner: &str, now: Instant) -> Result<(Vec<u8>, Vec<u8>), Error> {
        self.begin_for(owner, now, false)
    }
    pub fn begin_management(
        &mut self,
        owner: &str,
        now: Instant,
    ) -> Result<(Vec<u8>, Vec<u8>), Error> {
        self.begin_for(owner, now, true)
    }
    fn begin_for(
        &mut self,
        owner: &str,
        now: Instant,
        management: bool,
    ) -> Result<(Vec<u8>, Vec<u8>), Error> {
        self.expire(now);
        if self.pending.as_ref().is_some_and(|p| p.owner != owner) {
            return Err("Attempt in progress".into());
        }
        // Replacing an unused attempt destroys its old key too.
        self.clear();
        let secret = Secret::generate()?;
        let public = secret.public.to_vec();
        let mut id = [0; 32];
        OsRng.fill_bytes(&mut id);
        self.pending = Some(Pending {
            owner: owner.into(),
            id,
            secret,
            expires: now + TTL,
            management,
        });
        Ok((id.to_vec(), public))
    }
    fn open_payload(
        &mut self,
        owner: &str,
        id: &[u8],
        ciphertext: &[u8],
        now: Instant,
        management: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.expire(now);
        let p = self.pending.as_ref().ok_or("No live attempt")?;
        if p.owner != owner || id != p.id {
            return Err("Unknown attempt".into());
        }
        // Consume BEFORE validating/decrypting: a malformed submission burns the key.
        let p = self.pending.take().unwrap();
        if ciphertext.len() != SEALED || p.management != management {
            return Err("Invalid envelope length".into());
        }
        let opened = p.secret.unseal(ciphertext);
        drop(p); // Secret zeroizes its guarded allocation on drop, before handing PIN to the backend.
        let payload = opened.map_err(|_| "Invalid encrypted envelope")?;
        if payload.len() != PLAIN
            || payload[0] != if management { 2 } else { 1 }
            || &payload[1..33] != id
        {
            return Err("Invalid envelope".into());
        }
        Ok(payload)
    }
    pub fn open(
        &mut self,
        owner: &str,
        id: &[u8],
        ciphertext: &[u8],
        now: Instant,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let payload = self.open_payload(owner, id, ciphertext, now, false)?;
        let length = payload[33] as usize;
        if !(4..=12).contains(&length) || payload[34 + length..].iter().any(|b| *b != 0) {
            return Err("Invalid PIN envelope".into());
        }
        let pin = &payload[34..34 + length];
        if !valid_pin(pin) {
            return Err("Invalid PIN".into());
        }
        Ok(Zeroizing::new(pin.to_vec()))
    }
    pub fn open_management(
        &mut self,
        owner: &str,
        id: &[u8],
        ciphertext: &[u8],
        now: Instant,
    ) -> Result<ManagementRequest, Error> {
        let payload = self.open_payload(owner, id, ciphertext, now, true)?;
        let current_len = payload[34] as usize;
        let new_len = payload[35] as usize;
        if !(4..=12).contains(&current_len) || new_len > 12 || 36 + current_len + new_len > PLAIN {
            return Err("Invalid management PIN length".into());
        }
        let current = &payload[36..36 + current_len];
        let new = &payload[36 + current_len..36 + current_len + new_len];
        if !valid_pin(current)
            || payload[36 + current_len + new_len..]
                .iter()
                .any(|b| *b != 0)
        {
            return Err("Invalid management payload".into());
        }
        let new_pin = match payload[33] {
            1 if valid_pin(new) => Some(Zeroizing::new(new.to_vec())),
            2 if new.is_empty() => None,
            _ => return Err("Invalid management operation".into()),
        };
        Ok(ManagementRequest {
            current: Zeroizing::new(current.to_vec()),
            new_pin,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_roundtrip_binding_and_single_use() {
        let now = Instant::now();
        for new in [None, Some(b"987654321098".as_slice())] {
            let mut e = Exchange::default();
            let (id, pk) = e.begin_management(":1.1", now).unwrap();
            let c = seal_management(&id, &pk, b"123456789012", new).unwrap();
            assert_eq!(c.len(), SEALED);
            assert!(e.open_management(":1.2", &id, &c, now).is_err());
            let request = e.open_management(":1.1", &id, &c, now).unwrap();
            assert_eq!(request.current.as_slice(), b"123456789012");
            assert_eq!(request.new_pin.as_ref().map(|p| p.as_slice()), new);
            assert!(e.open_management(":1.1", &id, &c, now).is_err());
        }
    }
    #[test]
    fn management_cannot_repurpose_verification_attempts() {
        let now = Instant::now();
        let mut e = Exchange::default();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        let c = seal_management(&id, &pk, b"123456", None).unwrap();
        assert!(e.open_management(":1.1", &id, &c, now).is_err());
        assert!(e.pending.is_none());
        let (id, pk) = e.begin_management(":1.1", now).unwrap();
        let c = seal(&id, &pk, b"123456").unwrap();
        assert!(e.open(":1.1", &id, &c, now).is_err());
        assert!(e.pending.is_none());
    }
    #[test]
    fn malformed_management_payload_burns_key() {
        let now = Instant::now();
        for (offset, value) in [(33, 3), (34, 255), (35, 1), (63, 1)] {
            let mut e = Exchange::default();
            let (id, pk) = e.begin_management(":1.1", now).unwrap();
            let mut plain = [0u8; PLAIN];
            plain[0] = 2;
            plain[1..33].copy_from_slice(&id);
            plain[33] = 2;
            plain[34] = 4;
            plain[36..40].copy_from_slice(b"2468");
            plain[offset] = value;
            let public: [u8; 32] = pk.try_into().unwrap();
            let c = crate::sodium::seal(&public, &plain).unwrap();
            assert!(e.open_management(":1.1", &id, &c, now).is_err());
            assert!(e.pending.is_none());
        }
    }
    #[test]
    fn roundtrip_consumes_and_rotates() {
        let mut e = Exchange::default();
        let now = Instant::now();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        let c = seal(&id, &pk, b"123456").unwrap();
        assert_eq!(c.len(), SEALED);
        assert_eq!(&*e.open(":1.1", &id, &c, now).unwrap(), b"123456");
        assert!(e.open(":1.1", &id, &c, now).is_err());
        let (_, pk2) = e.begin(":1.1", now).unwrap();
        assert_ne!(pk, pk2);
    }
    #[test]
    fn tampering_consumes_key() {
        let mut e = Exchange::default();
        let now = Instant::now();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        let good = seal(&id, &pk, b"123456").unwrap();
        let mut bad = good.clone();
        bad[60] ^= 1;
        assert!(e.open(":1.1", &id, &bad, now).is_err());
        assert!(e.open(":1.1", &id, &good, now).is_err());
    }
    #[test]
    fn wrong_sender_cannot_consume_and_expiry_destroys() {
        let mut e = Exchange::default();
        let now = Instant::now();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        let c = seal(&id, &pk, b"123456").unwrap();
        assert!(e.open(":1.2", &id, &c, now).is_err());
        assert!(e.begin(":1.2", now).is_err());
        assert!(e.open(":1.1", &id, &c, now + TTL).is_err());
        assert!(e.begin(":1.2", now + TTL).is_ok());
    }
    #[test]
    fn replaced_attempt_and_other_instance_fail() {
        let mut e = Exchange::default();
        let now = Instant::now();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        let c = seal(&id, &pk, b"123456").unwrap();
        e.begin(":1.1", now).unwrap();
        assert!(e.open(":1.1", &id, &c, now).is_err());
        assert!(Exchange::default().open(":1.1", &id, &c, now).is_err());
    }
    #[test]
    fn malformed_length_burns_key_and_pin_limits() {
        let mut e = Exchange::default();
        let now = Instant::now();
        let (id, pk) = e.begin(":1.1", now).unwrap();
        assert!(seal(&id, &pk, b"123").is_err());
        assert!(seal(&id, &pk, b"12345x").is_err());
        assert!(e.open(":1.1", &id, &[], now).is_err());
        assert!(e.pending.is_none());
    }
}
