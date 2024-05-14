mod entry;
mod gc;
mod thread;

use crate::rt::gc::{heap, mark_gray, OwnedKObject};
use crate::rt::thread::{thread_suspension_dec, thread_suspension_inc};
use std::alloc::{alloc, dealloc, Layout};
use std::mem::{align_of, size_of};
use std::process::abort;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub type KRef = *mut ObjectHead;
pub type KConstRef = *const ObjectHead;

#[derive(Debug)]
pub struct ObjectHead {
    pub metadata: TypeMetadata,
    pub info: GcInfo,
    pub index: usize,
    pub root_index: usize,
    pub mark_fn: Option<unsafe extern "C" fn(KRef)>,
    pub object_size: usize,
}

#[derive(Debug)]
pub struct GcInfo {
    pub color: AtomicU8,
}

impl GcInfo {
    pub const fn new(color: GcColor) -> Self {
        GcInfo {
            color: AtomicU8::new(color as u8),
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub enum GcColor {
    White = 0,
    Gray = 1,
    Black = 2,
}

#[derive(Debug, Copy, Clone)]
pub struct TypeMetadata {
    pub align: usize,
}

#[export_name = "krab.gc.allocate"]
pub unsafe extern "C" fn krab_gc_allocate(size: usize, align: usize) -> KRef {
    let obj = unsafe { gc_allocate(size, align).0.cast() };

    let index = unsafe {
        (*heap().0.get())
            .insert(OwnedKObject(obj))
            .expect("unable to allocate object")
    };

    unsafe {
        (*obj).index = index;
    }

    obj
}

// *slot: T? = obj: T?
#[export_name = "krab.gc.writeBarrier_00"]
pub unsafe extern "C" fn krab_gc_write_barrier_00(slot: *mut KRef, obj: KRef) {
    if unsafe { barrier_enabled() } {
        unsafe {
            let t = *slot;
            if !t.is_null() {
                // Yuasa
                barrier_mark(t);
            }

            if !obj.is_null() {
                // Dijkstra
                barrier_mark(obj);
            }
        }
    }

    unsafe {
        *slot = obj;
    }
}

// *slot: T? = obj: T
#[export_name = "krab.gc.writeBarrier_01"]
pub unsafe extern "C" fn krab_gc_write_barrier_01(slot: *mut KRef, obj: KRef) {
    if unsafe { barrier_enabled() } {
        unsafe {
            let t = *slot;
            if !t.is_null() {
                barrier_mark(*slot);
            }

            barrier_mark(obj);
        }
    }

    unsafe {
        *slot = obj;
    }
}

// *slot: T = obj: T?
#[export_name = "krab.gc.writeBarrier_10"]
pub unsafe extern "C" fn krab_gc_write_barrier_10(slot: *mut KRef, obj: KRef) {
    if unsafe { barrier_enabled() } {
        unsafe {
            barrier_mark(*slot);

            if !obj.is_null() {
                barrier_mark(obj);
            }
        }
    }

    unsafe {
        *slot = obj;
    }
}

// *slot: T = obj: T
#[export_name = "krab.gc.writeBarrier_11"]
pub unsafe extern "C" fn krab_gc_write_barrier_11(slot: *mut KRef, obj: KRef) {
    if unsafe { barrier_enabled() } {
        unsafe {
            barrier_mark(*slot);
            barrier_mark(obj);
        }
    }

    unsafe {
        *slot = obj;
    }
}

#[export_name = "krab.gc.safepoint"]
pub unsafe extern "C" fn krab_gc_safepoint() {
    let mut suspend = world_stopped();

    if suspend {
        thread_suspension_inc();
    } else {
        return;
    }

    while suspend {
        std::thread::park();
        suspend = world_stopped();
    }

    thread_suspension_dec();
}

#[export_name = "krab.gc.enterSaferegion"]
pub unsafe extern "C" fn krab_gc_enter_saferegion() {
    thread_suspension_inc();
}

#[export_name = "krab.gc.exitSaferegion"]
pub unsafe extern "C" fn krab_gc_exit_saferegion() {
    while world_stopped() {
        std::thread::park();
    }

    thread_suspension_dec();
}

static mut BARRIER_ENABLED: bool = false;

#[inline]
unsafe fn barrier_enabled() -> bool {
    unsafe { BARRIER_ENABLED }
}

unsafe fn enable_barrier(b: bool) {
    unsafe {
        BARRIER_ENABLED = b;
    }
}

static WORLD_STOPPED: AtomicBool = AtomicBool::new(false);

fn world_stopped() -> bool {
    WORLD_STOPPED.load(Ordering::Relaxed)
}

fn stop_world(b: bool) {
    WORLD_STOPPED.store(b, Ordering::Relaxed);
}

#[inline]
unsafe fn barrier_mark(obj: KRef) {
    unsafe {
        mark_gray(obj);
    }
}

// |ObjectHead|Alignment|Object|
pub unsafe fn gc_allocate(size: usize, align: usize) -> (*mut u8, usize) {
    // in release mode, the alignment would be checked in Layout::from_size_align.
    debug_assert!(align.is_power_of_two(), "incorrect alignment: {align}");

    let align = align.max(align_of::<ObjectHead>());
    let offset = calculate_offset(align);

    let alloc_size = size + offset;

    let layout = match Layout::from_size_align(alloc_size, align) {
        Ok(layout) => layout,
        Err(e) => {
            eprintln!("{e}, Size = {alloc_size}, Align = {align}");
            abort();
        }
    };

    let ptr = unsafe { alloc(layout) };

    unsafe {
        ptr.cast::<ObjectHead>().write(ObjectHead {
            metadata: TypeMetadata { align },
            mark_fn: None,
            index: usize::MAX,
            root_index: usize::MAX,
            info: GcInfo::new(if !barrier_enabled() {
                GcColor::White
            } else {
                GcColor::Gray
            }),
            object_size: size,
        });
    }

    (ptr, offset)
}

pub unsafe fn gc_deallocate(ptr: *mut u8) {
    let head_ptr = ptr.cast::<ObjectHead>();

    let metadata = unsafe { (*head_ptr).metadata };
    let size = unsafe { (*head_ptr).object_size };

    let offset = calculate_offset(metadata.align);

    let alloc_size = size + offset;

    let layout = Layout::from_size_align(alloc_size, metadata.align).unwrap();

    unsafe {
        dealloc(ptr, layout);
    }
}

#[inline]
pub const fn calculate_offset(mut align: usize) -> usize {
    let head_align = align_of::<ObjectHead>();

    if align < head_align {
        align = head_align;
    }

    let head_size = size_of::<ObjectHead>();
    // |Align|Align|Align|Align|
    // |Head---------| -> Offset = 3 * Align
    // |Head-------------| -> Offset = 3 * Align
    // |-------Head------|Object...|
    let mut offset = (head_size / align) * align;
    if offset != head_size {
        offset += align;
    }

    offset
}
