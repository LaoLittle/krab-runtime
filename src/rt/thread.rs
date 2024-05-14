use crate::rt::barrier_enabled;
use crate::rt::gc::{thread_root_set, ThreadRoot, REGISTRY};
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::thread;
use std::thread::Thread;
use std::time::Duration;

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
        // poll per 100us
        thread::sleep(Duration::from_micros(100));
    }
}

#[inline]
pub fn wake_all_threads(threads: &[Thread]) {
    for thread in threads {
        thread.unpark();
    }
}

pub fn thread_enabling_chan() -> &'static (Sender<Thread>, Receiver<Thread>) {
    static CHAN: OnceLock<(Sender<Thread>, Receiver<Thread>)> = OnceLock::new();

    CHAN.get_or_init(|| crossbeam_channel::unbounded())
}

#[inline]
pub unsafe fn krab_thread_prologue() {
    // before the registry registers
    thread_inc();

    let thread = thread::current();

    REGISTRY.with(|r| {
        let locals = r.root.get();

        let id = unsafe {
            (*thread_root_set().0.get())
                .insert(ThreadRoot {
                    thread: thread.clone(),
                    locals,
                })
                .expect("unable to insert root set")
        };

        r.root_index.set(id);
    });

    if unsafe { barrier_enabled() } {
        thread_enabling_chan().0.send(thread).unwrap();
    }
}

#[inline]
pub unsafe fn krab_thread_epilogue() {
    REGISTRY.with(|r| {
        let id = r.root_index.get();
        if id == usize::MAX {
            eprintln!("thread epilogue called before prologue");
            std::process::abort();
        }

        unsafe {
            (*thread_root_set().0.get()).take(id); // it should not block because we haven't entered gc cycle yet.
        }
    });

    thread_dec();
}
