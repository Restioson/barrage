use barrage::{Disconnected, SendError};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::time::Duration;
use std::sync::Arc;
use std::thread;
use tokio_test::assert_ok;
use std::sync::atomic::{AtomicBool, Ordering};

struct PollOnce<'a, F: Future + Unpin>(&'a mut F);

impl<'a, F: Future + Unpin> Future for PollOnce<'a, F> {
    type Output = Poll<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Pin::new(&mut self.0).poll(cx))
    }
}

#[tokio::test]
async fn one_message_unbounded() {
    let (tx, rx) = barrage::new(None);
    let rx2 = rx.clone();
    tx.send_async("Hello!").await.unwrap();
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx2.recv_async().await, Ok("Hello!"));
}

#[test]
fn shared_recv() {
    let (tx, rx) = barrage::new(None);
    let mut fut1 = rx.recv_async();
    let mut fut2 = rx.recv_async();
    let pin1 = Pin::new(&mut fut1);
    let pin2 = Pin::new(&mut fut2);

    let woken1 = Arc::new(AtomicBool::new(false));
    let woken2 = Arc::new(AtomicBool::new(false));
    let woken1_clone = woken1.clone();
    let woken2_clone = woken2.clone();

    let waker = waker_fn::waker_fn(move || woken1_clone.store(true, Ordering::SeqCst));
    let waker2 = waker_fn::waker_fn(move || woken2_clone.store(true, Ordering::SeqCst));
    assert!(pin1.poll(&mut Context::from_waker(&waker)).is_pending());
    assert!(pin2.poll(&mut Context::from_waker(&waker2)).is_pending());

    tx.send("Hello!").unwrap();

    assert!(woken1.load(Ordering::SeqCst));
    assert!(woken2.load(Ordering::SeqCst));

    let pin1 = Pin::new(&mut fut1);
    let pin2 = Pin::new(&mut fut2);
    assert!(pin1.poll(&mut Context::from_waker(&waker)).is_ready());
    assert!(pin2.poll(&mut Context::from_waker(&waker2)).is_pending());
}

#[tokio::test]
async fn sync_receive_from_wait_async_send() {
    let (tx, rx) = barrage::new(None);

    let handle = tokio::task::spawn_blocking(move || {
        assert_eq!("Hello!", rx.recv().unwrap());
    });

    tokio::time::delay_for(Duration::from_millis(500)).await;
    tx.send_async("Hello!").await.unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn sync_receive_from_wait_try_send() {
    let (tx, rx) = barrage::new(None);

    let handle = tokio::task::spawn_blocking(move || {
        assert_eq!("Hello!", rx.recv().unwrap());
    });

    tokio::time::delay_for(Duration::from_millis(500)).await;
    tx.try_send("Hello!").unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn sync_receive() {
    let (tx, rx) = barrage::new(None);

    tx.send_async("Hello!").await.unwrap();
    assert_eq!("Hello!", rx.recv().unwrap());
}

#[tokio::test]
async fn new_recv_after_send() {
    let (tx, rx) = barrage::new(None);
    tx.send_async("Hello!").await.unwrap();
    let rx2 = rx.clone();
    tx.send_async("Hello 2!").await.unwrap();
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx2.recv_async().await, Ok("Hello 2!"));
}

#[tokio::test]
async fn tx_drop_disconnect() {
    let (tx, rx) = barrage::new(None);
    tx.send_async("Hello!").await.unwrap();
    drop(tx);
    let rx2 = rx.clone();
    assert_eq!(rx2.recv_async().await, Err(Disconnected));
    assert_eq!(rx2.recv_async().await, Err(Disconnected));
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx.recv_async().await, Err(Disconnected));
    assert_eq!(rx.recv_async().await, Err(Disconnected));
}

#[tokio::test]
async fn rx_drop_disconnect() {
    let (tx, rx) = barrage::new(None);
    let _tx2 = tx.clone();
    tx.send_async("Hello!").await.unwrap();
    drop(rx);
    assert_eq!(tx.send_async("Hello!").await, Err(SendError("Hello!")));
    assert_eq!(tx.send_async("Hello!").await, Err(SendError("Hello!")));
}

#[tokio::test]
async fn bounded_wait_resume() {
    let (tx, rx) = barrage::new(Some(1));
    tx.send_async("Hello!").await.unwrap();
    let mut fut = tx.send_async("Hello!");
    assert_eq!(PollOnce(&mut fut).await, Poll::Pending);
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(PollOnce(&mut fut).await, Poll::Ready(Ok(())));
}

#[tokio::test]
async fn try_send_bounded_wait_resume() {
    let (tx, rx) = barrage::new(Some(1));
    tx.try_send("Hello!").unwrap();
    let mut fut = tx.send_async("Hello!");
    assert_eq!(PollOnce(&mut fut).await, Poll::Pending);
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(PollOnce(&mut fut).await, Poll::Ready(Ok(())));
}

