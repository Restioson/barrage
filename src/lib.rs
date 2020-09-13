//! Barrage - an asynchronous broadcast channel. Each message sent will be received by every receiver.
//! When the channel reaches its cap, send operations will block, wait, or fail (depending on which
//! type of send was chosen). Cloned receivers will only receive messages sent after they are cloned.
//!
//! # Example
//!
//! ```rust
//!
//! let (tx, mut rx1) = barrage::unbounded();
//! let mut rx2 = rx1.clone();
//! tx.send("Hello!");
//! let mut rx3 = rx1.clone();
//! assert_eq!(rx1.recv(), Ok("Hello!"));
//! assert_eq!(rx2.recv(), Ok("Hello!"));
//! assert_eq!(rx3.try_recv(), Ok(None));
//! ```

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use concurrent_queue::ConcurrentQueue;
use event_listener::{Event, EventListener};
use std::fmt::Debug;
use facade::sync::atomic::{AtomicUsize, Ordering};
use facade::sync::Arc;
use facade::*;

mod facade;

type ReceiverQueue<T> = ConcurrentQueue<Arc<T>>;

struct Shared<T> {
    receiver_queues: RwLock<Vec<Arc<ReceiverQueue<T>>>>,
    on_final_receive: Event,
    on_send: Event,
    n_receivers: AtomicUsize,
    n_senders: AtomicUsize,
    len: AtomicUsize,
    capacity: Option<usize>,
}

/// All senders have disconnected from the channel and there are no more messages waiting.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

/// The broadcaster side of the channel.
pub struct Sender<T: Clone + Unpin>(Arc<Shared<T>>);

impl<T: Clone + Unpin> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.n_senders.fetch_add(1, Ordering::Relaxed);
        Sender(self.0.clone())
    }
}

impl<T: Clone + Unpin> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.0.n_senders.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }

        self.0.on_send.notify(self.0.n_receivers.load(Ordering::Acquire));
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrySendError<T> {
    Disconnected(T),
    Full(T),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SendError<T>(pub T);

impl<T: Clone + Unpin> Sender<T> {
    /// Try to broadcast a message to all receivers. If the message cap is reached, this will fail.
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        if self.0.n_receivers.load(Ordering::Acquire) == 0 {
            return Err(TrySendError::Disconnected(item));
        }

        let shared = &self.0;
        if shared.capacity.map(|c| c == shared.len.load(Ordering::Acquire)).unwrap_or(false) {
            return Err(TrySendError::Full(item));
        }

        let item = Arc::new(item);

        // This isn't a for loop because idx is only advanced for present queues
        for q in lock_read(&shared.receiver_queues).iter() {
            assert!(q.push(item.clone()).is_ok());
        }

        shared.len.fetch_add(1, Ordering::Release);
        shared.on_send.notify(shared.n_receivers.load(Ordering::Acquire));

        Ok(())
    }

    /// Broadcast a message to all receivers. If the message cap is reached, this will block until
    /// the queue is no longer full.
    pub fn send(&self, mut item: T) -> Result<(), SendError<T>> {
        loop {
            let event_listener = self.0.on_final_receive.listen();
            match self.try_send(item) {
                Ok(()) => break Ok(()),
                Err(TrySendError::Disconnected(item)) => break Err(SendError(item)),
                Err(TrySendError::Full(ret)) => {
                    item = ret;
                    event_listener.wait();
                }
            }
        }
    }

    /// Broadcast a message to all receivers. If the message cap is reached, this will
    /// asynchronously wait until the queue is no longer full.
    pub fn send_async(&self, item: T) -> SendFut<T> {
        SendFut {
            item: Some(item),
            sender: self,
            event_listener: None,
        }
    }
}

/// The future representing an asynchronous broadcast operation.
///
/// # Panics
///
/// This will panic if polled after returning `Poll::Ready`.
pub struct SendFut<'a, T: Clone + Unpin> {
    item: Option<T>,
    sender: &'a Sender<T>,
    event_listener: Option<EventListener>,
}

impl<'a, T: Clone + Unpin> Future for SendFut<'a, T> {
    type Output = Result<(), SendError<T>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let poll = loop {
            let item = self.item.take().expect("cannot poll completed send future");
            let mut listener = self.sender.0.on_final_receive.listen();
            break match self.sender.try_send(item) {
                Ok(()) => Poll::Ready(Ok(())),
                Err(TrySendError::Disconnected(item)) => Poll::Ready(Err(SendError(item))),
                Err(TrySendError::Full(ret)) => {
                    self.item.replace(ret);

                    if let Poll::Ready(_) = Pin::new(&mut listener).poll(cx) {
                        continue;
                    }

                    self.event_listener = Some(listener);
                    Poll::Pending
                }
            }
        };

