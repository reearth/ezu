//! Turn heap exhaustion into something the host can name.
//!
//! Rust's reaction to a failed allocation is `handle_alloc_error`, which on
//! wasm is an `unreachable` trap. The instance dies mid-call, the return
//! value is never written, and the host is left holding a bare
//! "unreachable" with nothing saying which of the hundreds of allocations
//! in a render was the one that could not be served.
//!
//! The JS shell has the same problem and solves it by throwing a JS `Error`
//! named `OutOfMemory` from inside the allocator. There is no such thing to
//! throw here, so this allocator calls an **imported host function** —
//! `ezu_host.oom(requested_bytes)` — at the moment the underlying allocator
//! returns null. The host records the size and traps the instance (in
//! wazero, by panicking out of the host function), which is what turns an
//! opaque `unreachable` into "this instance asked for N bytes and could not
//! have them". The Go package does exactly that and reports
//! [`ErrorKind::OutOfMemory`](ezu_renderer::ErrorKind::OutOfMemory), the
//! same name a browser caller sees.
//!
//! The constraint that shapes this: it runs **inside the allocator**, so it
//! cannot allocate. That is why the signal is a bare `u32` through an
//! import and not a message, a formatted string, or anything that touches
//! the heap on the way out.
//!
//! Limits, which callers must respect:
//!
//! - **The instance is finished.** The trap unwinds the wasm frames without
//!   running any Rust cleanup: half-built values leak and the allocator's
//!   own bookkeeping is whatever it was mid-call. Drop the instance and, if
//!   the host retries, build a fresh one. Nothing here makes OOM
//!   recoverable *in place* — it makes it diagnosable.
//! - It only fires for allocation failure. A wasm stack overflow, or a host
//!   that kills the instance for exceeding a cap rather than refusing
//!   `memory.grow`, still ends it without warning.
//!
//! Because the import is unconditional, a host must provide `ezu_host.oom`
//! for the module to instantiate at all. That is deliberate: a host that
//! silently omitted it would be back to the bare `unreachable`, and the
//! failure would be discovered on the day the heap ran out rather than the
//! day the host was written.

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};

    #[link(wasm_import_module = "ezu_host")]
    unsafe extern "C" {
        /// Tell the host the heap could not grow. It is expected not to
        /// return: the instance is unusable either way, and trapping is how
        /// the host turns the failed call into a typed error.
        fn oom(requested_bytes: u32);
    }

    pub struct TellHostOnOom;

    /// Report and never return. Allocates nothing: the only argument is a
    /// number already in a register, which is what makes this safe to call
    /// from inside the allocator.
    #[cold]
    #[inline(never)]
    fn report(size: usize) -> ! {
        unsafe { oom(size.min(u32::MAX as usize) as u32) };
        // Only reached if the host let the call return. There is no heap to
        // continue on, so end the instance rather than hand back a null the
        // caller will dereference.
        core::arch::wasm32::unreachable()
    }

    unsafe impl GlobalAlloc for TellHostOnOom {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(layout) };
            if p.is_null() {
                report(layout.size());
            }
            p
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let p = unsafe { System.alloc_zeroed(layout) };
            if p.is_null() {
                report(layout.size());
            }
            p
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let p = unsafe { System.realloc(ptr, layout, new_size) };
            if p.is_null() {
                report(new_size);
            }
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOC: imp::TellHostOnOom = imp::TellHostOnOom;
