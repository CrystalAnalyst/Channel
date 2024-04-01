#![allow(unused)]
#![allow(dead_code)]
#![allow(dropping_references)]

use core::time;
use crossbeam_channel as mpsc;
use parking_lot_core::SpinWait;
use std::f32::MIN_EXP;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::{
    cell::UnsafeCell,
    fmt::Debug,
    marker::PhantomData,
    sync::{atomic, mpsc as std_mpsc, Arc},
    thread::{self, Thread},
};
use std::{fmt, ptr};

const SPINTIME: u32 = 100_000; // ns

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

impl<T: Clone + Sync> Seat<T> {
    fn take(&self) -> T {
        // read the state and validate the current readerCount.
        let read = self.read.load(atomic::Ordering::Acquire);
        let state = unsafe { &*self.state.get() };
        assert!(read < state.max, " the number of readers exceeds!");
        // value extraction and notification(to the writers)
        let mut waiting = None;
        let v = if read + 1 == state.max {
            waiting = self.waiting.take();
            unsafe { &mut *self.state.get() }.val.take().unwrap()
        } else {
            let v = state.val.clone().expect("there should be value but not!");
            drop(state);
            v
        };
        // increment the count and writer notify
        self.read.fetch_add(1, atomic::Ordering::AcqRel);
        if let Some(t) = waiting {
            t.unpark();
        }
        v
    }
}

/// `BusInner` encapsulates data, which can be accessed by both the writers and readers.
struct BusInner<T> {
    /*------Ring buffer--------*/
    ring: Vec<Seat<T>>,
    len: usize,

    /*-----Pos Tracking--------*/
    tail: atomic::AtomicUsize, // Indicate the index where the nxt write will occur.

    /*----State Management-----*/
    closed: atomic::AtomicBool, // the state of the Bus.
}

impl<T> Debug for BusInner<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusInner")
            .field("ring", &self.ring)
            .field("len", &self.len)
            .field("tail", &self.tail)
            .field("closed", &self.closed)
            .finish()
    }
}

/// `Bus` is the core data structure.
struct Bus<T> {
    /*-------------Core State Management-----------*/
    state: Arc<BusInner<T>>, // holds the inner state of the bus, including the data being transmitted.

    /*-------------Reader Management---------------*/
    readers: usize,    // Number of current Active readers.
    rleft: Vec<usize>, // tracks readers that should be skipped for each index.
    leaving: (mpsc::Sender<usize>, mpsc::Receiver<usize>), // used by receivers to signal when they are done.
    waiting: (
        mpsc::Sender<(thread::Thread, usize)>,
        mpsc::Receiver<(thread::Thread, usize)>,
    ), // used by receivers to signal when they're wating for new entries
    unpark: mpsc::Sender<thread::Thread>, // Send to unparker to wake up parking threads.
    cache: Vec<(thread::Thread, usize)>, // caching to keep track of threads waiting for the next write.
}

impl<T> Bus<T> {
    pub fn new(mut len: usize) -> Bus<T> {
        use std::iter;

        // Set Inner state, ring buffer must have room for one padding element.
        len += 1;
        let inner = Arc::new(BusInner {
            ring: (0..len).map(|_| Seat::default()).collect(),
            tail: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            len,
        });

        // unparking threads Asynchrounously.
        let (unpark_tx, unpark_rx) = mpsc::unbounded::<thread::Thread>();
        let _ = thread::Builder::new()
            .name("bus_unparking".to_owned())
            .spawn(move || {
                // listens for unpark requests on the receiver channel (unpark_rx)
                // and unparks the corresponding threads
                for t in unpark_rx.iter() {
                    t.unpark();
                }
            });

        // return the Assembling of all the components.
        Bus {
            state: inner,
            readers: 0,
            rleft: iter::repeat(0).take(len).collect(),
            leaving: mpsc::unbounded(),
            waiting: mpsc::unbounded(),
            unpark: unpark_tx,
            cache: Vec::new(),
        }
    }

    /// get the expected number of reads for the given seat(at)
    #[inline]
    fn expect(&mut self, at: usize) -> usize {
        // get the Max number of expected reads for given seat.
        let max_reads = unsafe { &*self.state.ring[at].state.get() }.max;
        // substract the number of reads that should be skipped.
        let adjusted_reads = max_reads - self.rleft[at];

        adjusted_reads
    }

