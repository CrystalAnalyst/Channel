#![allow(unused)]
#![allow(dead_code)]

use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::{atomic, mpsc, Arc},
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

/// `BusInner` encapsulates data, which can be accessed by both the writers and readers.
struct BusInner<T> {
    ring: Vec<Seat<T>>,
    len: usize,
    tail: atomic::AtomicUsize,
    closed: atomic::AtomicBool,
}

struct Bus<T> {
    state: Arc<BusInner<T>>,

    readers: usize,
    rleft: Vec<usize>,

    leaving: (mpsc::Sender<usize>, mpsc::Receiver<usize>),
    waiting: (
        mpsc::Sender<(thread::Thread, usize)>,
        mpsc::Receiver<(thread::Thread, usize)>,
    ),

    unpark: mpsc::Sender<thread::Thread>,
    cache: Vec<(thread::Thread, usize)>,
}
