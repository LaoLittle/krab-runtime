use std::mem::{align_of, size_of};

use krab_runtime::rt::{calculate_offset, gc_deallocate, krab_gc_allocate, ObjectHead};

#[derive(Debug)]
struct ExampleObject {
    data: [i32; 4],
}

fn main() {
    let mut obj = ExampleObject { data: [0; 4] };

    obj.data[0] = 114;
    obj.data[1] = 514;

    dbg!(align_of::<ExampleObject>());

    unsafe {
        let off = calculate_offset(align_of::<ExampleObject>());
        let ptr = krab_gc_allocate(size_of::<ExampleObject>(), align_of::<ExampleObject>());

        let obj_ptr = ptr.cast::<u8>().add(off).cast::<ExampleObject>();
        obj_ptr.write(obj);

        println!("{:?}", ptr.cast::<ObjectHead>().as_ref());
        println!("{:?}", obj_ptr.as_ref());

        gc_deallocate(ptr.cast());
    }
}
