extern crate channel;

use std::sync::mpsc;
use std::time;

/* ----------------Basic Functionality:SPMC--------------------*/
#[test]
/// Tests basic functionality of
/// broadcasting a message and receiving it with two receivers.
fn it_works() {
    let mut c = channel::Bus::new(10);
    let mut r1 = c.add_rx();
    let mut r2 = c.add_rx();
    assert_eq!(c.try_broadcast(true), Ok(()));
    assert_eq!(r1.try_recv(), Ok(true));
    assert_eq!(r2.try_recv(), Ok(true));
}

#[test]
/// Tests the Debug trait implementation for the Bus and a receiver.
fn debug() {
    let mut c = channel::Bus::new(10);
    println!("{:?}", c);
    let mut r = c.add_rx();
    println!("{:?}", c);
    println!("{:?}", r);
    assert_eq!(c.try_broadcast(true), Ok(()));
    println!("{:?}", c);
    assert_eq!(r.try_recv(), Ok(true));
    println!("{:?}", c);
}

#[test]
///  Similar to debug, but tests with a non-Debug type
/// to ensure the outer structure can still be debug-printed.
fn debug_not_inner() {
    // Foo does not implement Debug
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Foo;

    let mut c = channel::Bus::new(10);
    println!("{:?}", c);
    let mut r = c.add_rx();
    println!("{:?}", c);
    println!("{:?}", r);
    assert!(matches!(c.try_broadcast(Foo), Ok(())));
    println!("{:?}", c);
    assert!(matches!(r.try_recv(), Ok(Foo)));
    println!("{:?}", c);
}

/* ----------------Capacity Handling--------------------*/
#[test]
/// Nonblocking: Ensures that broadcasting fails when the channel is full.
fn it_fails_when_full() {
    let mut c = channel::Bus::new(1);
    let r1 = c.add_rx();
    assert_eq!(c.try_broadcast(true), Ok(()));
    assert_eq!(c.try_broadcast(false), Err(false));
    drop(r1);
}

#[test]
/// Tests successful broadcasting after the channel has been emptied.
fn it_succeeds_when_not_full() {
    let mut c = channel::Bus::new(1);
    let mut r1 = c.add_rx();
    assert_eq!(c.try_broadcast(true), Ok(()));
    assert_eq!(c.try_broadcast(false), Err(false));
    assert_eq!(r1.try_recv(), Ok(true));
    assert_eq!(c.try_broadcast(true), Ok(()));
}

/* ----------------Empty and Full States--------------------*/
#[test]
/// Ensures that attempting to receive from an empty channel results in an error.
fn it_fails_when_empty() {
    let mut c = channel::Bus::<bool>::new(10);
    let mut r1 = c.add_rx();
    assert_eq!(r1.try_recv(), Err(mpsc::TryRecvError::Empty));
}

#[test]
/// Tests that a message can be successfully read from a full channel.
fn it_reads_when_full() {
    let mut c = channel::Bus::new(1);
    let mut r1 = c.add_rx();
    assert_eq!(c.try_broadcast(true), Ok(()));
    assert_eq!(r1.try_recv(), Ok(true));
}

/* ----------------Testing Iteration--------------------*/
#[test]
#[cfg_attr(miri, ignore)]
/// Tests the iterator functionality of the receiver
/// by broadcasting multiple messages and receiving them in sequence.
fn it_iterates() {
    use std::thread;

    let mut tx = channel::Bus::new(2);
    let mut rx = tx.add_rx();

    // broadcast multiple messages.
    let j = thread::spawn(move || {
        for i in 0..1_000 {
            tx.broadcast(i);
        }
    });

    // receive them in sequence.
    let mut ii = 0;
    for i in rx.iter() {
        assert_eq!(i, ii);
        ii += 1;
    }

    j.join().unwrap();
    assert_eq!(ii, 1_000);
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}

#[test]
#[cfg_attr(miri, ignore)]
///  Runs the iteration test multiple times to check for robustness under repeated stress.
fn aggressive_iteration() {
    for _ in 0..1_000 {
        use std::thread;

        let mut tx = channel::Bus::new(2);
        let mut rx = tx.add_rx();
        let j = thread::spawn(move || {
            for i in 0..1_000 {
                tx.broadcast(i);
            }
        });

        let mut ii = 0;
        for i in rx.iter() {
            assert_eq!(i, ii);
            ii += 1;
        }

        j.join().unwrap();
        assert_eq!(ii, 1_000);
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
    }
}

