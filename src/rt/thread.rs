use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::Thread;
use std::time::Duration;
use crate::rt::gc::ThreadRoot;

static THREAD_NUM: AtomicU32 = AtomicU32::new(0);

static THREAD_SUSPENDED_NUM: AtomicU32 = AtomicU32::new(0);

#[inline]
pub fn thread_inc() -> u32 {
    THREAD_NUM.fetch_add(1, Ordering::Relaxed)
}

#[inline]
pub fn thread_dec() -> u32 {
    THREAD_NUM.fetch_sub(1, Ordering::Relaxed)
}

#[inline]
pub fn thread_suspension_inc() -> u32 {
    THREAD_SUSPENDED_NUM.fetch_add(1, Ordering::Relaxed)
}

#[inline]
pub fn thread_suspension_dec() -> u32 {
    THREAD_SUSPENDED_NUM.fetch_sub(1, Ordering::Relaxed)
}

#[inline]
pub fn wait_for_suspension() {
    while THREAD_SUSPENDED_NUM.load(Ordering::Relaxed) != THREAD_NUM.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_micros(100));
    }
}

#[inline]
pub fn wake_all_threads(threads: &[Thread]) {
    for thread in threads {
        thread.unpark();
    }
}