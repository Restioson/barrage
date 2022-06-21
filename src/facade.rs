// These modules are a hack to get around IntelliJ not yet understanding mutually exclusive feature
// gates as mutually exclusive

mod rwlock {
    #[cfg(not(windows))]
    pub use spin::rwlock::*;
    #[cfg(windows)]
    pub use std::sync::*;
}

pub type RwLock<T> = rwlock::RwLock<T>;
pub type WriteGuard<'a, T> = rwlock::RwLockWriteGuard<'a, T>;
pub type ReadGuard<'a, T> = rwlock::RwLockReadGuard<'a, T>;

#[cfg(not(windows))]
pub fn wait_lock<'a, T, F, R>(lock: &'a RwLock<T>, try_lock: F) -> R
where
    F: Fn(&'a RwLock<T>) -> Option<R>,
{
    #[allow(unused_variables)]
    let mut i = 4;

    loop {
        for _ in 0..10 {
            if let Some(guard) = try_lock(lock) {
                return guard;
            }
            std::thread::yield_now();
        }

        std::thread::sleep(std::time::Duration::from_nanos(1 << i));

        i += 1;
    }
}

#[cfg(not(windows))]
pub fn lock_write<T>(lock: &RwLock<T>) -> WriteGuard<'_, T> {
    wait_lock(lock, |l| l.try_write())
}

#[cfg(not(windows))]
pub fn lock_read<T>(lock: &RwLock<T>) -> ReadGuard<'_, T> {
    wait_lock(lock, |l| l.try_read())
}

#[cfg(windows)]
pub fn lock_write<T>(lock: &RwLock<T>) -> WriteGuard<'_, T> {
    lock.write().unwrap()
}

#[cfg(windows)]
pub fn lock_read<T>(lock: &RwLock<T>) -> ReadGuard<'_, T> {
    lock.read().unwrap()
}
