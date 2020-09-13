# barrage

A simple async broadcast channel. It is runtime agnostic and can be used from any executor. It can also operate
synchronously.

## Example

```rust
#[tokio::main]
async fn main() {
    let (tx, rx) = barrage::unbounded();
    let rx2 = rx.clone();
    tx.send_async("Hello!").await.unwrap();
    assert_eq!(rx.recv_async().await, Ok("Hello!"));
    assert_eq!(rx2.recv_async().await, Ok("Hello!"));
}
```
