#[cfg(test)]
mod tests {
    use crate::allocator::{DarkAllocator, MINIMUM_SIZE, MAXIMUM_SIZE};
    use crate::error::DarkError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    // --- 1. INITIALIZATION & BOUNDS TESTS ---

    #[test]
    fn test_initialization_default_size() {
        let allocator = DarkAllocator::new(None).unwrap();
        assert_eq!(allocator.capacity(), MAXIMUM_SIZE);
        assert_eq!(allocator.used_capacity(), 0);
        assert_eq!(allocator.remaining_capacity(), MAXIMUM_SIZE);
    }

    #[test]
    fn test_initialization_custom_size_rounding() {
        // Requesting a size above MINIMUM_SIZE should round up to the next power of two
        let requested = 5000; // Between 4096 (4KB) and 8192 (8KB)
        let allocator = DarkAllocator::new(Some(requested)).unwrap();
        assert_eq!(allocator.capacity(), 8192);
    }

    #[test]
    fn test_initialization_validation_errors() {
        // Test size below minimum (4KB)
        let too_small = DarkAllocator::new(Some(1024));
        assert!(matches!(too_small, Err(DarkError::AllocationError(_))));

        // Test size above maximum (4MB)
        let too_large = DarkAllocator::new(Some(MAXIMUM_SIZE + 1));
        assert!(matches!(too_large, Err(DarkError::AllocationError(_))));
    }

    // --- 2. BASIC ALLOCATION & PRIMITIVE TESTS ---

    #[test]
    #[allow(clippy::approx_constant)]
    fn test_basic_allocations() {
        let allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();

        // Allocate different primitive types
        let val_u8 = allocator.allocate(42u8).unwrap();
        assert_eq!(*val_u8, 42);
        *val_u8 = 100;
        assert_eq!(*val_u8, 100);

        let val_u32 = allocator.allocate(1337u32).unwrap();
        assert_eq!(*val_u32, 1337);

        let val_f64 = allocator.allocate(3.14159f64).unwrap();
        assert_eq!(*val_f64, 3.14159);

        // Verify capacity tracking reflects usage
        assert!(allocator.used_capacity() > 0);
        assert!(allocator.remaining_capacity() < allocator.capacity());
    }

    // --- 3. ALIGNMENT ENFORCEMENT TESTS ---

    #[test]
    fn test_alignment_strictness() {
        let allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();

        // Allocate a 1-byte type (u8), which leaves the cursor at an unaligned/odd offset relative to larger types
        let _b1 = allocator.allocate(1u8).unwrap();
        let _b2 = allocator.allocate(2u8).unwrap();

        // Now allocate an 8-byte aligned type (u64). The allocator must calculate padding.
        let aligned_val = allocator.allocate(9999u64).unwrap();
        assert_eq!(*aligned_val, 9999);

        // Verify mathematically that the returned reference address respects 8-byte alignment
        let addr = aligned_val as *const u64 as usize;
        assert_eq!(addr % core::mem::align_of::<u64>(), 0);
    }

    // --- 4. RESET & REUSABILITY TESTS ---

    #[test]
    fn test_arena_reset_and_reusability() {
        let mut allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();

        // Fill some data
        let _x = allocator.allocate(100i32).unwrap();
        let _y = allocator.allocate(200i32).unwrap();
        let used_before_reset = allocator.used_capacity();
        assert!(used_before_reset > 0);

        // Reset the arena
        allocator.reset();

        // Verify capacity is cleared
        assert_eq!(allocator.used_capacity(), 0);
        assert_eq!(allocator.remaining_capacity(), allocator.capacity());

        // Allocate again after reset
        let z = allocator.allocate(500i32).unwrap();
        assert_eq!(*z, 500);
        assert_eq!(allocator.used_capacity(), core::mem::size_of::<i32>());
    }

    // --- 5. OUT OF MEMORY (OOM) TESTS ---

    #[test]
    fn test_out_of_memory_handling() {
        // Create an allocator with the smallest possible capacity (4096 bytes)
        let allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();
        
        loop {
            let data = 0i128;
            if allocator.allocate(data).is_err() {
                break; // Hit OOM as expected
            }
        }

        // Try allocating one more item, should strictly return OutOfMemory error
        let final_attempt = allocator.allocate(42u8);
        assert!(matches!(final_attempt, Err(DarkError::OutOfMemory)));
    }

    // --- 6. DROP GLUE & LIFO ORDER TESTS ---

    // A tracker struct to monitor drop counts and exact destruction order
    struct DropTracker {
        id: usize,
        drop_counter: Arc<AtomicUsize>,
        drop_log: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl Drop for DropTracker {
        fn drop(&mut self) {
            self.drop_counter.fetch_add(1, Ordering::SeqCst);
            self.drop_log.lock().unwrap().push(self.id);
        }
    }

    #[test]
    fn test_drop_glue_lifo_order() {
        let drop_count = Arc::new(AtomicUsize::new(0));
        let drop_log = Arc::new(std::sync::Mutex::new(Vec::new()));

        {
            let mut allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();

            // Allocate items sequentially with IDs 1, 2, 3
            allocator.allocate(DropTracker {
                id: 1,
                drop_counter: Arc::clone(&drop_count),
                drop_log: Arc::clone(&drop_log),
            }).unwrap();

            allocator.allocate(DropTracker {
                id: 2,
                drop_counter: Arc::clone(&drop_count),
                drop_log: Arc::clone(&drop_log),
            }).unwrap();

            allocator.allocate(DropTracker {
                id: 3,
                drop_counter: Arc::clone(&drop_count),
                drop_log: Arc::clone(&drop_log),
            }).unwrap();

            // Trigger reset (should drop items in reverse LIFO order: 3 -> 2 -> 1)
            allocator.reset();

            assert_eq!(drop_count.load(Ordering::SeqCst), 3);

            let log = drop_log.lock().unwrap();
            assert_eq!(*log, vec![3, 2, 1]);
        } // Allocator goes out of scope here, drop stack cleanup runs again but stack is already empty.
    }

    // --- 7. THREAD SAFETY (`SEND`) TESTS ---

    #[test]
    fn test_allocator_send_compliance() {
        let allocator = DarkAllocator::new(Some(MINIMUM_SIZE)).unwrap();

        // Spawn a thread, move the allocator into it, allocate data inside the thread, and drop it there
        let handle = thread::spawn(move || {
            let val = allocator.allocate(777u32).unwrap();
            assert_eq!(*val, 777);
            allocator.capacity()
        });

        let capacity_result = handle.join().unwrap();
        assert_eq!(capacity_result, MINIMUM_SIZE);
    }
}