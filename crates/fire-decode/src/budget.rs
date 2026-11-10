//! The process-wide thread budget (FD8).
//!
//! One counter of busy threads, capped at the core count, shared by every decode in the process:
//! - a decode's calling thread holds a [`Seat`] for the whole decode;
//! - a parallel pass, and each FFI call that starts threads of its own (libheif and its codecs, the
//!   PSD bridge), takes a [`Lease`] on what is free beyond that, and runs on the calling thread
//!   alone when nothing is.
//!
//! So one big image gets every core, and twenty concurrent decodes get about one each. No rayon:
//! it is a counter around the scoped threads the crate already used. The budget is per binary;
//! the two Explorer DLLs that can share a surrogate process count separately.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Threads busy on the crate's work right now, callers' threads included.
static BUSY: AtomicUsize = AtomicUsize::new(0);

/// The machine's hardware threads, at least 1.
pub(crate) fn cores() -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    *CORES.get_or_init(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

thread_local! {
    /// Whether this thread is already counted in [`BUSY`]: it holds a [`Seat`], or it is a helper a
    /// [`Lease`] paid for.
    static SEATED: Cell<bool> = const { Cell::new(false) };
    /// This thread's cap on a pass's threads, itself included; 0 = none. Set for a decode from
    /// `DecodeOptions::max_threads`.
    static CAP: Cell<usize> = const { Cell::new(0) };
}

/// The calling thread's own place in the budget, held for one decode. A thread that is already
/// counted (a nested decode, a lease's helper) takes nothing.
pub(crate) struct Seat {
    took: bool,
}

impl Seat {
    pub(crate) fn take() -> Seat {
        if SEATED.get() {
            return Seat { took: false };
        }
        SEATED.set(true);
        BUSY.fetch_add(1, Ordering::AcqRel);
        Seat { took: true }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if self.took {
            BUSY.fetch_sub(1, Ordering::AcqRel);
            SEATED.set(false);
        }
    }
}

/// Threads lent to one pass or FFI call beyond the calling thread, returned on drop.
pub(crate) struct Lease {
    extra: usize,
    // Dropped after `extra` is returned: the calling thread's own seat, if this lease took it.
    _seat: Seat,
}

impl Lease {
    /// The threads the pass may run on, the calling thread included: at least 1.
    pub(crate) fn threads(&self) -> usize {
        1 + self.extra
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.extra > 0 {
            BUSY.fetch_sub(self.extra, Ordering::AcqRel);
        }
    }
}

/// Lease up to `want` threads, the calling thread included, from what the budget has free and
/// under this thread's cap.
pub(crate) fn lease(want: usize) -> Lease {
    let seat = Seat::take();
    let cap = match CAP.get() {
        0 => usize::MAX,
        cap => cap,
    };
    let want_extra = want.min(cap).saturating_sub(1);
    let mut extra = 0;
    if want_extra > 0 {
        let cores = cores();
        let mut busy = BUSY.load(Ordering::Acquire);
        loop {
            let take = want_extra.min(cores.saturating_sub(busy));
            if take == 0 {
                break;
            }
            match BUSY.compare_exchange_weak(busy, busy + take, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    extra = take;
                    break;
                }
                Err(now) => busy = now,
            }
        }
    }
    Lease { extra, _seat: seat }
}

/// Run `f` with this thread's passes capped at `n` threads, itself included (0 = no cap). The
/// previous cap comes back even if `f` unwinds.
pub(crate) fn with_cap<R>(n: usize, f: impl FnOnce() -> R) -> R {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAP.set(self.0);
        }
    }
    let _restore = Restore(CAP.replace(n));
    f()
}

/// Run `work` on a helper thread a lease paid for: counted already, and kept from leasing more
/// (a nested pass would otherwise take a second share of the same cores).
fn as_helper(work: impl FnOnce()) {
    SEATED.set(true);
    CAP.set(1);
    work();
}

/// The least work worth a thread of its own: below it the spawn costs more than the thread saves.
/// Under 1 MiB a pass runs on the calling thread.
const MIN_BYTES_PER_THREAD: usize = 512 << 10;

/// The most threads one pass over memory takes ([`par_chunks_mut`], [`par_chunks`]): widening,
/// interleaving, narrowing, the alpha scan. Those are bound by memory, not by the cores: measured
/// on the M3 Max, 14 threads gained nothing over 8 on 64-256 MiB buffers (a 4096² EXR and PNG, an
/// 8192² PSD) and cost 0.05 ms on 8 MiB ones, a 3% loss on a 2 ms TGA. So such a pass takes at
/// most 8, as before the budget. The work bound by computation takes every core the budget has
/// free: the colour transform and the area filter ([`par_chunks_mut_heavy`]), and libheif and its
/// codecs, which lease them for the whole call. Together those made a 48 MP iPhone HEIC 20%
/// faster.
const MAX_PASS_THREADS: usize = 8;

