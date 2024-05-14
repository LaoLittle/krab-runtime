use crate::rt::thread::{thread_enabling_chan, wait_for_suspension, wake_all_threads};
use crate::rt::{enable_barrier, gc_deallocate, stop_world, GcColor, KRef};
use crossbeam_channel::{Receiver, Sender};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use sharded_slab::Slab;
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::thread;
use std::thread::Thread;

pub struct OwnedKObject(pub(crate) KRef);

unsafe impl Send for OwnedKObject {}
unsafe impl Sync for OwnedKObject {}

pub struct SyncKObject(pub(crate) KRef);

unsafe impl Send for SyncKObject {}
unsafe impl Sync for SyncKObject {}

pub struct Heap(pub UnsafeCell<Slab<OwnedKObject>>);

unsafe impl Send for Heap {}
unsafe impl Sync for Heap {}

pub fn heap() -> &'static Heap {
    static HEAP: OnceLock<Heap> = OnceLock::new();

    HEAP.get_or_init(|| Heap(UnsafeCell::new(Slab::new())))
}

pub fn gc_root() -> &'static Slab<SyncKObject> {
    static ROOT: OnceLock<Slab<SyncKObject>> = OnceLock::new();

    ROOT.get_or_init(|| Slab::new())
}

pub fn gray_chan() -> &'static (Sender<SyncKObject>, Receiver<SyncKObject>) {
    static GRAY_CHAN: OnceLock<(Sender<SyncKObject>, Receiver<SyncKObject>)> = OnceLock::new();

    GRAY_CHAN.get_or_init(|| crossbeam_channel::unbounded())
}

pub unsafe fn mark_gray(obj: KRef) {
    match unsafe { &(*obj) }.info.color.compare_exchange(
        GcColor::White as u8,
        GcColor::Gray as u8,
        Ordering::Relaxed,
        Ordering::Relaxed,
    ) {
        Ok(_) => {
            // add the object to gray set.
            gray_chan().0.send(SyncKObject(obj)).unwrap();
        }
        Err(_) => {} // already gray or black.
    }
}

// Recv all gray objects from channel and perform marking.
pub unsafe fn gc_mark_black() {
    while let Ok(obj) = gray_chan().1.try_recv() {
        let obj = obj.0;
        unsafe {
            (*obj)
                .info
                .color
                .store(GcColor::Black as u8, Ordering::Relaxed);
            (*obj).mark_fn.unwrap()(obj);
        }
    }
}

pub struct ThreadRootSet(pub UnsafeCell<Slab<ThreadRoot>>);

unsafe impl Send for ThreadRootSet {}
unsafe impl Sync for ThreadRootSet {}

pub struct ThreadRoot {
    pub thread: Thread,
    pub locals: *const Vec<*mut KRef>,
}

unsafe impl Send for ThreadRoot {}
unsafe impl Sync for ThreadRoot {}

pub fn thread_root_set() -> &'static ThreadRootSet {
    static ROOT_SET: OnceLock<ThreadRootSet> = OnceLock::new();

    ROOT_SET.get_or_init(|| ThreadRootSet(UnsafeCell::new(Slab::new())))
}

#[derive(Default)]
pub(crate) struct ThreadRegistry {
    pub root: UnsafeCell<Vec<*mut KRef>>,
    pub root_index: Cell<usize>,
}

thread_local! {
    pub static REGISTRY: ThreadRegistry = ThreadRegistry {
        root: UnsafeCell::new(Vec::new()),
        root_index: Cell::new(usize::MAX)
    };
}

#[inline]
#[export_name = "krab.gc.pushLocal"]
pub unsafe extern "C" fn push_local(obj: *mut KRef) {
    REGISTRY.with(|r| unsafe {
        (*r.root.get()).push(obj);
    });
}

#[inline]
#[export_name = "krab.gc.popLocal"]
pub unsafe extern "C" fn pop_local() {
    REGISTRY.with(|r| unsafe {
        (*r.root.get()).pop();
    });
}

const MARK_THREADS: usize = 4;

pub unsafe fn gc_thread_start() {
    thread::spawn(|| {
        let pool = ThreadPoolBuilder::new()
            .num_threads(MARK_THREADS)
            .build()
            .unwrap();

        let mut root_objects: Vec<SyncKObject> = Vec::new();
        let mut threads: Vec<Thread> = Vec::new();
        let mut heap_objects: Vec<SyncKObject> = Vec::new();
        // let mut white_set: Vec<KConstRef> = Vec::new();

        loop {
            thread::park(); // todo: wait for gc signal

            stop_world(true); // stop the world
            wait_for_suspension();

            // ====== WORLD STOPPED ======
            // Fetch all objects from roots.

            unsafe {
                enable_barrier(true); // This is safe since no other threads are currently reading/writing the flag because of the STW.
            }

            unsafe {
                for root in (*thread_root_set().0.get()).unique_iter() {
                    // Safe. Same as above.
                    root_objects.reserve((*root.locals).len());
                    for &local in (*root.locals).iter() {
                        let obj = *local;
                        if obj.is_null() {
                            continue;
                        }

                        root_objects.push(SyncKObject(obj));
                    }

                    threads.push(root.thread.clone());
                }
            }

            stop_world(false); // start the world
            wake_all_threads(&threads);

            // ====== WORLD STARTED ======
            // Concurrent Marking Phase

            root_objects.par_iter().for_each(|e| unsafe {
                mark_gray(e.0);
            });

            pool.scope(|scope| {
                for _ in 0..MARK_THREADS {
                    scope.spawn(|_| unsafe {
                        gc_mark_black();
                    });
                }
            });

            stop_world(true);
            wait_for_suspension();

            // ====== WORLD STOPPED ======
            // Final Marking Phase
            unsafe {
                enable_barrier(false); // disable write barrier
            }

            unsafe {
                // mark newly created objects.
                gc_mark_black();
            }

            unsafe {
                for object in (*heap().0.get()).unique_iter() {
                    heap_objects.push(SyncKObject(object.0));
                }
            }

            // no more gray objects remain or create since barrier is disabled.

            stop_world(false); // start the world
            wake_all_threads(&threads);
            while let Ok(t) = thread_enabling_chan().1.try_recv() {
                // wake newly created threads.
                t.unpark();
            }

            // ====== WORLD STARTED ======
            // Sweep Phase

            // We only manipulate old objects, so it's safe to sweep concurrently.
            // All white objects here are not reachable.

            heap_objects.par_iter().for_each(|obj| {
                if unsafe { &(*obj.0) }.info.color.load(Ordering::Relaxed) != GcColor::White as u8 {
                    return;
                }

                // run finalizer here...
                // write barrier is always enabled in finalizers,
                // and any object created in finalizers should be marked as black object,
                // and should be left for the next gc cycle.
            });

            heap_objects.par_iter().for_each(|obj| {
                let obj = obj.0;

                match unsafe { &(*obj) }.info.color.compare_exchange(
                    GcColor::Black as u8,
                    GcColor::White as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // black. Do nothing.
                        return;
                    }
                    Err(_) => {
                        // white.

                        // remove from the heap.
                        unsafe {
                            (*heap().0.get()).remove((*obj).index);
                            gc_deallocate(obj.cast());
                        }
                    }
                }
            });

            // Clear GC state
            root_objects.clear();
            threads.clear();
            heap_objects.clear();
        }
    });
}
