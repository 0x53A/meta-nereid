//! Private helper IPC for authentication-bound volume secrets.
//! No key material is exposed through the public D-Bus interface.
use crate::{backend::run_helper, Error};
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Debug)]
pub(crate) struct AuthenticationRejected;
impl std::fmt::Display for AuthenticationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gatekeeper rejected authentication")
    }
}
impl std::error::Error for AuthenticationRejected {}

pub(crate) const MAX_KEY_BLOB: usize = 4096;
const RECORD_HEADER: usize = 76;
const MAX_RECORD: usize = RECORD_HEADER + MAX_KEY_BLOB;

pub(crate) fn validate_versions(bytes: &[u8]) -> Result<(), Error> {
    let text = std::str::from_utf8(bytes)?;
    let mut values = Vec::new();
    if !text.ends_with('\n') {
        return Err("Incomplete Keymaster configuration".into());
    }
    for line in text.split_terminator('\n') {
        if line.is_empty() || line.len() > 8 || !line.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Invalid Keymaster configuration".into());
        }
        values.push(line.parse::<u32>()?);
    }
    if values.len() != 3
        || values[0] == 0
        || values[0] > 999999
        || !(201501..=209912).contains(&values[1])
        || !(1..=12).contains(&(values[1] % 100))
        || !(20150101..=20991231).contains(&values[2])
        || !(1..=12).contains(&((values[2] / 100) % 100))
        || !(1..=31).contains(&(values[2] % 100))
    {
        return Err("Invalid Keymaster version values".into());
    }
    Ok(())
}

/// Opaque Keymaster key blob, secure user ID, nonce and authenticated ciphertext.
/// This is our disk format, not the Qualcomm request format.
pub(crate) struct WrappedKey(Zeroizing<Vec<u8>>);

