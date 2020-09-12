// These modules are a hack to get around IntelliJ not yet understanding mutually exclusive feature
// gates as mutually exclusive

pub mod thread {
    #[cfg(not(loom))]
    pub use std::thread::*;
    #[cfg(loom)]
    pub use loom::thread::*;
}

pub mod sync {
    #[cfg(not(loom))]
    pub use std::sync::*;
    #[cfg(loom)]
    pub use loom::sync::*;

    pub mod atomic {
        #[cfg(not(loom))]
        pub use std::sync::atomic::*;
        #[cfg(loom)]
        pub use loom::sync::atomic::*;
    }
}

mod rwlock {
    #[cfg(all(not(loom), windows))]
    pub use std::sync::*;
    #[cfg(all(not(loom), not(windows)))]
    pub use spinny::*;
    #[cfg(loom)]
    pub use loom::sync::*;
}

pub type RwLock<T> = rwlock::RwLock<T>;
pub type WriteGuard<'a, T> = rwlock::RwLockWriteGuard<'a, T>;
pub type ReadGuard<'a, T> = rwlock::RwLockReadGuard<'a, T>;

#[cfg(not(windows))]
pub fn wait_lock<'a, T, F, R>(lock: &'a RwLock<T>, try_lock: F) -> R
    where F: Fn(&'a RwLock<T>) -> Option<R>
{
    #[allow(unused_variables)]
    let mut i = 4;

    loop {
        for _ in 0..10 {
            if let Some(guard) = try_lock(lock) {
                return guard;
            }
            thread::yield_now();
        }

        #[cfg(not(loom))]
        {
            thread::sleep(std::time::Duration::from_nanos(1 << i));
        }

        i += 1;
    }
}

#[cfg(all(not(windows), not(loom)))]
pub fn lock_write<T>(lock: &RwLock<T>) -> WriteGuard<'_, T> {
    wait_lock(lock, |l| l.try_write())
}

#[cfg(all(not(windows), not(loom)))]
pub fn lock_read<T>(lock: &RwLock<T>) -> ReadGuard<'_, T> {
    wait_lock(lock, |l| l.try_read())
}

#[cfg(all(not(windows), loom))]
pub fn lock_write<T>(lock: &RwLock<T>) -> WriteGuard<'_, T> {
    wait_lock(lock, |l| Some(l.try_write().unwrap()))
}

#[cfg(all(not(windows), loom))]
pub fn lock_read<T>(lock: &RwLock<T>) -> ReadGuard<'_, T> {
    wait_lock(lock, |l| Some(l.try_read().unwrap()))
}

#[cfg(windows)]
pub fn lock_write<T>(lock: &RwLock<T>) -> WriteGuard<'_, T> {
    lock.lock().unwrap()
}

#[cfg(windows)]
pub fn lock_read<T>(lock: &RwLock<T>) -> ReadGuard<'_, T> {
    lock.lock().unwrap()
}