/* ----------------Action after the channel closed--------------------*/
#[test]
///  Ensures that receivers can detect
///  when the channel has been closed and no more messages will be sent.
fn it_detects_closure() {
    let mut tx = channel::Bus::new(1);
    let mut rx = tx.add_rx();
    assert_eq!(tx.try_broadcast(true), Ok(()));
    assert_eq!(rx.try_recv(), Ok(true));
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    drop(tx);
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}

#[test]
/// Tests that messages sent before the channel is closed
/// can still be received after closure.
fn it_recvs_after_close() {
    let mut tx = channel::Bus::new(1);
    let mut rx = tx.add_rx();
    assert_eq!(tx.try_broadcast(true), Ok(()));
    drop(tx);
    assert_eq!(rx.try_recv(), Ok(true));
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}

/* ----------------Receiver Management--------------------*/
#[test]
/// Ensures that the channel functions correctly when receivers are dropped (i.e., removed).
fn it_handles_leaves() {
    let mut c = channel::Bus::new(1);
    let mut r1 = c.add_rx();
    let r2 = c.add_rx();
    assert_eq!(c.try_broadcast(true), Ok(()));
    drop(r2);
    assert_eq!(r1.try_recv(), Ok(true));
    assert_eq!(c.try_broadcast(true), Ok(()));
}

/* --------------Blocking Behavior: writes & reads--------------------*/
#[test]
#[cfg_attr(miri, ignore)]
/// Tests blocking behavior for writing when the channel is full,
/// and unblocks the writer by reading a message.
fn it_runs_blocked_writes() {
    use std::thread;

    let mut c = Box::new(channel::Bus::new(1));
    let mut r1 = c.add_rx();
    c.broadcast(true); // this is fine

    // buffer is now full
    assert_eq!(c.try_broadcast(false), Err(false));
    // start other thread that blocks
    let c = thread::spawn(move || {
        c.broadcast(false);
    });
    // unblock sender by receiving
    assert_eq!(r1.try_recv(), Ok(true));
    // drop r1 to release other thread and safely drop c
    drop(r1);
    c.join().unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
/// Tests blocking behavior for reading when the channel is empty,
/// and unblocks the reader by broadcasting a message.
fn it_runs_blocked_reads() {
    use std::sync::mpsc;
    use std::thread;

    let mut tx = Box::new(channel::Bus::new(1));
    let mut rx = tx.add_rx();
    // buffer is now empty
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    // start other thread that blocks
    let c = thread::spawn(move || {
        rx.recv().unwrap();
    });
    // unblock receiver by broadcasting
    tx.broadcast(true);
    // check that thread now finished
    c.join().unwrap();
}

/* --------------Stress Tesing && Benchmark--------------------*/
#[test]
#[cfg_attr(miri, ignore)]
/// Ensures the channel can handle a high volume of messages (10,000)
/// being sent and received correctly.
fn it_can_count_to_10000() {
    use std::thread;

    let mut c = channel::Bus::new(2);
    let mut r1 = c.add_rx();
    let j = thread::spawn(move || {
        for i in 0..20_000 {
            c.broadcast(i);
        }
    });

    for i in 0..20_000 {
        assert_eq!(r1.recv(), Ok(i));
    }

    j.join().unwrap();
    assert_eq!(r1.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}

#[test]
#[cfg_attr(miri, ignore)]
/// Here I Simulate a more complex scenario with limited channel capacity and multiple receivers
/// consuming different amounts of messages to test concurrency and blocking behavior.
fn test_busy() {
    use std::thread;

    // start a bus with limited space
    let mut bus = channel::Bus::new(1);

    // first receiver only receives 5 items
    let mut rx1 = bus.add_rx();
    let t1 = thread::spawn(move || {
        for _ in 0..5 {
            rx1.recv().unwrap();
        }
        drop(rx1);
    });

    // second receiver receives 10 items
    let mut rx2 = bus.add_rx();
    let t2 = thread::spawn(move || {
        for _ in 0..10 {
            rx2.recv().unwrap();
        }
        drop(rx2);
    });

    // let receivers start
    std::thread::sleep(time::Duration::from_millis(500));

    // try to send 25 items -- should work fine
    for i in 0..25 {
        std::thread::sleep(time::Duration::from_millis(100));
        match bus.try_broadcast(i) {
            Ok(_) => (),
            Err(e) => println!("Broadcast failed {}", e),
        }
    }

    // done sending -- wait for receivers (which should already be done)
    t1.join().unwrap();
    t2.join().unwrap();
    assert!(true);
}
