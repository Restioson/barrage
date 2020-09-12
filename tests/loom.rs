#![cfg(loom)]

use loom::future::block_on;
use loom::sync::Arc;
use loom::thread;
use tokio_test::assert_ok;

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
