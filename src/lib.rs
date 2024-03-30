#![allow(unused)]
#![allow(dead_code)]

use std::ops::Deref;
use std::{
    cell::UnsafeCell,
    fmt::Debug,
    marker::PhantomData,
    sync::{atomic, mpsc, Arc},
    thread::{self, Thread},
};
use std::{fmt, ptr};

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
    /// A raw pointer type which can be safely shared between threads.
    /// This type has the same in-memory representation as a *mut T.
    ptr: atomic::AtomicPtr<T>,
    /// Adding a PhantomData<T> field to your type tells the compiler
    /// that your type acts as though it stores a value of type T,
    /// even though it doesn't really.
    /// This information is used when computing certain safety properties.
    _marker: PhantomData<Option<Box<T>>>,
}

impl<T> fmt::Debug for AtomicOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicOption")
            .field("ptr", &self.ptr)
            .finish()
    }
}

impl<T> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

// consider AtomicOption<T> is `Send` as long as the type `T` can be `Send`.
unsafe impl<T: Send> Send for AtomicOption<T> {}
// consider AtomicOption<T> is `Sync` as long as the type `T` can be `Sync`.
unsafe impl<T: Sync> Sync for AtomicOption<T> {}

impl<T> AtomicOption<T> {
    /// create an empty instance of AtomicOption.
    fn empty() -> Self {
        Self {
            ptr: atomic::AtomicPtr::new(ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// swaps the value stored in the `AtomicPtr<T>`
    /// with a new value and returns the old value.
    fn swap(&self, val: Option<Box<T>>) -> Option<Box<T>> {
        // If the val is Some(), swaps the boxed value into the ptr.
        // else, swaps a null pointer into the ptr.
        let old = match val {
            Some(val) => self.ptr.swap(Box::into_raw(val), atomic::Ordering::AcqRel),
            None => self.ptr.swap(ptr::null_mut(), atomic::Ordering::Acquire),
        };
        if old.is_null() {
            None
        } else {
            Some(unsafe { Box::from_raw(old) })
        }
    }

    /// swap with param:`None`, which means
    /// just return the value stored in the AtomicPtr<T>.
    fn take(&self) -> Option<Box<T>> {
        self.swap(None)
    }
}

/// A seat represents a single location in the circurlar buffer.
struct Seat<T> {
    read: atomic::AtomicUsize,
    state: MutSeatState<T>,
    waiting: AtomicOption<thread::Thread>,
}

impl<T> fmt::Debug for Seat<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Seat")
            .field("read", &self.read)
            .field("state", &self.state)
            .field("wating", &self.waiting)
            .finish()
    }
}

impl<T> Default for Seat<T> {
    fn default() -> Self {
        Seat {
            read: atomic::AtomicUsize::new(0),
            waiting: AtomicOption::empty(),
            state: MutSeatState(UnsafeCell::new(SeatState { max: 0, val: None })),
        }
    }
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

impl<T> Bus<T> {}

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
