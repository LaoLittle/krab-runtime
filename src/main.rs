use std::mem::{align_of, size_of};

use gc_runtime::rt::{gc_allocate, gc_deallocate, ObjectHead};

#[derive(Debug)]
struct ExampleObject {
    data: [i32; 4],
}

fn test() {}

fn dab(f: Option<fn()>) {
    f.unwrap()();
}

fn main() {
    dab(Some(test));
    
    let mut obj = ExampleObject {
        data: [0; 4]
    };

    obj.data[0] = 114;
    obj.data[1] = 514;

    dbg!(align_of::<ExampleObject>());

    unsafe {
        let (ptr, off) = gc_allocate(size_of::<ExampleObject>(), align_of::<ExampleObject>());

        let obj_ptr = ptr.add(off).cast::<ExampleObject>();
        obj_ptr.write(obj);

        println!("{:?}", ptr.cast::<ObjectHead>().as_ref());
        println!("{:?}", obj_ptr.as_ref());

        gc_deallocate(ptr);
    }
    
}
