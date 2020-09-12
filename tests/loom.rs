#![cfg(loom)]
//! Adapted from https://github.com/tokio-rs/tokio/blob/master/tokio/src/sync/mod.rs.
//! Thank you Tokio team <3

use loom::future::block_on;
use loom::sync::Arc;
use loom::thread;
use tokio_test::assert_ok;

//#[test]
fn broadcast_send() {
    loom::model(|| {
        let (tx1, mut rx) = barrage::bounded(2);
        let tx1 = Arc::new(tx1);
        let tx2 = tx1.clone();

        let th1 = thread::spawn(|| block_on(async move {
            assert_ok!(tx1.send_async("one").await);
            assert_ok!(tx1.send_async("two").await);
            assert_ok!(tx1.send_async("three").await);
        }));

        let th2 = thread::spawn(|| block_on(async move {
            assert_ok!(tx2.send_async("inye").await);
            assert_ok!(tx2.send_async("zimbini").await);
            assert_ok!(tx2.send_async("zintathu").await);
        }));

        block_on(async {
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
    });
}

// An `Arc` is used as the value in order to detect memory leaks.
// TODO(sigill)
//#[test]
fn broadcast_two() {
    loom::model(|| {
        let (tx, mut rx1) = barrage::bounded::<Arc<&'static str>>(16);
        let mut rx2 = rx1.clone();

        let th1 = thread::spawn(move || {
            block_on(async {
                let v = assert_ok!(rx1.recv_async().await);
                assert_eq!(*v, "hello");

                let v = assert_ok!(rx1.recv_async().await);
                assert_eq!(*v, "world");

                assert!(rx1.recv_async().await.is_err());
            });
        });

        let th2 = thread::spawn(move || {
            block_on(async {
                let v = assert_ok!(rx2.recv_async().await);
                assert_eq!(*v, "hello");

                let v = assert_ok!(rx2.recv_async().await);
                assert_eq!(*v, "world");

                assert!(rx2.recv_async().await.is_err());
            });
        });

        assert_ok!(block_on(tx.send_async(Arc::new("hello"))));
        assert_ok!(block_on(tx.send_async(Arc::new("world"))));
        drop(tx);

        assert_ok!(th1.join());
        assert_ok!(th2.join());
    });
}

#[test]
fn drop_tx_rx() {
    loom::model(|| {
        let (tx, mut rx1) = barrage::bounded(16);
        let rx2 = rx1.clone();

        let th1 = thread::spawn(move || {
            block_on(async {
                let v = assert_ok!(rx1.recv_async().await);
                assert_eq!(v, "one");

                let v = assert_ok!(rx1.recv_async().await);
                assert_eq!(v, "two");

                let v = assert_ok!(rx1.recv_async().await);
                assert_eq!(v, "three");

                assert!(rx1.recv_async().await.is_err());
            });
        });

        // let th2 = thread::spawn(move || {
        //     drop(rx2);
        // });

        assert_ok!(block_on(tx.send_async("one")));
        assert_ok!(block_on(tx.send_async("two")));
        assert_ok!(block_on(tx.send_async("three")));
        drop(tx);

        assert_ok!(th1.join());
        // assert_ok!(th2.join());
    });
}

//#[test]
fn a() {
    loom::model(|| {
        let arc = Arc::new(());
        let arc2 = arc.clone();
        let th1 = thread::spawn(move || drop(arc2));
        drop(arc);
        assert_ok!(th1.join());
    })
}
