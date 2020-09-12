mod facade;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use concurrent_queue::ConcurrentQueue;
use event_listener::{Event, EventListener};
use std::fmt::Debug;

use facade::sync::atomic::{AtomicUsize, Ordering};
use facade::sync::Arc;
use facade::*;

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

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

pub struct Sender<T: Clone + Unpin>(Arc<Shared<T>>);

impl<T: Clone + Unpin> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let x = self.0.n_senders.fetch_add(1, Ordering::SeqCst); // TODO(loom)
        println!("n_senders += 1 (now is {})", x + 1);
        Sender(self.0.clone())
    }
}

impl<T: Clone + Unpin> Drop for Sender<T> {
    fn drop(&mut self) {
        println!("Tx drop");
        if self.0.n_senders.fetch_sub(1, Ordering::SeqCst) != 1 { // TODO(loom)
            return;
        }
        self.0.on_send.notify(self.0.n_receivers.load(Ordering::SeqCst));
        println!(
            " -> Notifying {} receivers. n_senders = {}",
            self.0.n_receivers.load(Ordering::SeqCst),
            self.0.n_senders.load(Ordering::SeqCst)
        );
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

        shared.len.fetch_add(1, Ordering::Acquire);
        shared.on_send.notify_additional(shared.n_receivers.load(Ordering::Acquire));

        Ok(())
    }

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

    pub fn send_async(&self, item: T) -> SendFut<T> {
        SendFut {
            item: Some(item),
            sender: self,
            event_listener: None,
        }
    }
}

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
        match self.sender.try_send(self.item.take().expect("send future is finished")) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(TrySendError::Disconnected(item)) => Poll::Ready(Err(SendError(item))),
            Err(TrySendError::Full(ret)) => {
                let mut listener = self.sender.0.on_final_receive.listen();
                assert_eq!(Pin::new(&mut listener).poll(cx), Poll::Pending);
                self.event_listener = Some(listener);
                self.item.replace(ret);
                Poll::Pending
            }
        }
    }
}

pub struct Receiver<T: Clone + Unpin> {
    shared: Arc<Shared<T>>,
    queue: Arc<ConcurrentQueue<Arc<T>>>,
}

impl<T: Clone + Unpin> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        let queue = Arc::new(ConcurrentQueue::unbounded());
        let mut receiver_queues = lock_write(&self.shared.receiver_queues);
        receiver_queues.push(queue.clone());
        self.shared.n_receivers.fetch_add(1, Ordering::Relaxed);

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

        self.shared.on_final_receive.notify_additional(self.shared.n_senders.load(Ordering::Acquire));
    }
}

impl<T: Clone + Unpin> Receiver<T> {
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

    pub fn try_recv(&mut self) -> Result<Option<T>, Disconnected> {
        match self.queue.pop() {
            Ok(item) => {
                if Arc::strong_count(&item) == 1 {
                    self.shared.len.fetch_sub(1, Ordering::Release);
                    self.shared.on_final_receive.notify_additional(1);
                }

                Ok(Some((&*item).clone()))
            },
            Err(_) if self.shared.n_senders.load(Ordering::SeqCst) > 0 => { // TODO(loom)
                println!(" -> n_senders = {}", self.shared.n_senders.load(Ordering::SeqCst));
                Ok(None)
            },
            Err(_) => {
                println!("Disconnected");
                Err(Disconnected)
            },
        }
    }

    pub fn recv_async(&mut self) -> RecvFut<T> {
        RecvFut {
            receiver: self,
            event_listener: None,
        }
    }
}

pub struct RecvFut<'a, T: Clone + Unpin> {
    receiver: &'a mut Receiver<T>,
    event_listener: Option<EventListener>,
}

impl<'a, T: Clone + Unpin> Future for RecvFut<'a, T> {
    type Output = Result<T, Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        println!("Rx poll");
        let mut listener = self.receiver.shared.on_send.listen();
        match self.receiver.try_recv() {
            Ok(Some(item)) => {
                println!(" -> got item");
                Poll::Ready(Ok(item))
            },
            Ok(None) => {
                println!(" -> waiting");
                println!(" -> poll is pending: {}", Pin::new(&mut listener).poll(cx).is_pending());
                self.event_listener = Some(listener);
                Poll::Pending
            },
            Err(_) => {
                println!(" -> disconnected");
                Poll::Ready(Err(Disconnected))
            },
        }
    }
}

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

pub fn bounded<T: Clone + Unpin>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    new(Some(capacity))
}

pub fn unbounded<T: Clone + Unpin>() -> (Sender<T>, Receiver<T>) {
    new(None)
}
