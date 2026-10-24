use std::alloc::{Layout, alloc, dealloc};
use core::ptr::{self, NonNull};
use core::cell::{Cell, RefCell};
use core::mem;
use crate::drop::DropStack;
use crate::error::DarkError;

// Constants
pub const MINIMUM_SIZE: usize = 4096; // 4KB
pub const MAXIMUM_SIZE: usize = 4096 * 1024; // 4MB

pub struct DarkAllocator {
    arena_size: usize, // Total size of the memory pool allocated at initialization
    arena_base: NonNull<u8>, // Pointer to the start of the allocated memory pool
    next: Cell<NonNull<u8>>, // Pointer to the next free byte

    // A structure needed to drop the items correctly when we reset the allocator
    // or go out of scope.
    drop_stack: RefCell<DropStack>,
}

// Implementing Send for Dark Allocator
// # Safety:
// 1. Exclusive & Global Heap Ownership: `DarkAllocator` exclusively owns its underlying memory pool.
//    The memory is requested from a thread-safe allocator (`std::alloc::alloc`), meaning the pool can be
//    deallocated on any thread.
// 2. Type Safety: The allocator only stores (allocates) and manages type that implement `Send`.
// 3. Sync Isolation: We explicitly do NOT implement `Sync` because the allocator contains `Cell` and `RefCell`
//    types used via shared references (e.g. using `allocate(&self, value)`). This ensures that no two threads can
//    ever concurrently invoke allocation / mutation routines on the same allocator instance.
unsafe impl Send for DarkAllocator {}

// Main implementation block
impl DarkAllocator {
    // Create a new instance of Dark Allocator (a memory pool)
    // If user gives None, we use our default max size otherwise clean
    // the size given by user and use it instead.
    pub fn new(size: Option<usize>) -> Result<Self, DarkError> {
        let arena_size;
        if let Some(size) = size {
            // Check if size is smaller than minimum required size
            if size < MINIMUM_SIZE {
                return Err(DarkError::AllocationError(
                    format!("Minimum size required is 4KB (4096 bytes). Requested: {size} bytes.")
                ));
            }

            // Check if size is greater than maximum allowed size
            if size > MAXIMUM_SIZE {
                return Err(DarkError::AllocationError(
                    format!("Maximum size allowed is 4MB (4096 * 1024 bytes). Requested: {size} bytes.")
                ));
            }

            // Round size up to the next power of 2
            if size != MINIMUM_SIZE && size != MAXIMUM_SIZE {
                arena_size = size.next_power_of_two();
            } else {
                arena_size = size;
            }
        } else {
            arena_size = MAXIMUM_SIZE;
        }

        let layout = unsafe {
            // This is safe because all conditions are checked:
            // Align is not zero, it is also a power of 2 and `arena_size`
            // does not overflow isize (arena_size < isize::MAX)
            Layout::from_size_align_unchecked(arena_size, 16)
        };

        let ptr = unsafe {
            // This is safe because the size in `layout` is non-zero.
            // Though this can still fail (return null) if memory is exhausted.
            alloc(layout)
        };

        let ptr = match NonNull::new(ptr) {
            Some(p) => p,
            None => {
                return Err(DarkError::SysOutOfMemory);
            }
        };

        Ok(Self {
            arena_size,
            arena_base: ptr,
            next: Cell::new(ptr),
            drop_stack: RefCell::new(DropStack::new())
        })
    }