        poll
    }
}

/// The receiver side of the channel. This will receive every message broadcast.
pub struct Receiver<T: Clone + Unpin> {
    shared: Arc<Shared<T>>,
    queue: Arc<ConcurrentQueue<Arc<T>>>,
}

impl<T: Clone + Unpin> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let queue = Arc::new(ConcurrentQueue::unbounded());
        let mut receiver_queues = lock_write(&self.shared.receiver_queues);
        receiver_queues.push(queue.clone());
        self.shared.n_receivers.fetch_add(1, Ordering::Release);

        Receiver {
            shared: self.shared.clone(),
            queue,
        }
    }
}

impl<T: Clone + Unpin> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut receiver_queues = lock_write(&self.shared.receiver_queues);
        receiver_queues.retain(|other| !Arc::ptr_eq(&self.queue, other));

        if self.shared.n_receivers.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }

        self.shared.on_final_receive.notify(self.shared.n_senders.load(Ordering::Acquire));
    }
}

impl<T: Clone + Unpin> Receiver<T> {
    /// Receive a broadcast message. If there are none in the queue, it will block until another is
    /// sent or all senders disconnect.
    pub fn recv(&mut self) -> Result<T, Disconnected> {
        loop {
            let listener = self.shared.on_send.listen();
            match self.try_recv() {
                Ok(Some(item)) => break Ok(item),
                Ok(None) => listener.wait(),
                Err(_) => break Err(Disconnected),
            }
        }
    }

    /// Try to receive a broadcast message. If there are none in the queue, it will return `None`, or
    /// if there are no senders it will return `Disconnected`.
    pub fn try_recv(&mut self) -> Result<Option<T>, Disconnected> {
        match self.queue.pop() {
            Ok(item) => {
                if Arc::strong_count(&item) == 1 {
                    self.shared.len.fetch_sub(1, Ordering::Release);
                    self.shared.on_final_receive.notify_additional(1);
                }

                Ok(Some((&*item).clone()))
            },
            Err(_) if self.shared.n_senders.load(Ordering::Acquire) > 0 => Ok(None),
            Err(_) => Err(Disconnected),
        }
    }

    /// Receive a broadcast message. If there are none in the queue, it will asynchronously wait
    /// until another is sent or all senders disconnect.
    pub fn recv_async(&mut self) -> RecvFut<T> {
        RecvFut {
            receiver: self,
            event_listener: None,
        }
    }
}

/// The future representing an asynchronous receive operation.
pub struct RecvFut<'a, T: Clone + Unpin> {
    receiver: &'a mut Receiver<T>,
    event_listener: Option<EventListener>,
}

impl<'a, T: Clone + Unpin> Future for RecvFut<'a, T> {
    type Output = Result<T, Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let mut listener = self.receiver.shared.on_send.listen();
            break match self.receiver.try_recv() {
                Ok(Some(item)) => Poll::Ready(Ok(item)),
                Ok(None) => {
                    if let Poll::Ready(_) = Pin::new(&mut listener).poll(cx) {
                        continue;
                    }
                    self.event_listener = Some(listener);
                    Poll::Pending
                },
                Err(_) => Poll::Ready(Err(Disconnected)),
            }
        }
    }
}

/// Create a new channel with the given capacity. If `None` is passed, it will be unbounded.
pub fn new<T: Clone + Unpin>(capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
    let receiver_queue = Arc::new(ConcurrentQueue::unbounded());

    let shared = Shared {
        receiver_queues: RwLock::new(vec![receiver_queue.clone()]),
        on_final_receive: Event::new(),
        on_send: Event::new(),
        n_receivers: AtomicUsize::new(1),
        n_senders: AtomicUsize::new(1),
        len: AtomicUsize::new(0),
        capacity,
    };
    let shared = Arc::new(shared);

    (Sender(shared.clone()), Receiver { shared, queue: receiver_queue })
}

/// Create a bounded channel of the given capacity.
pub fn bounded<T: Clone + Unpin>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    new(Some(capacity))
}

/// Create an unbounded channel.
pub fn unbounded<T: Clone + Unpin>() -> (Sender<T>, Receiver<T>) {
    new(None)
}
