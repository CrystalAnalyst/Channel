#![allow(unused)]
#![allow(dead_code)]
#![allow(dropping_references)]

use core::time;
use crossbeam_channel as mpsc;
use mpsc::RecvTimeoutError;
use mpsc::{Receiver, Sender};
use parking_lot_core::SpinWait;
use std::f32::MIN_EXP;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;
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

impl<T> SeatState<T> {
    /// for testing.
    pub fn new() -> Self {
        Self { max: 0, val: None }
    }
}

/// Using UnsafeCell to realize `interior mutability`
/// which allows multiple `&mut T` to modify the data(SeatState).
struct MutSeatState<T>(UnsafeCell<SeatState<T>>);

/// impl Sync trait for SeatState to safely shared between threads.
/// Why unsafe? cause we(developers) neet to ensure that the `T` is `Sync`.
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn test_seat_state() {
        let mut state = SeatState { max: 10, val: None };
        assert_eq!(state.max, 10);
        assert_eq!(state.val, None);
        state.val = Some(5);
        assert_eq!(state.max, 10);
        assert_eq!(state.val, Some(5));
    }

    #[test]
    fn test_sync_impl() {
        let state = MutSeatState(UnsafeCell::new(SeatState::new()));
        // Test that Sync trait is implemented correctly
        let shared_state: &Mutex<MutSeatState<i32>> = &Mutex::new(state);
        let mut shared_guard = shared_state.lock().unwrap();
        // Perform some operations on the shared state
        // ...
        drop(shared_guard);
    }
}

struct AtomicOption<T> {
    /// Allows for atomic operations on raw pointers(*mut T)
    ptr: atomic::AtomicPtr<T>,
    /// The PhantomData<Option<Box<T>>> marker
    /// indicates that AtomicOption logically owns a Box<T>(a heap-allocated T),
    /// although the actual storage is managed by the raw pointer.
    _marker: PhantomData<Option<Box<T>>>,
}

impl<T> fmt::Debug for AtomicOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicOption")
            .field("ptr", &self.ptr)
            .finish()
    }
}

/// Ensures when an AtomicOption is dropped, any contained value is properly deallocated.
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
    /// Initializes an AtomicOption with a null pointer, representing None.
    fn empty() -> Self {
        Self {
            ptr: atomic::AtomicPtr::new(ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// Swaps the value stored in the `AtomicPtr<T>`
    /// with a new value and returns the old value.
    fn swap(&self, val: Option<Box<T>>) -> Option<Box<T>> {
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

    /// Atomically replaces the current pointer
    /// with a null pointer and returns the old value.
    fn take(&self) -> Option<Box<T>> {
        self.swap(None)
    }
}

/// A seat represents a single location in the circurlar buffer.
struct Seat<T> {
    state: MutSeatState<T>,
    read: atomic::AtomicUsize,
    waiting: AtomicOption<Thread>,
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
            state: MutSeatState(UnsafeCell::new(SeatState::new())),
            read: atomic::AtomicUsize::new(0),
            waiting: AtomicOption::empty(),
        }
    }
}

impl<T: Clone + Sync> Seat<T> {
    /// Allow a reader(`BusReader`) to extract "a copy of" the value stored in a Seat.
    fn take(&self) -> T {
        let read = self.read.load(atomic::Ordering::Acquire);
        let state = unsafe { &*self.state.get() };
        assert!(read < state.max, " the number of readers exceeds!");
        let mut waiting = None;
        // If the current reader is the last reader (read + 1 == state.max),
        // It'll retrieves the thread waiting for this Seat to become available again,
        // if any, and unparks it.
        let v = if read + 1 == state.max {
            waiting = self.waiting.take();
            // remove the value for the last reader.
            unsafe { &mut *self.state.get() }.val.take().unwrap()
        } else {
            let v = state.val.clone().expect("there should be value but not!");
            drop(state);
            v
        };
        self.read.fetch_add(1, atomic::Ordering::AcqRel);
        if let Some(t) = waiting {
            t.unpark();
        }
        v
    }
}

/// `BusInner` encapsulates data, which can be accessed by both the writers and readers.
struct BusInner<T> {
    ring: Vec<Seat<T>>,
    len: usize,
    tail: atomic::AtomicUsize,
    closed: atomic::AtomicBool,
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
pub struct Bus<T> {
    /// holds the inner state of the bus.
    state: Arc<BusInner<T>>,
    ///
    /// Number of current Active readers.
    readers: usize,
    ///
    /// Tracks numbers of read miss(readers leave before they actually read) for each slot in the ring buffer.
    rleft: Vec<usize>,
    ///
    /// An unbounded channel for readers that are waiting for new broadcasts.
    waiting: (Sender<(Thread, usize)>, Receiver<(Thread, usize)>),
    ///
    /// An unbounded channel for readers that are leaving, to signal their positions.
    leaving: (Sender<usize>, Receiver<usize>),
    ///
    /// This `unpark` is designed to handle this situation:
    /// when a `BusReader` wanna read data at a specific place, but there's no data.
    /// At this time it'll block himself(using thread::park()) and waits for writer to write data,
    /// and when the data is available, the `bus` unpark all the reader threads that has been parked.
    /// This enables the bus to wake up waiting readers when new data is available or when the bus is closing.
    unpark: Sender<Thread>,
    /// Temporarily stores reader threads(waiting for the next write) that need to be unparked.
    /// This helps in managing and batching thread wake-ups efficiently.
    cache: Vec<(Thread, usize)>,
}

impl<T> fmt::Debug for Bus<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bus")
            .field("state", &self.state)
            .field("readers", &self.readers)
            .field("rleft", &self.rleft)
            .field("leaving", &self.leaving)
            .field("waiting", &self.waiting)
            .field("unpark", &self.unpark)
            .field("cache", &self.cache)
            .finish()
    }
}

