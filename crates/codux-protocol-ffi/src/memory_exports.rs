//! Returning freed pages to the OS.
//!
//! Dropping a terminal session frees its grid, but the platform allocator keeps
//! those pages on its own free lists — RSS does not move. Measured on a real
//! device (arm64, Android 16) with 16 cached sessions of 20k lines each:
//! evicting 15 of them left RSS at 138 MB, and a single purge took it to 18 MB.
//! That matters because Android's low-memory killer scores processes by RSS, so
//! the pages we are not using still count against us until we hand them back.
//!
//! This only returns memory that is already free. It cannot lose data, and the
//! pages fault back in on demand if they are needed again.

/// Hand freed-but-retained pages back to the OS.
///
/// Returns true when the platform had a purge to run. The call is not free
/// (tens of milliseconds when there is a lot to return), so callers should
/// reach for it after bulk eviction or when going to the background — not on a
/// hot path.
#[unsafe(no_mangle)]
pub extern "C" fn codux_malloc_purge() -> bool {
    purge()
}

#[cfg(target_os = "android")]
fn purge() -> bool {
    // dlsym rather than a direct extern: bionic only exports mallopt from API
    // 26, and this library is built against a lower platform level, so linking
    // it directly would fail to load on every device. M_PURGE (-101) drains
    // this thread's arena, M_PURGE_ALL (-104) drains all of them.
    unsafe extern "C" {
        fn dlsym(
            handle: *mut core::ffi::c_void,
            symbol: *const core::ffi::c_char,
        ) -> *mut core::ffi::c_void;
    }
    const M_PURGE: i32 = -101;
    const M_PURGE_ALL: i32 = -104;
    unsafe {
        let symbol = dlsym(core::ptr::null_mut(), c"mallopt".as_ptr());
        if symbol.is_null() {
            return false;
        }
        let mallopt: extern "C" fn(i32, i32) -> i32 = core::mem::transmute(symbol);
        mallopt(M_PURGE, 0);
        mallopt(M_PURGE_ALL, 0);
    }
    true
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn purge() -> bool {
    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut core::ffi::c_void, goal: usize) -> usize;
    }
    // A null zone means "every zone"; a goal of 0 means "as much as you can".
    unsafe { malloc_zone_pressure_relief(core::ptr::null_mut(), 0) };
    true
}

#[cfg(not(any(target_os = "android", target_os = "ios", target_os = "macos")))]
fn purge() -> bool {
    false
}