impl WrappedKey {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if !(RECORD_HEADER + 1..=MAX_RECORD).contains(&bytes.len()) || &bytes[..4] != b"NKW1" {
            return Err("Invalid wrapped key record".into());
        }
        let blob_len = u32::from_le_bytes(bytes[4..8].try_into()?) as usize;
        let sid = u64::from_le_bytes(bytes[8..16].try_into()?);
        if blob_len == 0
            || blob_len > MAX_KEY_BLOB
            || bytes.len() != RECORD_HEADER + blob_len
            || sid == 0
        {
            return Err("Invalid wrapped key record bounds".into());
        }
        Ok(Self(Zeroizing::new(bytes.to_vec())))
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(crate) struct ProvisionedKey {
    pub(crate) wrapped: WrappedKey,
    pub(crate) secret: Zeroizing<[u8; 32]>,
}

/// PIN, HAT, and clear secret travel only through private inherited pipes.
/// The native helper must complete listener cleanup before its reply is accepted.
fn request(
    helper: &Path,
    operation: u32,
    uid: u32,
    handle: &[u8],
    pin: &[u8],
    wrapped: &[u8],
) -> Result<Zeroizing<Vec<u8>>, Error> {
    if uid == 0
        || handle.is_empty()
        || handle.len() > 1024
        || !(4..=12).contains(&pin.len())
        || !pin.iter().all(u8::is_ascii_digit)
        || wrapped.len() > MAX_RECORD
    {
        return Err("Invalid Keymaster helper input".into());
    }
    let mut frame = Zeroizing::new(Vec::with_capacity(
        20 + handle.len() + pin.len() + wrapped.len(),
    ));
    frame.extend_from_slice(b"NGK2");
    frame.extend_from_slice(&operation.to_le_bytes());
    frame.extend_from_slice(&uid.to_le_bytes());
    frame.extend_from_slice(&(handle.len() as u16).to_le_bytes());
    frame.extend_from_slice(&(pin.len() as u16).to_le_bytes());
    frame.extend_from_slice(&(wrapped.len() as u32).to_le_bytes());
    frame.extend_from_slice(handle);
    frame.extend_from_slice(pin);
    frame.extend_from_slice(wrapped);
    let response = run_helper(helper, &frame, 16 + 32 + MAX_RECORD)?;
    decode_reply(operation, &response)
}

fn decode_reply(operation: u32, response: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    if response.len() < 16
        || &response[..4] != b"NGR2"
        || u32::from_le_bytes(response[4..8].try_into()?) != operation
    {
        return Err("Invalid Keymaster helper reply".into());
    }
    let status = i32::from_le_bytes(response[8..12].try_into()?);
    let length = u32::from_le_bytes(response[12..16].try_into()?) as usize;
    // NGK2 status 1 means Gatekeeper rejected authentication before creating a
    // key or releasing a secret. Raw -30 does not establish the rejection's
    // cause. This IPC value is not a Keymaster error code.
    if status == 1 && length == 0 && response.len() == 16 {
        return Err(Box::new(AuthenticationRejected));
    }
    if response.len() != 16 + length || status != 0 {
        // No generic error is reinterpreted as proof of a wrong PIN. The caller
        // stops on any failure; a native operation may already have run.
        return Err("Keymaster operation failed; stopped".into());
    }
    if (operation == 3 && !(32 + RECORD_HEADER + 1..=32 + MAX_RECORD).contains(&length))
        || (operation == 4 && length != 32)
        || !matches!(operation, 3 | 4)
    {
        return Err("Invalid Keymaster helper payload".into());
    }
    Ok(Zeroizing::new(response[16..].to_vec()))
}

pub(crate) fn provision(
    helper: &Path,
    uid: u32,
    handle: &[u8],
    pin: &[u8],
) -> Result<ProvisionedKey, Error> {
    let reply = request(helper, 3, uid, handle, pin, &[])?;
    Ok(ProvisionedKey {
        secret: Zeroizing::new(reply[..32].try_into()?),
        wrapped: WrappedKey::parse(&reply[32..])?,
    })
}

pub(crate) fn unwrap(
    helper: &Path,
    uid: u32,
    handle: &[u8],
    pin: &[u8],
    wrapped: &WrappedKey,
) -> Result<Zeroizing<[u8; 32]>, Error> {
    let reply = request(helper, 4, uid, handle, pin, wrapped.bytes())?;
    Ok(Zeroizing::new(reply[..].try_into()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_configuration_requires_three_explicit_valid_values() {
        assert!(
            validate_versions(include_bytes!("../deploy/keymaster.conf.hoki-reference")).is_ok()
        );
        for invalid in [
            b"0\n0\n0\n".as_slice(),
            b"90000\n202113\n20211205\n",
            b"90000\n202112\n20211205",
            b"90000\n202112\n20211205\n\n",
        ] {
            assert!(validate_versions(invalid).is_err());
        }
    }

    #[test]
    fn wrapped_record_rejects_truncation_trailing_data_and_zero_sid() {
        let mut record = vec![0; RECORD_HEADER + 3];
        record[..4].copy_from_slice(b"NKW1");
        record[4..8].copy_from_slice(&3u32.to_le_bytes());
        record[8..16].copy_from_slice(&7u64.to_le_bytes());
        assert!(WrappedKey::parse(&record).is_ok());
        assert!(WrappedKey::parse(&record[..record.len() - 1]).is_err());
        record.push(0);
        assert!(WrappedKey::parse(&record).is_err());
        record.pop();
        record[8..16].fill(0);
        assert!(WrappedKey::parse(&record).is_err());
    }

    #[test]
    fn no_secret_is_released_from_error_or_wrong_operation_reply() {
        let mut reply = b"NGR2".to_vec();
        reply.extend_from_slice(&4u32.to_le_bytes());
        reply.extend_from_slice(&0i32.to_le_bytes());
        reply.extend_from_slice(&32u32.to_le_bytes());
        reply.extend_from_slice(&[0x42; 32]);
        assert!(decode_reply(4, &reply).is_ok());
        assert!(decode_reply(3, &reply).is_err());
        reply[8..12].copy_from_slice(&(-26i32).to_le_bytes());
        assert!(decode_reply(4, &reply).is_err());
    }
}