impl<T> Bus<T> {
    /// Allocates a new 'Bus'
    /// @ param: len, you can customize the capacity of Ringbuffer.
    pub fn new(mut len: usize) -> Bus<T> {
        use std::iter;

        len += 1;
        let inner = Arc::new(BusInner {
            ring: (0..len).map(|_| Seat::default()).collect(),
            tail: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            len,
        });

        let (unpark_tx, unpark_rx) = mpsc::unbounded::<Thread>();
        let _ = thread::Builder::new()
            .name("unparking_thread".to_owned())
            .spawn(move || {
                for t in unpark_rx.iter() {
                    t.unpark();
                }
            });

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

    /// The `expect()` method returns the adjusted read count,
    /// which is the expected number of reads for the given position after considering the missed reads.
    /// when `expect(at)` == `ring[at].read` => the `Seat(ring[at])` is free,
    /// and can be written with new value by writer(by broadcast).
    #[inline]
    fn expect(&mut self, at: usize) -> usize {
        // get the Max number of expected reads for given seat.
        let max_reads = unsafe { &*self.state.ring[at].state.get() }.max;
        // substract the number of reads that should be skipped.
        let adjusted_reads = max_reads - self.rleft[at];

        adjusted_reads
    }

    /* ---------------BroadCast(Writing) Interface---------------- */
    /// Attempts to place the given value on the bus.
    ///
    /// If the bus is full, the behavior depends on `block`.
    /// If false(Nonblocking), the value given is returned in an `Err()`.
    /// Otherwise, the current thread will be parked until there is space in the bus.
    /// again, and the broadcast will be tried again until it succeeds.
    ///
    /// Note that broadcasts will succeed even if there are no consumers!
    fn broadcast_inner(&mut self, val: T, block: bool) -> Result<(), T> {
        let tail = self.state.tail.load(atomic::Ordering::Relaxed);
        let fence = (tail + 1) % self.state.len;
        let spintime = time::Duration::new(0, SPINTIME);
        let mut sw = SpinWait::new();
        // check if there is space to write(in case the writer overwrite Unread data)
        // and Updating the state when readers leave.
        loop {
            let fence_read = self.state.ring[fence].read.load(atomic::Ordering::Acquire);
            if fence_read == self.expect(fence) {
                break;
            }
            while let Ok(mut left_at) = self.leaving.1.try_recv() {
                self.readers -= 1;
                while left_at != tail {
                    self.rleft[left_at] += 1;
                    left_at = (left_at + 1) % self.state.len;
                }
            }
            if fence_read == self.expect(fence) {
                break;
            } else if block {
                // Registers the current thread in the waiting field so it can be unparked later.
                //
                // [Timing]: When a reader consumes an item from the buffer,
                // it might check the waiting field and unpark any waiting writer, allowing it to continue.
                self.state.ring[fence]
                    .waiting
                    .swap(Some(Box::new(thread::current())));
                // Ensures memory visibility of the above changes.
                self.state.ring[fence]
                    .read
                    .fetch_add(0, atomic::Ordering::Release);
                // The spin method is used to avoid immediately parking the thread if the position might become free soon.
                // [optimization]: To avoid the overhead of parking and unparking if a slot is likely to be freed quickly.
                if !sw.spin() {
                    thread::park_timeout(spintime);
                }
                continue;
            } else {
                return Err(val);
            }
        }
        // Writing to the Bus
        let readers = self.readers;
        {
            let next = &self.state.ring[tail];
            let state = unsafe { &mut *next.state.get() };
            state.max = readers;
            state.val = Some(val);
            // Write Done. Now clean the `waiting` field of this seat,
            // meaning that the parked writer thread can be unparked.
            next.waiting.take();
            // resets the `read` counter of the `next` to 0.
            next.read.store(0, atomic::Ordering::Release);
        }
        self.rleft[tail] = 0;
        // move the tail pointer to the next one.
        let tail = (tail + 1) % self.state.len;
        self.state.tail.store(tail, atomic::Ordering::Release);
        // 6. Unblocks waiting reader threads after Broadcast(Writing) operation.
        // here, we got t and at:
        //       t:  represents Thread that's waiting
        //       at: represents its waiting position in the ring.
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
        // .drain(..) acts full range clears the vector, like `clear()` does
        for w in self.cache.drain(..) {
            self.waiting.0.send(w).unwrap();
        }

        Ok(())
    }

    /// NonBlocking Boardcast(allow the writer thread to fail)
    pub fn try_broadcast(&mut self, val: T) -> Result<(), T> {
        self.broadcast_inner(val, false)
    }

    /// Blocking Boradcast(strongly) ensures that the writer must finish the write(boardcast) Action
    /// If it cannot do it, then panic.
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

    /// Returns the number of active consumers currently attached to this bus.
    /// It is not guaranteed that a sent message will reach this number of consumers, as active
    /// consumers may never call `recv` or `try_recv` again before dropping.
    pub fn rx_count(&self) -> usize {
        self.readers - self.leaving.1.len()
    }
}

impl<T> Drop for Bus<T> {
    fn drop(&mut self) {
        self.state.closed.store(true, atomic::Ordering::Relaxed);
        self.state.tail.fetch_add(0, atomic::Ordering::AcqRel);
    }
}

/// a receiver of messages from the bus.
/// Using crossbeam-channel to Solve 2 Problems:
///     1. When reader wanna read data at its `head` pos but there's no data available yet => waiting.
///     2. When reader finish its read, It will leave, we got to tell the bus. => leaving.
pub struct BusReader<T> {
    /*-------------Core State Management-----------*/
    bus: Arc<BusInner<T>>, // holds a reference to the inner state of the bus that the reader is reading from.

