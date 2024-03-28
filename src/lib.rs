#![allow(unused)]
#![allow(dead_code)]

use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::{atomic, Arc},
    thread,
};

/// Represents the state of a Seat in the circular buffer.
struct SeatState<T> {
    max: usize,
    val: Option<T>,
}

struct MutSeatState<T>(UnsafeCell<SeatState<T>>);

struct AtomicOption<T> {
    ptr: atomic::AtomicPtr<T>,
    _marker: PhantomData<Option<Box<T>>>,
}

/// A seat represents a single location in the circurlar buffer.
struct Seat<T> {
    read: atomic::AtomicUsize,
    state: MutSeatState<T>,
    waiting: AtomicOption<thread::Thread>,
}
