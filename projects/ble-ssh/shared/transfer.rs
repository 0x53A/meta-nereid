use std::{future::Future, io, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const FRAME_HEADER: usize = 8;

pub fn encode_frame(id: u64, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(FRAME_HEADER + payload.len());
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

pub fn decode_frame(frame: &[u8]) -> io::Result<(u64, &[u8])> {
    if frame.len() < FRAME_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated BLE frame",
        ));
    }
    Ok((
        u64::from_le_bytes(frame[..FRAME_HEADER].try_into().unwrap()),
        &frame[FRAME_HEADER..],
    ))
}

pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

pub fn payload_size(att_mtu: usize) -> io::Result<usize> {
    if att_mtu < 23 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid ATT MTU",
        ));
    }
    Ok((att_mtu - 3).min(512) - FRAME_HEADER)
}

// Only one packet is read and outstanding at a time. The caller must complete
// send only after delivery confirmation, not merely after queuing a signal.
pub async fn confirmed_copy<R, F, Fut>(mut reader: R, payload: usize, mut send: F) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: Future<Output = io::Result<()>>,
{
    if payload == 0 || payload > 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid BLE payload size",
        ));
    }
    let mut buffer = vec![0; payload];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        tokio::time::timeout(DELIVERY_TIMEOUT, send(buffer[..count].to_vec()))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "BLE confirmation timed out"))??;
    }
}

// Both directions remain polled while either writer is backpressured. These
// are transport sessions: EOF/error on either side terminates the whole session.
pub async fn session<A, B>(outgoing: A, incoming: B) -> io::Result<()>
where
    A: Future<Output = io::Result<()>>,
    B: Future<Output = io::Result<()>>,
{
    tokio::select! {
        result = outgoing => result,
        result = incoming => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{duplex, AsyncWriteExt};

    #[tokio::test]
    async fn preserves_bytes_across_mtu_boundaries() {
        let data: Vec<u8> = (0..4097).map(|n| (n % 251) as u8).collect();
        for mtu in [23, 64, 247, 512, 517] {
            let packets = Arc::new(Mutex::new(Vec::new()));
            let captured = packets.clone();
            let size = payload_size(mtu).unwrap();
            confirmed_copy(&data[..], size, move |packet| {
                captured.lock().unwrap().push(packet);
                async { Ok(()) }
            })
            .await
            .unwrap();
            let packets = packets.lock().unwrap();
            assert!(packets.iter().all(|packet| {
                let framed = encode_frame(42, packet);
                assert_eq!(decode_frame(&framed).unwrap(), (42, packet.as_slice()));
                framed.len() <= (mtu - 3).min(512)
            }));
            assert_eq!(packets.concat(), data);
        }
        assert!(payload_size(22).is_err());
    }

    #[tokio::test]
    async fn confirmation_blocks_the_next_packet() {
        let (release, confirmation) = tokio::sync::oneshot::channel();
        let mut confirmation = Some(confirmation);
        let calls = Arc::new(Mutex::new(0));
        let count = calls.clone();
        let transfer = confirmed_copy(&b"abcdefghijklmnopqrstuvwxyz"[..], 20, move |_| {
            *count.lock().unwrap() += 1;
            let gate = confirmation.take();
            async move {
                if let Some(gate) = gate {
                    gate.await.unwrap();
                }
                Ok(())
            }
        });
        tokio::pin!(transfer);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut transfer)
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 1);
        release.send(()).unwrap();
        transfer.await.unwrap();
        assert_eq!(*calls.lock().unwrap(), 2);
    }

    #[tokio::test]
    async fn blocked_output_does_not_block_input() {
        let (mut source, mut outgoing_reader) = duplex(1);
        let (mut blocked_writer, _blocked_reader) = duplex(1);
        let (mut incoming_writer, mut incoming_reader) = duplex(1);
        let (mut destination, mut receiver) = duplex(1);
        let proxy = session(
            async {
                tokio::io::copy(&mut outgoing_reader, &mut blocked_writer)
                    .await
                    .map(|_| ())
            },
            async {
                tokio::io::copy(&mut incoming_reader, &mut destination)
                    .await
                    .map(|_| ())
            },
        );
        let exercise = async {
            source.write_all(b"abc").await.unwrap();
            incoming_writer.write_all(b"z").await.unwrap();
            let mut byte = [0];
            receiver.read_exact(&mut byte).await.unwrap();
            assert_eq!(byte, *b"z");
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                _ = proxy => panic!("session ended prematurely"),
                _ = exercise => {},
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_confirmation_times_out_without_more_reads() {
        let error = confirmed_copy(&b"data"[..], 20, |_| std::future::pending())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn failed_confirmation_stops_the_transfer() {
        let mut calls = 0;
        let error = confirmed_copy(&[0u8; 100][..], 20, |_| {
            calls += 1;
            async { Err(io::Error::new(io::ErrorKind::BrokenPipe, "unsubscribed")) }
        })
        .await
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn eof_and_errors_cancel_the_other_direction() {
        session(async { Ok(()) }, std::future::pending())
            .await
            .unwrap();
        assert!(session(std::future::pending(), async {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        })
        .await
        .is_err());
    }
}