    /*---------Reader Position and Status----------*/
    head: usize,  // points to the next position to be read.
    closed: bool, // indicates whether the reader has been closed.

    /*---------Signaling and Communication-------- */
    /// using leaving to send leaving message to the Bus to infrom.
    leaving: Sender<usize>,
    ///
    /// When a reader intend to read a value from ring[head] but there's no data,
    /// using waiting to send "I'm ThreadXXX, I'm waiting at `head` pos" to bus(writer) to inform.
    waiting: Sender<(Thread, usize)>,
}

impl<T> BusReader<T> {
    /// Returns an iterator that will block waiting for broadcasts.
    /// It will return `None` when the bus has been closed (i.e., the `Bus` has been dropped).
    pub fn iter(&mut self) -> BusIter<'_, T> {
        BusIter(self)
    }
}

impl<T> Debug for BusReader<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BusReader")
            .field("bus", &self.bus)
            .field("head", &self.head)
            .field("closed", &self.closed)
            .field("leaving", &self.leaving)
            .field("waiting", &self.waiting)
            .finish()
    }
}

#[derive(Clone, Copy)]
pub enum RecvCondition {
    Try,
    Block,
    Timeout(time::Duration),
}

/// Readers method to extract value from the ring buffer.
impl<T: Clone + Sync> BusReader<T> {
    fn recv_inner(&mut self, block: RecvCondition) -> Result<T, std_mpsc::RecvTimeoutError> {
        // Status check
        if self.closed {
            return Err(std_mpsc::RecvTimeoutError::Disconnected);
        }

        // variable declaration: mainly time-related.
        let start = match block {
            RecvCondition::Timeout(_) => Some(Instant::now()),
            _ => None,
        };
        let spintime = time::Duration::new(0, SPINTIME);
        let mut was_closed = false;
        let mut sw = SpinWait::new();
        let mut first = true;

        // Before we do the action, check the bus's state.
        /*
           Check by the following Order
               1. Empty or not ? If not empty, just Exit the loop.
               2. If it's empty, whether it's closed or not ?
               3. If it is closed, then return the Err(Disconnected)
               4. If it's not closed, But it's empty, should the reader thread block ?
               5. If it's non-blocking, that is RecvCondition::Try => We return directly.
               6. If it's Block, means that the reader thread will wait the writer thread to place value.
                  Now we need use channel to send to the writer that I(thread::current()) am waiting at self.head
        */
        loop {
            let tail = self.bus.tail.load(atomic::Ordering::Acquire);
            // If not empty, then quit.
            if tail != self.head {
                break;
            }
            // check closed or not ?
            if self.bus.closed.load(atomic::Ordering::Relaxed) {
                if !was_closed {
                    was_closed = true;
                    continue;
                }
                self.closed = true;
                return Err(std_mpsc::RecvTimeoutError::Disconnected);
            }
            // Not closed, if nonblocking => return Timeout.
            if let RecvCondition::Try = block {
                return Err(std_mpsc::RecvTimeoutError::Timeout);
            }
            // Blocking (wait until there's data being placed by writer).
            // park and tell writer to notify on write.
            if first {
                if let Err(..) = self.waiting.send((thread::current(), self.head)) {
                    /// Atomic operations with Release or Acquire semantics can also synchronize with a fence.
                    /// A fence which has SeqCst ordering, in addition to having both Acquire and Release semantics,
                    /// participates in the global program order of the other SeqCst operations and/or fences.
                    atomic::fence(atomic::Ordering::SeqCst);
                    continue;
                }
                first = false;
            }
            // Now, It's empty but I wanna read data from the ringBuffer(bus),
            // And I expect that it won't take too long so I don't wanna the thread to sleep
            // then I figured out that SpinLock is a good choice here.

            // The `spin` method of `SpinWait` returns `true` if the spinning should continue and `false` if it should stop.
            // Typically, it spins for a few iterations and then decides that it's better to yield the thread to avoid wasting CPU cycles.
            // In this context, `if !sw.spin()` checks if the spinning should stop.
            // If it returns `false`, it means the spinning is complete and the thread should now be parked or yield.
            if !sw.spin() {
                match block {
                    RecvCondition::Timeout(t) => {
                        match t.checked_sub(start.as_ref().unwrap().elapsed()) {
                            Some(left) => {
                                if left < spintime {
                                    thread::park_timeout(left);
                                } else {
                                    thread::park_timeout(spintime);
                                }
                            }
                            None => return Err(std_mpsc::RecvTimeoutError::Timeout),
                        }
                    }
                    RecvCondition::Block => thread::park_timeout(spintime),
                    RecvCondition::Try => unreachable!(),
                }
            }
        }

        // There indeed exists available data and I can read it.
        let head = self.head;
        let ret = self.bus.ring[head].take();
        self.head = (head + 1) % self.bus.len;
        Ok(ret)
    }

