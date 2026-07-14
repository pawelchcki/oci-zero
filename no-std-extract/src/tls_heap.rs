use core::cell::UnsafeCell;
use core::ffi::c_void;

const TLS_ARENA_SIZE: usize = 4 * 1024 * 1024;
const ALIGNMENT: usize = 16;

#[repr(align(16))]
struct AlignedBytes([u8; TLS_ARENA_SIZE]);

struct Arena {
    bytes: UnsafeCell<AlignedBytes>,
    used: UnsafeCell<usize>,
}

// The binary is single-threaded and creates one TLS session.
unsafe impl Sync for Arena {}

static ARENA: Arena = Arena {
    bytes: UnsafeCell::new(AlignedBytes([0; TLS_ARENA_SIZE])),
    used: UnsafeCell::new(0),
};

#[cfg(feature = "bench-metrics")]
pub fn used() -> usize {
    // The hosted binary is single-threaded and calls this only after the TLS
    // session has been closed and dropped.
    unsafe { *ARENA.used.get() }
}

#[cfg(feature = "bench-metrics")]
pub const fn capacity() -> usize {
    TLS_ARENA_SIZE
}

struct SingleThreadedCriticalSection;

critical_section::set_impl!(SingleThreadedCriticalSection);

unsafe impl critical_section::Impl for SingleThreadedCriticalSection {
    unsafe fn acquire() -> critical_section::RawRestoreState {}

    unsafe fn release(_restore_state: critical_section::RawRestoreState) {}
}

/// Fixed-arena replacement for the C allocator used internally by MbedTLS.
///
/// MbedTLS performs many small allocations but this process creates only one
/// session, so reclamation is unnecessary. Exhaustion is reported as a null
/// pointer in the same way as `calloc`.
#[no_mangle]
unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(length) = count.checked_mul(size) else {
        return core::ptr::null_mut();
    };
    if length == 0 {
        return core::ptr::null_mut();
    }

    let used = unsafe { &mut *ARENA.used.get() };
    let Some(start) = used
        .checked_add(ALIGNMENT - 1)
        .map(|value| value & !(ALIGNMENT - 1))
    else {
        return core::ptr::null_mut();
    };
    let Some(end) = start.checked_add(length) else {
        return core::ptr::null_mut();
    };
    if end > TLS_ARENA_SIZE {
        return core::ptr::null_mut();
    }

    let pointer = unsafe { (*ARENA.bytes.get()).0.as_mut_ptr().add(start) };
    unsafe { pointer.write_bytes(0, length) };
    *used = end;
    pointer.cast()
}

#[no_mangle]
unsafe extern "C" fn free(_pointer: *mut c_void) {}
