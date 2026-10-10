//! A client's outgoing messages, written by a thread of its own so a client that stops reading can't stall the
//! server. Bounded, so it can't grow without end either: past the limit messages are dropped until the queue
//! drains, and then the client needs everything resent.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// a client that won't take its last messages within this is dropped anyway
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct Outbox {
    tx: Sender<Arc<Vec<u8>>>,
    /// bytes queued and not yet written
    queued: Arc<AtomicUsize>,
    behind: Arc<AtomicBool>,
    limit: usize,
    drained: Arc<dyn Fn() + Send + Sync>,
    stream: UnixStream,
    writer: Option<JoinHandle<()>>,
}

impl Outbox {
    /// Writes to `stream` on a thread. Once behind, `drained` is called when the queue empties.
    pub fn new(stream: &UnixStream, limit: usize, drained: impl Fn() + Send + Sync + 'static) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Arc<Vec<u8>>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let behind = Arc::new(AtomicBool::new(false));
        let mut socket = stream.try_clone()?;
        let drained: Arc<dyn Fn() + Send + Sync> = Arc::new(drained);
        let (q, b, d) = (queued.clone(), behind.clone(), drained.clone());
        // ends once the outbox is dropped and its queue written
        let writer = thread::spawn(move || {
            for msg in rx {
                if socket.write_all(&msg).is_err() {
                    break;
                }
                if q.fetch_sub(msg.len(), Ordering::SeqCst) == msg.len() && b.load(Ordering::SeqCst) {
                    d();
                }
            }
            let _ = socket.shutdown(Shutdown::Both);
        });
        Ok(Self { tx, queued, behind, limit, drained, stream: stream.try_clone()?, writer: Some(writer) })
    }

    /// Queues `msg`, unless the client is behind or this would put it there. Returns whether it was queued.
    pub fn send(&self, msg: Arc<Vec<u8>>) -> bool {
        if self.behind() {
            return false;
        }
        if self.queued.load(Ordering::SeqCst) + msg.len() > self.limit {
            self.behind.store(true, Ordering::SeqCst);
            // the writer may have emptied the queue before it could see `behind`
            if self.queued.load(Ordering::SeqCst) == 0 {
                (self.drained)();
            }
            return false;
        }
        self.force(msg);
        true
    }

    /// Queues `msg` past the limit, for what must arrive: the resend, and goodbyes.
    pub fn force(&self, msg: Arc<Vec<u8>>) {
        self.queued.fetch_add(msg.len(), Ordering::SeqCst);
        let _ = self.tx.send(msg);
    }

    pub fn behind(&self) -> bool {
        self.behind.load(Ordering::SeqCst)
    }

    pub fn caught_up(&self) {
        self.behind.store(false, Ordering::SeqCst);
    }

    /// Lets the writer finish what's queued, but not wait forever on a client that stopped reading.
    pub fn close(mut self) {
        let writer = self.writer.take();
        drop(self);
        if let Some(w) = writer {
            let _ = w.join();
        }
    }
}

impl Drop for Outbox {
    fn drop(&mut self) {
        let _ = self.stream.set_write_timeout(Some(CLOSE_TIMEOUT));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn drops_past_the_limit_until_drained() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let (tx, rx) = mpsc::channel();
        let outbox = Outbox::new(&server, 1 << 20, move || tx.send(()).unwrap()).unwrap();
        let chunk = Arc::new(vec![b'x'; 64 << 10]);
        // the client isn't reading, so the socket fills and then the queue
        let mut sent = 0;
        while outbox.send(chunk.clone()) {
            sent += chunk.len();
        }
        assert!(outbox.behind());
        assert!(!outbox.send(Arc::new(b"small".to_vec())), "dropped while behind");
        outbox.force(Arc::new(b"end".to_vec()));

        let mut read = vec![0; sent + 3];
        client.read_exact(&mut read).unwrap();
        assert!(read.ends_with(b"xend"));
        rx.recv_timeout(Duration::from_secs(5)).expect("told once drained");
        assert!(outbox.behind());
        outbox.caught_up();
        assert!(outbox.send(chunk));
        let reader = thread::spawn(move || client.read_to_end(&mut Vec::new()).unwrap());
        outbox.close();
        assert_eq!(reader.join().unwrap(), 64 << 10, "written, then hung up");
    }
}
