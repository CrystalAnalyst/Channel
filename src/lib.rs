#![allow(unused)]
#![allow(dead_code)]

use std::{
    cell::UnsafeCell,
    marker::PhantomData,
    sync::{atomic, mpsc, Arc},
    thread::{self, Thread},
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

/// `Bus` is the core data structure.
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

pub struct BusReader<T> {
    bus: Arc<BusInner<T>>,
    head: usize,
    leaving: (mpsc::Sender<usize>),
    waiting: mpsc::Receiver<(Thread, usize)>,
    closed: bool,
}

pub struct BusIter<'a, T>(&'a mut BusReader<T>);

pub struct BusIntoIter<T>(BusReader<T>);
