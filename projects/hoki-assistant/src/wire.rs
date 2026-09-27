//! One ordered RFCOMM session. No retries of an utterance after submission.
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub const UUID: &str = "494ff010-8ff4-4da7-9c56-ead025905e53";
pub const HELLO: u8 = 1;
pub const READY: u8 = 2;
pub const PCM: u8 = 3;
pub const END: u8 = 4;
// Phone also accepts explicit cancellation; this client closes the connection.
#[allow(dead_code)]
pub const CANCEL: u8 = 5;
pub const STATE: u8 = 6;
pub const AUDIO_FORMAT: u8 = 7;
pub const AUDIO: u8 = 8;
pub const DONE: u8 = 9;
pub const ERROR: u8 = 10;
pub const MAX: usize = 65536;
pub async fn read(r: &mut (impl AsyncRead + Unpin)) -> io::Result<(u8, Vec<u8>)> {
    let kind = r.read_u8().await?;
    let len = r.read_u32().await? as usize;
    if !(HELLO..=ERROR).contains(&kind) || len > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid assistant frame",
        ));
    }
    let mut data = vec![0; len];
    r.read_exact(&mut data).await?;
    Ok((kind, data))
}
pub async fn write(w: &mut (impl AsyncWrite + Unpin), kind: u8, data: &[u8]) -> io::Result<()> {
    if data.len() > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Frame too large",
        ));
    }
    w.write_u8(kind).await?;
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await?;
    w.flush().await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn fragmented_frame_and_eof() {
        let (mut a, mut b) = tokio::io::duplex(3);
        let task = tokio::spawn(async move {
            write(&mut a, PCM, &[1, 2, 3, 4]).await.unwrap();
        });
        assert_eq!(read(&mut b).await.unwrap(), (PCM, vec![1, 2, 3, 4]));
        task.await.unwrap();
        assert!(read(&mut b).await.is_err());
    }
    #[tokio::test]
    async fn refuses_oversized_length_before_allocation() {
        let bytes = [PCM, 0, 1, 0, 1];
        assert!(read(&mut bytes.as_slice()).await.is_err());
    }
}