/// How many parts a pass is cut into per thread. More parts than threads, taken from a shared
/// queue, so a slower core (an efficiency core) ends up doing fewer of them instead of holding the
/// pass up.
const PARTS_PER_THREAD: usize = 4;

/// Run `f` over `buf` cut into contiguous parts, each a whole number of `unit`-byte items, on as
/// many threads as the budget lends; `f` also gets the part's byte offset into `buf`.
pub(crate) fn par_chunks_mut(buf: &mut [u8], unit: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    let want = (buf.len() / MIN_BYTES_PER_THREAD).min(MAX_PASS_THREADS);
    run_chunks_mut(buf, unit, want, f);
}

fn run_chunks_mut(buf: &mut [u8], unit: usize, want: usize, f: impl Fn(usize, &mut [u8]) + Sync) {
    if want < 2 {
        f(0, buf);
        return;
    }
    let lease = lease(want);
    let threads = lease.threads();
    if threads < 2 {
        f(0, buf);
        return;
    }
    let per = part_size(buf.len(), threads, unit);
    let queue = Mutex::new(buf.chunks_mut(per).enumerate());
    let work = || {
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some((i, part)) = next else { return };
            f(i * per, part);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(|| as_helper(work));
        }
        work();
    });
}

/// [`par_chunks_mut`] for work bound by computation rather than memory (lcms2's ~9 ns a pixel, the
/// area filter's weighted sums): every core the budget has free.
pub(crate) fn par_chunks_mut_heavy(
    buf: &mut [u8],
    unit: usize,
    f: impl Fn(usize, &mut [u8]) + Sync,
) {
    run_chunks_mut(buf, unit, buf.len() / MIN_BYTES_PER_THREAD, f);
}

/// [`par_chunks_mut`] for a buffer that is only read.
pub(crate) fn par_chunks(buf: &[u8], unit: usize, f: impl Fn(usize, &[u8]) + Sync) {
    let want = (buf.len() / MIN_BYTES_PER_THREAD).min(MAX_PASS_THREADS);
    if want < 2 {
        f(0, buf);
        return;
    }
    let lease = lease(want);
    let threads = lease.threads();
    if threads < 2 {
        f(0, buf);
        return;
    }
    let per = part_size(buf.len(), threads, unit);
    let queue = Mutex::new(buf.chunks(per).enumerate());
    let work = || {
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some((i, part)) = next else { return };
            f(i * per, part);
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads {
            scope.spawn(|| as_helper(work));
        }
        work();
    });
}

/// A part size for `len` bytes over `threads`: [`PARTS_PER_THREAD`] parts each, a whole number of
/// `unit`-byte items, never empty.
fn part_size(len: usize, threads: usize, unit: usize) -> usize {
    let unit = unit.max(1);
    len.div_ceil(threads * PARTS_PER_THREAD)
        .div_ceil(unit)
        .max(1)
        * unit
}

#[cfg(test)]
pub(crate) fn busy() -> usize {
    BUSY.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests run on parallel threads and share the counter, so the budget is checked against
    /// itself, not against zero.
    #[test]
    fn a_lease_is_bounded_by_cores_and_returned() {
        let before = busy();
        {
            let lease = lease(usize::MAX);
            assert!(lease.threads() >= 1 && lease.threads() <= cores());
            assert!(busy() >= before + lease.threads() - 1);
        }
        // Other tests may hold leases of their own; ours is gone.
        assert!(busy() <= before + cores());
    }

    #[test]
    fn the_cap_bounds_a_lease() {
        with_cap(1, || assert_eq!(lease(64).threads(), 1));
        with_cap(2, || assert!(lease(64).threads() <= 2));
    }

    #[test]
    fn a_seat_is_taken_once_per_thread() {
        std::thread::spawn(|| {
            let before = busy();
            let a = Seat::take();
            let b = Seat::take();
            assert!(a.took && !b.took);
            drop(b);
            assert!(SEATED.get());
            drop(a);
            assert!(!SEATED.get());
            let _ = before;
        })
        .join()
        .unwrap();
    }

    #[test]
    fn par_chunks_mut_covers_every_byte_once() {
        let mut buf = vec![0u8; (8 << 20) + 12];
        par_chunks_mut(&mut buf, 4, |offset, part| {
            assert_eq!(offset % 4, 0);
            for (i, b) in part.iter_mut().enumerate() {
                *b = ((offset + i) % 251) as u8 + 1;
            }
        });
        assert!(
            buf.iter()
                .enumerate()
                .all(|(i, &b)| b == (i % 251) as u8 + 1)
        );
    }

    #[test]
    fn part_size_is_whole_units() {
        assert_eq!(part_size(1000, 2, 16) % 16, 0);
        assert!(part_size(1, 8, 16) >= 16);
        assert_eq!(part_size(0, 8, 1), 1);
    }
}
