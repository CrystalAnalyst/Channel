#![allow(unused)]
#![allow(dead_code)]

use std::fmt;
use std::ops::Deref;
use std::{
    cell::UnsafeCell, fmt::Debug, marker::PhantomData, sync::{atomic, mpsc, Arc}, thread::{self, Thread}
};

/// Represents the state of a Seat in the circular buffer.
struct SeatState<T> {
    max: usize,
    val: Option<T>,
}

struct MutSeatState<T>(UnsafeCell<SeatState<T>>);

unsafe impl<T> Sync for MutSeatState<T> {}

impl<T> Deref for MutSeatState<T> {
    type Target = UnsafeCell<SeatState<T>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
    
}

impl<T> fmt::Debug for MutSeatState<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MutSeatState").field(&self.0).finish()
    }
}

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

/// `Bus` is the core data structure.
struct Bus<T> {
    state: Arc<BusInner<T>>,
    // current number of readers.
    readers: usize,
    // tracks readers that should be skipped for each index.
    rleft: Vec<usize>,
    // used by receivers to signal when they are done.
    leaving: (mpsc::Sender<usize>, mpsc::Receiver<usize>),
    // used by receivers to signal when they're wating for new entries
    waiting: (
        mpsc::Sender<(thread::Thread, usize)>,
        mpsc::Receiver<(thread::Thread, usize)>,
    ),
    // channel used to communicate to unparker that a given
    // thread should be woken up.
    unpark: mpsc::Sender<thread::Thread>,
    // caching to keep track of threads waiting for the next write.
    cache: Vec<(thread::Thread, usize)>,
}

impl<T> Bus<T> {
    pub fn new(len: usize) -> RES
}

/// a receiver of messages from the bus.
pub struct BusReader<T> {
    bus: Arc<BusInner<T>>,
    // head points to the current position in the bus.
    head: usize,
    // `leaving` always used to signal when reader is done.
    leaving: (mpsc::Sender<usize>),
    waiting: mpsc::Receiver<(Thread, usize)>,
    closed: bool,
}

pub struct BusIter<'a, T>(&'a mut BusReader<T>);

pub struct BusIntoIter<T>(BusReader<T>);