    /* ---------------BroadCast Interface---------------- */
    fn broadcast_inner(&mut self, val: T, block: bool) -> Result<(), T> {
        // 1. Initializatio and Set up
        let tail = self.state.tail.load(atomic::Ordering::Relaxed);
        let fence = (tail + 1) % self.state.len;
        let spintime = time::Duration::new(0, SPINTIME);
        let mut sw = SpinWait::new();
        // 2. Main Loop for preparing the necessity before writing.
        loop {
            let fence_read = self.state.ring[fence].read.load(atomic::Ordering::Acquire);
            if fence_read == self.expect(fence) {
                break;
            }

            while let Ok(mut left) = self.leaving.1.try_recv() {
                self.readers -= 1;
                while left != tail {
                    self.rleft[left] += 1;
                    left = (left + 1) % self.state.len;
                }
            }

            if fence_read == self.expect(fence) {
                break;
            } else if block {
                // 3. Handle Blocking
                self.state.ring[fence]
                    .waiting
                    .swap(Some(Box::new(thread::current())));
                self.state.ring[fence]
                    .read
                    .fetch_add(0, atomic::Ordering::Release);
                if !sw.spin() {
                    thread::park_timeout(spintime);
                }
                continue;
            } else {
                // 4. Error Handling
                return Err(val);
            }
        }
        // 5. Writing to the Bus
        let readers = self.readers;
        {
            let next = &self.state.ring[tail];
            let state = unsafe { &mut *next.state.get() };
            state.max = readers;
            state.val = Some(val);
            // here are the new value, so clean the `waiting` field of the `next` seat,
            // ensures that any parked threads with the seat are unblocked.
            next.waiting.take();
            // resets the `read` counter of the `next` to 0.
            // ensure they can accurately determine when they have consumed the value by the writer.
            next.read.store(0, atomic::Ordering::Release);
        }
        // 6. Unblocks waiting threads after Broadcast operation.
        while let Ok((t, at)) = self.waiting.1.try_recv() {
            if at == tail {
                // threads waiting for the current tail index are being added to a chche.
                // because these threads are waiting for the *next* broadcast.
                self.cache.push((t, at));
            } else {
                // others are sent an unpark signal.
                self.unpark.send(t).unwrap();
            }
        }
        // all waiting threads are notified accordingly.
        for w in self.cache.drain(..) {
            self.waiting.0.send(w).unwrap();
        }

        Ok(())
    }

    pub fn try_broadcast(&mut self, val: T) -> Result<(), T> {
        self.broadcast_inner(val, false)
    }

    pub fn broadcast(&mut self, val: T) {
        if let Err(_) = self.broadcast_inner(val, true) {
            unreachable!("broadcast without Blocking Cannot Fail!");
        }
    }

    /* -----------Consumer(Reciver) Management------------- */
    pub fn add_rx(&mut self) -> BusReader<T> {
        self.readers += 1;
        BusReader {
            bus: Arc::clone(&self.state),
            head: self.state.tail.load(atomic::Ordering::Relaxed),
            leaving: self.leaving.0.clone(),
            waiting: self.waiting.0.clone(),
            closed: false,
        }
    }

    pub fn rx_count(&self) -> usize {
        self.readers - self.leaving.1.len()
    }
}

/// a receiver of messages from the bus.
pub struct BusReader<T> {
    /*-------------Core State Management-----------*/
    bus: Arc<BusInner<T>>, // holds a reference to the inner state of the bus that the reader is reading from.

    /*---------Reader Position and Status----------*/
    head: usize,  // points to the next position to be read.
    closed: bool, // indicates whether the reader has been closed.

    /*---------Signaling and Communication-------- */
    leaving: (mpsc::Sender<usize>),
    waiting: mpsc::Sender<(Thread, usize)>,
}

#[derive(Clone, Copy)]
pub enum RecvCondition {
    Try,
    Block,
    Timeout(time::Duration),
}

impl<T: Clone + Debug> BusReader<T> {
    fn recv_inner(&mut self, block: RecvCondition) -> Result<T, std_mpsc::RecvTimeoutError> {
        todo!()
    }

    pub fn try_recv(&mut self) -> Result<T, std_mpsc::TryRecvError> {
        todo!()
    }

    pub fn recv(&mut self) -> Result<T, std_mpsc::RecvError> {
        todo!()
    }

    pub fn recv_timeout(
        &mut self,
        timeout: time::Duration,
    ) -> Result<T, std_mpsc::RecvTimeoutError> {
        todo!()
    }
}

pub struct BusIter<'a, T>(&'a mut BusReader<T>);

pub struct BusIntoIter<T>(BusReader<T>);