    /// Non-blocking Interface for BusReader to receive a value
    /// Means that if the ring is empty, I just return immediately.
    pub fn try_recv(&mut self) -> Result<T, std_mpsc::TryRecvError> {
        self.recv_inner(RecvCondition::Try).map_err(|e| match e {
            std_mpsc::RecvTimeoutError::Timeout => std_mpsc::TryRecvError::Empty,
            std_mpsc::RecvTimeoutError::Disconnected => std_mpsc::TryRecvError::Disconnected,
        })
    }

    /// Blocking Interface for BusReader to receive a value.
    /// Blocking Means that if the ring is empty, I wait still until the writer newly write data.
    pub fn recv(&mut self) -> Result<T, std_mpsc::RecvError> {
        match self.recv_inner(RecvCondition::Block) {
            Ok(v) => return Ok(v),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(std_mpsc::RecvError),
            _ => unreachable!("Blocking recv cannot fail!"),
        }
    }

    /// Time bound receive.
    /// If the data is not available now, I wait for a period of time.
    pub fn recv_timeout(
        &mut self,
        timeout: time::Duration,
    ) -> Result<T, std_mpsc::RecvTimeoutError> {
        self.recv_inner(RecvCondition::Timeout(timeout))
    }
}

/* -----------------------------Impl Traits: Drop and Iterator(Into and &mut) for BusReader<T>----------------------- */

