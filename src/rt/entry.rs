use crate::rt::gc::krab_thread_prologue;

#[export_name = "krab.lang.start"]
pub unsafe extern "C" fn krab_lang_start(
    main_f: unsafe extern "C" fn(),
    argc: isize,
    argv: *const *const u8,
) -> isize {
    println!("runtime.init");

    dbg!(argc, argv);
    
    unsafe {
        // main thread.
        krab_thread_prologue();

        main_f();
        
        // no epilogue. WE ARE ENDING HERE.
    }
    
    0
}
