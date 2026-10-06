//! Project etiquette for heavy work: lower our priority (children inherit it on Windows) and know whether a game runs.

/// below-normal priority for this process (idle when a game runs); best effort
pub fn lower_priority(idle: bool) {
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn GetCurrentProcess() -> isize;
            fn SetPriorityClass(h: isize, class: u32) -> i32;
        }
        SetPriorityClass(GetCurrentProcess(), if idle { 0x40 } else { 0x4000 });
    }
    #[cfg(not(windows))]
    let _ = idle;
}

/// TEMP / TMP for this process and its children: <repo>/work/tmp
pub fn temp_on_repo() {
    let t = crate::paths::repo("work/tmp");
    let _ = std::fs::create_dir_all(&t);
    unsafe {
        std::env::set_var("TEMP", &t);
        std::env::set_var("TMP", &t);
    }
}
