use bluer::{gatt::local::ReqError, Address};
use std::io;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

pub struct Write {
    pub peer: Address,
    pub mtu: usize,
    pub id: u64,
    pub data: Vec<u8>,
    pub complete: oneshot::Sender<Result<(), ReqError>>,
}

// Admission happens before the bounded queue, so a rejected client cannot
// consume the active client's queue capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    Idle,
    Pending(Address),
    Active(Address),
    Classic,
}
impl Gate {
    pub fn reserve(&mut self, peer: Address) -> bool {
        match *self {
            Self::Idle => {
                *self = Self::Pending(peer);
                true
            }
            Self::Pending(owner) => owner == peer,
            _ => false,
        }
    }
    pub fn enqueue(
        &mut self,
        sender: &mpsc::Sender<Write>,
        request: Write,
    ) -> Result<(), ReqError> {
        let previous = *self;
        let allowed = match *self {
            Self::Idle => request.id == 0 && request.data.is_empty() && self.reserve(request.peer),
            Self::Pending(peer) => {
                peer == request.peer && request.id == 0 && request.data.is_empty()
            }
            Self::Active(peer) => peer == request.peer && request.id != 0,
            Self::Classic => false,
        };
        if !allowed {
            return Err(ReqError::InProgress);
        }
        if sender.try_send(request).is_err() {
            *self = previous;
            return Err(ReqError::InProgress);
        }
        Ok(())
    }
}

// Reply to each ATT request only after its bytes have reached the local TCP
// socket. Pending callbacks and data occupy a bounded single-slot queue.
pub async fn forward<W: AsyncWrite + Unpin>(
    peer: Address,
    receiver: &mut mpsc::Receiver<Write>,
    mut writer: W,
) -> io::Result<()> {
    let mut next_id = 1u64;
    while let Some(request) = receiver.recv().await {
        if request.peer != peer || request.id == 0 {
            let _ = request.complete.send(Err(ReqError::InProgress));
            continue;
        }
        if request.id != next_id {
            let _ = request.complete.send(Err(ReqError::NotPermitted));
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected BLE request sequence",
            ));
        }
        let eof = request.data.is_empty();
        if let Err(error) = writer.write_all(&request.data).await {
            let _ = request.complete.send(Err(ReqError::Failed));
            return Err(error);
        }
        let _ = request.complete.send(Ok(()));
        if eof {
            return Ok(());
        }
        next_id = next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("BLE sequence exhausted"))?;
    }
    Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "BLE request channel closed",
    ))
}

// While classic transport owns the single SSH slot, reject BLE starts promptly.
pub async fn reject_busy(receiver: &mut mpsc::Receiver<Write>) -> io::Result<()> {
    while let Some(request) = receiver.recv().await {
        let _ = request.complete.send(Err(ReqError::InProgress));
    }
    Err(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "BLE request channel closed",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn packet(id: u64, data: &[u8]) -> (Write, oneshot::Receiver<Result<(), ReqError>>) {
        let (complete, reply) = oneshot::channel();
        (
            Write {
                peer: Address::any(),
                mtu: 23,
                id,
                data: data.into(),
                complete,
            },
            reply,
        )
    }

    #[tokio::test]
    async fn tcp_backpressure_delays_write_response_and_preserves_bytes() {
        let (sender, mut receiver) = mpsc::channel(1);
        let (writer, mut reader) = tokio::io::duplex(1);
        let worker = forward(Address::any(), &mut receiver, writer);
        let exercise = async {
            let (request, mut reply) = packet(1, b"abc");
            sender.send(request).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), &mut reply)
                    .await
                    .is_err()
            );
            let mut result = [0; 3];
            reader.read_exact(&mut result).await.unwrap();
            assert_eq!(&result, b"abc");
            reply.await.unwrap().unwrap();
            let (request, reply) = packet(2, b"");
            sender.send(request).await.unwrap();
            reply.await.unwrap().unwrap();
        };
        let (result, ()) = tokio::join!(worker, exercise);
        result.unwrap();
    }

    #[tokio::test]
    async fn foreign_ingress_cannot_fill_active_clients_queue() {
        let (sender, mut receiver) = mpsc::channel(1);
        let mut gate = Gate::Active(Address::any());
        let (mut foreign, _) = packet(0, b"");
        foreign.peer = "01:02:03:04:05:06".parse().unwrap();
        assert!(gate.enqueue(&sender, foreign).is_err());
        let (current, _) = packet(1, b"current");
        gate.enqueue(&sender, current).unwrap();
        assert_eq!(receiver.recv().await.unwrap().data, b"current");
        assert_eq!(gate, Gate::Active(Address::any()));
        let (duplicate_start, _) = packet(0, b"");
        assert!(gate.enqueue(&sender, duplicate_start).is_err());
    }

    #[test]
    fn first_pending_peer_owns_admission_and_classic_rejects_ble() {
        let (sender, _receiver) = mpsc::channel(1);
        let mut gate = Gate::Idle;
        let (start, _) = packet(0, b"");
        gate.enqueue(&sender, start).unwrap();
        assert!(gate.reserve(Address::any()));
        assert!(!gate.reserve("01:02:03:04:05:06".parse().unwrap()));
        gate = Gate::Classic;
        let (start, _) = packet(0, b"");
        assert!(gate.enqueue(&sender, start).is_err());
    }

    #[tokio::test]
    async fn overlapping_client_cannot_interrupt_current_peer() {
        let (sender, mut receiver) = mpsc::channel(1);
        let mut output = Vec::new();
        let worker = forward(Address::any(), &mut receiver, &mut output);
        let exercise = async {
            let (mut foreign, response) = packet(0, b"");
            foreign.peer = "01:02:03:04:05:06".parse().unwrap();
            sender.send(foreign).await.unwrap();
            assert!(response.await.unwrap().is_err());
            let (data, response) = packet(1, b"still connected");
            sender.send(data).await.unwrap();
            response.await.unwrap().unwrap();
            let (end, response) = packet(2, b"");
            sender.send(end).await.unwrap();
            response.await.unwrap().unwrap();
        };
        let (result, ()) = tokio::join!(worker, exercise);
        result.unwrap();
        assert_eq!(output, b"still connected");
    }

    #[tokio::test]
    async fn rejects_out_of_sequence_data() {
        let (sender, mut receiver) = mpsc::channel(1);
        let (request, reply) = packet(2, b"unexpected");
        sender.send(request).await.unwrap();
        assert!(forward(Address::any(), &mut receiver, tokio::io::sink())
            .await
            .is_err());
        assert!(reply.await.unwrap().is_err());
    }
}