    // Places a value of type T into the pool and returns the reference to it.
    //
    // # Safety / Proper Usage:
    // While this function is itself safe to call, the returned mutable reference points to
    // managed memory (the memory pool of the allocator). Users must **not** manually free or drop this
    // memory (e.g. using `ptr::drop_in_place`) as doing so will cause Undefined Behavior.
    //
    // Note: We allow `clippy::mut_from_ref` because this function returns a mutable reference
    // from a shared reference. While Clippy flags this as a dangerous pattern, our implementation
    // guarantees that the returned reference obeys Rust's aliasing rules and won't cause UB.
    //
    // Note: Current version of this allocator only supports types that implement Send trait.
    // This is to ensure that the allocator can be safely sent across threads. It is safe to
    // store types that are !Send too but only if they do not have a Drop Glue (i.e. they implement
    // Copy trait) but we currently do not support them.
    #[allow(clippy::mut_from_ref)]
    pub fn allocate<'a, T>(&'a self, value: T) -> Result<&'a mut T, DarkError> 
    where 
        T: 'a + Send
    {
        let size = mem::size_of::<T>();
        // Return reference to a dangling pointer if value is a ZST
        if size == 0 {
            let dangling = NonNull::<T>::dangling().as_ptr();
            unsafe {
                // Push it to drop_stack if it has a drop glue
                self.drop_stack.borrow_mut().push(dangling);
                // Return the reference and exit the function
                return Ok(&mut *dangling);
            }
        }

        let align_needed = mem::align_of::<T>();
        let current_ptr = self.next.get().as_ptr();

        // Get an aligned pointer to store `value`
        let aligned_ptr = unsafe {
            Self::next_aligned_pointer(current_ptr, align_needed)?
        };

        // Check if the aligned_ptr is in bounds
        let base_ptr = self.arena_base.as_ptr();
        let base_addr = base_ptr as usize;
        let aligned_addr = aligned_ptr as usize;

        // base + size cannot overflow due to OS and allocator guarantees.
        let end_addr = base_addr + self.arena_size;

        // `>=` because aligned address should be smaller than end address.
        if aligned_addr >= end_addr {
            return Err(DarkError::OutOfMemory);
        }

        // Check if there is enough space to store T.
        // Preventing overflow using checked_add
        let Some(next_addr) = aligned_addr.checked_add(size) else {
            return Err(DarkError::OutOfMemory);
        };

        // Checking if it fits in arena
        if next_addr > end_addr {
            return Err(DarkError::OutOfMemory);
        }

        // Write value to the aligned address (1), update the next pointer (2), and return
        // the mutable reference to the data (3)
        unsafe {
            // It is safe to write to aligned_ptr after casting because we have made
            // sure that the alignment and size requirements are fulfilled.
            let aligned_ptr = aligned_ptr as *mut T;

            // Preventing drop of unitialized memory by using `ptr::write`
            ptr::write(aligned_ptr, value);

            // Push the item to Drop Stack (it will not be pushed if it doesn't require drop).
            self.drop_stack.borrow_mut().push(aligned_ptr);

            // Update the `next` pointer to point to the next free byte.
            let next_ptr = base_ptr.with_addr(next_addr);
            self.next.set(NonNull::new_unchecked(next_ptr)); // Safe as `next_ptr` is guaranteed non-null.

            // It is safe to dereference it now as it has been initialized (written to).
            Ok(&mut *aligned_ptr)
        }
    }

    // Resets the arena and handles drop for elements that require it.
    // # Panics: if the drop implementation of some item tried to `allocate()` or
    // touch `drop_stack` in some way, it will result in a panic.
    pub fn reset(&mut self) {
        // Run the cleanup logic of our Drop Stack
        self.drop_stack.borrow_mut().cleanup();
        // Reset the `next` pointer to `arena_base`
        self.next.set(self.arena_base);
    }

    // Returns the next available aligned pointer for some value of type T
    //
    // # Safety: The returned pointer is only safe to dereference if the aligned address
    // still falls within the bounds of the original allocated memory pool.
    // The `align` parameter must be a power of 2.
    unsafe fn next_aligned_pointer(ptr: *mut u8, align: usize) -> Result<*mut u8, DarkError> {
        // Check if alignment is power of 2
        assert!(align.is_power_of_two(), "Alignment must be a power of 2");

        let addr = ptr as usize;

        // Use checked add to prevent overflow
        let Some(added) = addr.checked_add(align - 1) else {
            // Pointer address overflowed memory space during alignment
            return Err(DarkError::OutOfMemory);
        };

        let aligned_addr = added & !(align - 1);

        // Preserve provenance
        Ok(ptr.with_addr(aligned_addr))
    }

    // ---
    // # Some Useful Helpers:
    //
    // Get the capacity (total size) of the memory pool allocated at initialization.
    pub fn capacity(&self) -> usize {
        self.arena_size
    }

    // Get the used capacity of the pool in bytes
    pub fn used_capacity(&self) -> usize {
        let base = self.arena_base.as_ptr() as usize;
        let next = self.next.get().as_ptr() as usize;
        next - base
    }

    // Get the remaining capacity of the pool in bytes
    pub fn remaining_capacity(&self) -> usize {
        let used = self.used_capacity();
        self.arena_size - used
    }
}

// Implement Drop for Allocator
impl Drop for DarkAllocator {
    fn drop(&mut self) {
        // Run the cleanup logic of our Drop Stack.
        // # Panics: If the drop implementation of some item tried to `allocate()`
        // or touch `drop_stack` in some way, it will result in a panic.
        self.drop_stack.borrow_mut().cleanup();

        // Free the memory pool (arena) allocated at initialization
        unsafe {
            // This is safe because all conditions are checked.
            let layout = Layout::from_size_align_unchecked(self.arena_size, 16);
            dealloc(self.arena_base.as_ptr(), layout);
        }
    }
}