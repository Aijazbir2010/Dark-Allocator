# DarkAllocator 🌑

A high-performance, fixed-capacity, custom arena (bump) allocator written in unsafe Rust. Built as a deep-dive systems engineering project after mastering the *Rustonomicon* to explore raw memory management, pointer alignment, provenance preservation, and custom drop glue.

---

## Features

- **Blazing Fast Allocations:** Zero runtime overhead or global allocator calls (`malloc`/`box`) after initial setup. Subsequent allocations are performed purely via pointer arithmetic.
- **Type-Erased Drop Glue (`DropStack`):** Solves the classic arena allocator cleanup problem by safely tracking and executing destructors in strict **LIFO (Last-In, First-Out)** order upon reset or scope exit, without requiring runtime type tags per allocation.
- **Strict Alignment Enforcement:** Automatically calculates and applies padding to ensure every allocation strictly respects the alignment requirements of type `T`.
- **Safety & Soundness Guarantees:** 
  - Validates minimum (4KB) and maximum (4MB) initialization bounds.
  - Implements checked arithmetic (`checked_add`) to prevent buffer overflows.
  - Explicitly bounds thread safety via `Send` compliance while preventing shared multi-threaded mutation (`!Sync`).

---

## Architecture & How It Works

`DarkAllocator` requests a single contiguous block of raw memory from the operating system via `std::alloc::alloc` during initialization. 

1. Allocation (`allocate<T>`): Computes the necessary alignment offset from the current bump pointer, verifies that the allocation fits within the arena boundaries, writes the value using `ptr::write` to prevent uninitialized memory drops, and advances the cursor.

2. Cleanup (`DropStack`): For types implementing `Drop`, a function pointer wrapper (`vdrop`) is pushed to an internal stack, allowing the arena to safely invoke `ptr::drop_in_place` on mixed types when `reset()` or `drop()` is called.

## Usage Example

```rust
use dark_allocator::allocator::DarkAllocator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize an allocator with a custom size (e.g., 4KB minimum)
    let mut allocator = DarkAllocator::new(Some(4096))?;

    // Allocate values directly in the arena
    let x = allocator.allocate(42u32)?;
    let y = allocator.allocate(3.14f64)?;
    let text = allocator.allocate(String::from("Hello, Arena!"))?;

    println!("Allocated value: {}", *x);
    println!("Used capacity: {} bytes", allocator.used_capacity());

    // Instantly wipe the arena and run destructors in LIFO order
    allocator.reset();
    assert_eq!(allocator.used_capacity(), 0);

    Ok(())
}
```

## Running Tests & Miri
To run the comprehensive unit test suite:

```bash
cargo test
```

To verify memory safety, provenance, and absence of Undefined Behavior using Miri:

```bash
rustup +nightly component add miri
cargo +nightly miri test
```