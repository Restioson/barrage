use spinning_top::{Spinlock, SpinlockGuard};
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;
use concurrent_queue::ConcurrentQueue;
use event_listener::{Event, EventListener};
use std::fmt::Debug;

#[cfg(not(windows))]
type ChannelLock<T> = Spinlock<T>;
#[cfg(not(windows))]
type ChannelGuard<'a, T> = SpinlockGuard<'a, T>;

#[cfg(windows)]
type ChannelLock<T> = std::sync::Mutex<T>;
#[cfg(windows)]
type ChannelGuard<'a, T> = std::sync::MutexGuard<'a, T>;

#[cfg(not(windows))]
fn wait_lock<T>(lock: &ChannelLock<T>) -> ChannelGuard<'_, T> {
    let mut i = 4;
    loop {
        for _ in 0..10 {
            if let Some(guard) = lock.try_lock() {
                return guard;
            }
            thread::yield_now();
        }
        thread::sleep(Duration::from_nanos(1 << i));
        i += 1;
    }
}

#[cfg(windows)]
fn wait_lock<T>(lock: &ChannelLock<T>) -> ChannelGuard<'_, T> {
    lock.lock()
}

struct Inner<T> {
    items: VecDeque<Arc<T>>,
    receiver_queues: Vec<Weak<ConcurrentQueue<Arc<T>>>>,
}

struct Shared<T> {
    inner: ChannelLock<Inner<T>>,
    on_final_receive: Event,
    on_send: Event,
    n_receivers: AtomicUsize,
    n_senders: AtomicUsize,
    capacity: Option<usize>,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct Disconnected;

pub struct Sender<T: Clone + Unpin>(Arc<Shared<T>>);

impl<T: Clone + Unpin> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.n_senders.fetch_add(1,Ordering::AcqRel);
        Sender(self.0.clone())
    }
}

impl<T: Clone + Unpin> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.0.n_senders.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.on_send.notify(self.0.n_receivers.load(Ordering::Acquire));
        }
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
        let mut channel = wait_lock(&shared.inner);

        if shared.capacity.map(|c| c == channel.items.len()).unwrap_or(false) {
            return Err(TrySendError::Full(item));
        }

        let item = Arc::new(item);

        // This isn't a for loop because idx is only advanced for present queues
        let mut idx = 0;
        while idx < channel.receiver_queues.len() {
            let q = match channel.receiver_queues.get(idx) {
                Some(q) => q,
                None => break,
            };

            match q.upgrade() {
                Some(q) => {
                    let _ = q.push(item.clone());
                    idx += 1;
                }
                None => {
                    channel.receiver_queues.remove(idx);
                }
            }
        }

        channel.items.push_back(item);
        shared.on_send.notify(shared.n_receivers.load(Ordering::Acquire));

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
        let mut inner = wait_lock(&self.shared.inner);
        inner.receiver_queues.push(Arc::downgrade(&queue));
        self.shared.n_receivers.fetch_add(1, Ordering::AcqRel);

        Receiver {
            shared: self.shared.clone(),
            queue,
        }
    }
}

impl<T: Clone + Unpin> Drop for Receiver<T> {
    fn drop(&mut self) {
        if self.shared.n_receivers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shared.on_final_receive.notify(self.shared.n_senders.load(Ordering::Acquire));
        }
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
                // 2 because this arc and the item queue arc
                if Arc::strong_count(&item) == 2 {
                    self.remove_item();
                }

                Ok(Some((&*item).clone()))
            },
            Err(_) if self.shared.n_senders.load(Ordering::Acquire) > 0 => Ok(None),
            Err(_) => Err(Disconnected),
        }
    }

    pub fn recv_async(&mut self) -> RecvFut<T> {
        RecvFut {
            receiver: self,
            event_listener: None,
        }
    }

    fn remove_item(&mut self) {
        let mut inner = wait_lock(&self.shared.inner);
        inner.items.pop_front();
        self.shared.on_final_receive.notify(1);
    }
}

pub struct RecvFut<'a, T: Clone + Unpin> {
    receiver: &'a mut Receiver<T>,
    event_listener: Option<EventListener>,
}

impl<'a, T: Clone + Unpin> Future for RecvFut<'a, T> {
    type Output = Result<T, Disconnected>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.receiver.try_recv() {
            Ok(Some(item)) => Poll::Ready(Ok(item)),
            Ok(None) => {
                let mut listener = self.receiver.shared.on_send.listen();
                assert_eq!(Pin::new(&mut listener).poll(cx), Poll::Pending);
                self.event_listener = Some(listener);
                Poll::Pending
            },
            Err(_) => Poll::Ready(Err(Disconnected)),
        }
    }
}

pub fn new<T: Clone + Unpin>(capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
    let receiver_queue = Arc::new(ConcurrentQueue::unbounded());

    let inner = Inner {
        items: VecDeque::new(),
        receiver_queues: vec![Arc::downgrade(&receiver_queue)],
    };

    let shared = Shared {
        inner: ChannelLock::new(inner),
        on_final_receive: Event::new(),
        on_send: Event::new(),
        n_receivers: AtomicUsize::new(1),
        n_senders: AtomicUsize::new(1),
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
