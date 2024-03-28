#![allow(unused)]
#![allow(dead_code)]

use std::cell::UnsafeCell;

/// Represents the state of a Seat in the circular buffer.
struct SeatState<T> {
    max: usize,
    val: Option<T>,
}

struct MutSeatState<T>(UnsafeCell<SeatState<T>>);

