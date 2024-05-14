use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::thread;
use std::thread::Thread;
use crossbeam_channel::{Receiver, Sender};
use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};
use sharded_slab::Slab;
use crate::rt::{enable_barrier, GcColor, KRef, stop_world};
use crate::rt::thread::{thread_dec, thread_inc, wait_for_suspension, wake_all_threads};

pub struct OwnedKObject(pub(crate) KRef);

unsafe impl Send for OwnedKObject {}
unsafe impl Sync for OwnedKObject {}

pub struct SyncKObject(pub(crate) KRef);

unsafe impl Send for SyncKObject {}
unsafe impl Sync for SyncKObject {}

pub fn heap() -> &'static Slab<OwnedKObject> {
    static HEAP: OnceLock<Slab<OwnedKObject>> = OnceLock::new();
    
    HEAP.get_or_init(|| Slab::new())
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
    match (*obj).info.color.compare_exchange(
        GcColor::White as u8,
        GcColor::Gray as u8,
        Ordering::Relaxed,
        Ordering::Relaxed
    ) {
        Ok(_) => {
            // add the object to gray set.
            gray_chan().0.send(SyncKObject(obj)).unwrap();
        },
        Err(_) => {}, // already gray or black.
    }
}

pub unsafe fn gc_mark_black() {
    while let Ok(obj) = gray_chan().1.try_recv() {
        let obj = obj.0;
        unsafe {
            (*obj).info.color.store(GcColor::Black as u8, Ordering::Relaxed);
            (*obj).mark_fn.unwrap()(obj);
        }
    }
}

pub struct ThreadRootSet(UnsafeCell<Slab<ThreadRoot>>);

unsafe impl Send for ThreadRootSet {}
unsafe impl Sync for ThreadRootSet {}

pub struct ThreadRoot {
    pub thread: Thread,
    pub locals: *const Vec<*mut KRef>
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
    pub root_index: Cell<usize>
}

thread_local! {
    pub static REGISTRY: ThreadRegistry = ThreadRegistry {
        root: UnsafeCell::new(Vec::new()),
        root_index: Cell::new(usize::MAX)
    };
}

#[inline]
pub unsafe fn krab_thread_prologue() {
    // before the registry registers
    thread_inc();
    
    let thread = thread::current();
    
    REGISTRY.with(|r| {
        let locals = r.root.get();
        
        let id = unsafe { 
            (*thread_root_set().0.get()).insert(ThreadRoot {
                thread,
                locals
            }).expect("unable to insert root set")
        };
        
        r.root_index.set(id);
    });
    
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

#[inline]
#[export_name = "krab.gc.pushLocal"]
pub unsafe extern "C" fn push_local(obj: *mut KRef) {
    REGISTRY.with(|r| {
        unsafe {
            (*r.root.get()).push(obj);
        }
    });
}

#[inline]
#[export_name = "krab.gc.popLocal"]
pub unsafe extern "C" fn pop_local() {
    REGISTRY.with(|r| {
        unsafe {
            (*r.root.get()).pop();
        }
    });
}

pub unsafe fn gc_thread_start() {
    thread::spawn(|| {
        let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        
        let mut root_objects: Vec<SyncKObject> = Vec::new();
        let mut threads: Vec<Thread> = Vec::new();
        
        loop {
            thread::park(); // todo: wait for gc signal
            
            stop_world(true); // stop the world
            wait_for_suspension();
            
            // Fetch all objects from the root.
            
            unsafe {
                enable_barrier(true); // This is safe because no other threads are currently reading/writing the flag because of the STW.
            }
            
            unsafe {
                for root in (*thread_root_set().0.get()).unique_iter() { // Safe. Same as above.
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
            
            // Concurrent Marking Phase
            
            root_objects
                .par_iter()
                .for_each(|e| {
                    unsafe {
                        mark_gray(e.0);
                    }
                });
            
            root_objects.clear();

            pool.scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(|_| {
                        unsafe {
                            gc_mark_black();
                        }
                    });
                }
            });
            
            stop_world(true);
            wait_for_suspension();
            // Final Marking Phase
            gc_mark_black();
            
            unsafe { 
                enable_barrier(false);
            }
            
            stop_world(false); // start the world
            
            // Sweep Phase
            
        }
    });
}