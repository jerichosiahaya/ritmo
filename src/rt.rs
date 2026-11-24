//! Real-time guard for the audio thread. In debug and test builds a counting global allocator
//! records every allocation or free made by a thread between [`enter`] and [`exit`]; the audio
//! callback runs inside such a section and the UI reports [`violations`]. Release builds use the
//! system allocator unchanged and these functions do nothing.

#[cfg(any(debug_assertions, test))]
mod imp {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    thread_local! { static IN_RT: Cell<bool> = const { Cell::new(false) }; }
    static VIOLATIONS: AtomicUsize = AtomicUsize::new(0);

    struct Guard;

    impl Guard {
        fn check() {
            // `try_with`: the thread-local may already be gone while a thread shuts down.
            if IN_RT.try_with(Cell::get).unwrap_or(false) {
                VIOLATIONS.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // Counts, then forwards to the system allocator; `realloc` and `alloc_zeroed` use the
    // trait's defaults, which call `alloc`/`dealloc` and so are counted too.
    unsafe impl GlobalAlloc for Guard {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Self::check();
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            Self::check();
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GUARD: Guard = Guard;

    pub fn enter() {
        IN_RT.with(|f| f.set(true));
    }

    pub fn exit() {
        IN_RT.with(|f| f.set(false));
    }

    pub fn violations() -> usize {
        VIOLATIONS.load(Ordering::Relaxed)
    }
}

#[cfg(not(any(debug_assertions, test)))]
mod imp {
    pub fn enter() {}
    pub fn exit() {}
    pub fn violations() -> usize {
        0
    }
}

pub use imp::{enter, exit, violations};

#[cfg(test)]
mod tests {
    #[test]
    fn counts_heap_use_only_inside_a_section_on_this_thread() {
        let before = super::violations();
        drop(std::hint::black_box(vec![1u8; 64]));
        assert_eq!(super::violations(), before);

        super::enter();
        let inside = std::hint::black_box(vec![1u8; 64]);
        drop(inside);
        super::exit();
        assert_eq!(super::violations() - before, 2);
    }
}