/// The method sends the current head position to a channel (self.leaving).
///
/// Bus is notified when a reader is no longer active,
/// helping manage resources and possibly maintaining the correct state of the bus.
///
impl<T> Drop for BusReader<T> {
    fn drop(&mut self) {
        self.leaving.send(self.head);
    }
}

/// The Following Iterator implementations (BusIter and BusIntoIter) provide
/// a convenient way to consume the bus’s values in a loop, using standard iterator methods like next.
///

/// `IntoIterator` allows BusReader to be converted into an iterator.
impl<'a, T: Clone + Sync> IntoIterator for &'a mut BusReader<T> {
    type Item = T;
    type IntoIter = BusIter<'a, T>;
    /// The into_iter method converts self into a BusIter.
    fn into_iter(self) -> Self::IntoIter {
        BusIter(self)
    }
}

/// for owning BusReader<T> (not a reference).
impl<T: Clone + Sync> IntoIterator for BusReader<T> {
    type Item = T;
    type IntoIter = BusIntoIter<T>;
    /// The into_iter method converts self into a BusIntoIter.
    fn into_iter(self) -> Self::IntoIter {
        BusIntoIter(self)
    }
}

/// BusIter is a struct that holds a mutable reference to a BusReader.
pub struct BusIter<'a, T>(&'a mut BusReader<T>);

/// The Iterator trait implementation for BusIter allows it to yield items of type T.
impl<'a, T: Clone + Sync> Iterator for BusIter<'a, T> {
    type Item = T;
    /// The next method attempts to receive a value from the BusReader by calling recv.
    /// If successful, it returns Some(item); otherwise, it returns None.
    fn next(&mut self) -> Option<Self::Item> {
        self.0.recv().ok()
    }
}

/// BusIntoIter is a struct that holds ownership of a BusReader.
pub struct BusIntoIter<T>(BusReader<T>);

/// The Iterator trait implementation for BusIntoIter allows it to yield items of type T.
impl<T: Clone + Sync> Iterator for BusIntoIter<T> {
    type Item = T;
    /// Similar to BusIter, the next method attempts to receive a value
    /// from the BusReader and returns Some(item) if successful or None otherwise.
    fn next(&mut self) -> Option<Self::Item> {
        self.0.recv().ok()
    }
}