#[tokio::test]
async fn sync_send_bounded_wait_resume() {
    let (tx, rx) = barrage::new(Some(1));
    tx.send("Hello!").unwrap();
    let mut fut = tx.send_async("Hello!");
    assert_eq!(PollOnce(&mut fut).await, Poll::Pending);
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(PollOnce(&mut fut).await, Poll::Ready(Ok(())));
}

#[tokio::test(max_threads = 4)]
async fn no_reorder() {
    let (tx, rx) = barrage::new(Some(1000));

    let mut handles = Vec::new();

    for _ in 0..4 {
        let rx = rx.clone();
        let mut cur = 0;

        let task = tokio::spawn(async move {
            while let Ok(n) = rx.recv_async().await {
                assert_eq!(n, cur);
                cur += 1;
            }
        });
        handles.push(task);
    }

    let task = tokio::spawn(async move {
        for i in 0..10_000usize {
            tx.send_async(i).await.unwrap();
        }
    });
    handles.push(task);
    drop(rx);

    for task in handles {
        task.await.unwrap();
    }
}

// -- Tests from loom.rs adapted to run without loom --

#[test]
fn broadcast_send_threaded() {
    let (tx1, rx) = barrage::bounded(2);
    let tx1 = Arc::new(tx1);
    let tx2 = tx1.clone();

    let th1 = thread::spawn(move || {
        assert_ok!(tx1.send("one"));
        assert_ok!(tx1.send("two"));
        assert_ok!(tx1.send("three"));
    });

    let th2 = thread::spawn(move || {
        tokio_test::block_on(async {
            assert_ok!(tx2.send_async("inye").await);
            assert_ok!(tx2.send_async("zimbini").await);
            assert_ok!(tx2.send_async("zintathu").await);
        });
    });

    tokio_test::block_on(async {
        let mut num: usize = 0;
        loop {
            match rx.recv_async().await {
                Ok(_) => num += 1,
                Err(_) => break,
            }
        }
        assert_eq!(num, 6);
    });

    assert_ok!(th1.join());
    assert_ok!(th2.join());
}

#[test]
fn drop_rx() {
    let (tx, rx1) = barrage::bounded(16);
    let rx2 = rx1.clone();

    let th1 = thread::spawn(move || {
        tokio_test::block_on(async {
            let v = assert_ok!(rx1.recv_async().await);
            assert_eq!(v, "one");

            let v = assert_ok!(rx1.recv_async().await);
            assert_eq!(v, "two");

            let v = assert_ok!(rx1.recv_async().await);
            assert_eq!(v, "three");

            assert!(rx1.recv_async().await.is_err());
        });
    });

    let th2 = thread::spawn(move || {
        drop(rx2);
    });

    assert_ok!(tx.send("one"));
    assert_ok!(tx.send("two"));
    assert_ok!(tx.send("three"));
    drop(tx);

    assert_ok!(th1.join());
    assert_ok!(th2.join());
}

#[test]
fn shared_receiver_receives_once() {
    let (tx, rx) = barrage::unbounded();
    let shared1 = rx.clone().into_shared();
    let shared2 = shared1.clone();

    tx.try_send("Hello!").unwrap();
    assert_eq!(shared1.try_recv(), Ok(Some("Hello!")));
    assert_eq!(shared2.try_recv(), Ok(None));
    assert_eq!(rx.try_recv(), Ok(Some("Hello!")));
}

#[test]
fn shared_receiver_same_mailbox() {
    let (_, rx) = barrage::unbounded::<()>();
    let shared_a_1 = rx.clone().into_shared();
    let shared_a_2 = shared_a_1.clone();
    let shared_b = rx.into_shared();

    assert!(shared_a_1.same_mailbox(&shared_a_2));
    assert!(!shared_a_1.same_mailbox(&shared_b));
}

#[test]
fn shared_receiver_drop() {
    let (tx, rx) = barrage::unbounded();
    let shared1 = rx.into_shared();
    let shared2 = shared1.clone();

    tx.try_send("Hello!").unwrap();
    assert_eq!(shared1.try_recv(), Ok(Some("Hello!")));
    assert_eq!(shared2.try_recv(), Ok(None));

    drop(shared2);

    tx.try_send("Hello2!").unwrap();
    assert_eq!(shared1.try_recv(), Ok(Some("Hello2!")));

    drop(shared1);

    assert!(tx.try_send("Hello3!").is_err());
}

#[test]
fn shared_receiver_upgrade() {
    let (tx, rx) = barrage::unbounded();
    let shared1 = rx.into_shared();
    let rx2 = shared1.clone().upgrade();

    tx.try_send("Hello!").unwrap();
    assert_eq!(shared1.try_recv(), Ok(Some("Hello!")));
    assert_eq!(rx2.try_recv(), Ok(Some("Hello!")));
}