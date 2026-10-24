use core::mem;
use core::ptr;

// This struct stores the important information needed to run the drop
// glue for an item.
pub(crate) struct DropStackItem {
    // Pointer to the item for which to run the drop glue. It has been cast
    // to a thin, untyped pointer so the struct remains unique.
    data: *mut (),
    // Function pointer to the specially baked `drop_wrapper` function
    // that knows how to cast the pointer back to `T` correctly and drop it.
    vdrop: fn(*mut ())
}

// # Safety:
// It is safe to implement Send for DropStackItem because it is safe to run the
// destructor of the item in another thread because it can only store types
// which are Send (handled by the `allocate<T>()` function of the `DarkAllocator`) and
// by the `push()` method of `DropStack`
unsafe impl Send for DropStackItem {}

pub(crate) struct DropStack {
    // Vector acting as a stack (LIFO structure) which stores instances of DropStackItem.
    stack: Vec<DropStackItem>
}

impl DropStack {
    // Create a new `DropStack` instance (empty vector)
    pub fn new() -> Self {
        Self { 
            stack: Vec::new()
        }
    }

    // Push a new item to the stack only if it needs dropping.
    // # Safety: the `item` must be and remain a valid pointer and should not be
    // dropped anywhere else before this stack runs its cleanup logic.
    pub unsafe fn push<T: Send>(&mut self, item: *mut T) {
        if mem::needs_drop::<T>() {
            // Wrapper function that knows how to call drop for type T
            fn drop_wrapper<T>(ptr: *mut ()) {
                unsafe { ptr::drop_in_place(ptr as *mut T); }
            }

            // Push the item to the top of the Stack (back of the vector)
            self.stack.push(
                DropStackItem {
                    data: item as *mut (),
                    // `::<T>` is important here so it knows how to perform casting correctly
                    vdrop: drop_wrapper::<T>
                }
            );
        }
    }

    // Run the cleanup
    pub fn cleanup(&mut self) {
        // Pop items off the stack in reverse order (Last In, First Out)
        while let Some(item) = self.stack.pop() {
            // Run the `vdrop` function for the item which will run the `drop_in_place`
            // correctly for the data by casting it back to its real type.
            (item.vdrop)(item.data);
        }
    }
}

// Implement Drop for DropStack
impl Drop for DropStack {
    fn drop(&mut self) {
        self.cleanup();
    }
}