use barrage::{Disconnected, SendError};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::time::Duration;

struct PollOnce<'a, F: Future + Unpin>(&'a mut F);

impl<'a, F: Future + Unpin> Future for PollOnce<'a, F> {
    type Output = Poll<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Pin::new(&mut self.0).poll(cx))
    }
}

#[tokio::test]
async fn one_message_unbounded() {
    let (tx, mut rx) = barrage::new(None);
    let mut rx2 = rx.clone();
    tx.send_async("Hello!").await.unwrap();
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx2.recv_async().await, Ok("Hello!"));
}

#[tokio::test]
async fn sync_receive_from_wait_async_send() {
    let (tx, mut rx) = barrage::new(None);

    let handle = tokio::task::spawn_blocking(move || {
        assert_eq!("Hello!", rx.recv().unwrap());
    });

    tokio::time::delay_for(Duration::from_millis(500)).await;
    tx.send_async("Hello!").await.unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn sync_receive_from_wait_try_send() {
    let (tx, mut rx) = barrage::new(None);

    let handle = tokio::task::spawn_blocking(move || {
        assert_eq!("Hello!", rx.recv().unwrap());
    });

    tokio::time::delay_for(Duration::from_millis(500)).await;
    tx.try_send("Hello!").unwrap();
    handle.await.unwrap();
}

#[tokio::test]
async fn sync_receive() {
    let (tx, mut rx) = barrage::new(None);

    tx.send_async("Hello!").await.unwrap();
    assert_eq!("Hello!", rx.recv().unwrap());
}

#[tokio::test]
async fn new_recv_after_send() {
    let (tx, mut rx) = barrage::new(None);
    tx.send_async("Hello!").await.unwrap();
    let mut rx2 = rx.clone();
    tx.send_async("Hello 2!").await.unwrap();
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx2.recv_async().await, Ok("Hello 2!"));
}

#[tokio::test]
async fn tx_drop_disconnect() {
    let (tx, mut rx) = barrage::new(None);
    tx.send_async("Hello!").await.unwrap();
    drop(tx);
    let mut rx2 = rx.clone();
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
    let (tx, mut rx) = barrage::new(Some(1));
    tx.send_async("Hello!").await.unwrap();
    let mut fut = tx.send_async("Hello!");
    assert_eq!(PollOnce(&mut fut).await, Poll::Pending);
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(PollOnce(&mut fut).await, Poll::Ready(Ok(())));
}

#[tokio::test]
async fn try_send_bounded_wait_resume() {
    let (tx, mut rx) = barrage::new(Some(1));
    tx.try_send("Hello!").unwrap();
    let mut fut = tx.send_async("Hello!");
    assert_eq!(PollOnce(&mut fut).await, Poll::Pending);
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(PollOnce(&mut fut).await, Poll::Ready(Ok(())));
}

#[tokio::test]
async fn sync_send_bounded_wait_resume() {
    let (tx, mut rx) = barrage::new(Some(1));
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
        let mut rx = rx.clone();
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
