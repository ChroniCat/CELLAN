//! Persistent global worker thread pool for formula engine parallelism.
//!
//! Eliminates per-pass / per-level `std::thread::scope` OS thread creation overhead
//! while remaining zero-dependency, panic-safe, re-entrancy-safe, and safe for
//! concurrent callers (e.g., parallel tests or simultaneous background recalc + deps build).

use std::cell::Cell;
use std::collections::VecDeque;
#[cfg(panic = "unwind")]
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
#[cfg(panic = "unwind")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

thread_local! {
    static IN_POOL_WORKER: Cell<bool> = const { Cell::new(false) };
}

struct JobState {
    /// Erased pointer to `&(dyn Fn(usize) + Sync)` living on the caller's stack.
    /// Soundness: caller MUST NOT return (even on panic) until `closed == true` and
    /// `helpers_finished == helpers_joined`, guaranteeing the pointed-to closure outlives
    /// all worker invocations.
    func_ptr: *const (),
    trampoline: fn(*const (), usize),
    max_helpers: usize,
    helpers_joined: usize,
    helpers_finished: usize,
    closed: bool,
    #[cfg(panic = "unwind")]
    panicked: AtomicBool,
}

struct JobEntry {
    state: Mutex<JobState>,
    done_cv: Condvar,
}

// Safety: `func_ptr` points to a `Sync` closure valid until the caller waits on `done_cv`.
unsafe impl Send for JobState {}
unsafe impl Sync for JobState {}
unsafe impl Send for JobEntry {}
unsafe impl Sync for JobEntry {}

struct QueueState {
    jobs: VecDeque<Arc<JobEntry>>,
}

struct PoolShared {
    max_threads: usize,
    queue: Mutex<QueueState>,
    wake_cv: Condvar,
}

static POOL_SHARED: OnceLock<Arc<PoolShared>> = OnceLock::new();

fn shared_pool() -> &'static Arc<PoolShared> {
    POOL_SHARED.get_or_init(|| {
        let total = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(32);
        let bg_workers = total.saturating_sub(1);
        let shared = Arc::new(PoolShared {
            max_threads: total,
            queue: Mutex::new(QueueState {
                jobs: VecDeque::new(),
            }),
            wake_cv: Condvar::new(),
        });
        for idx in 0..bg_workers {
            let shared_clone = Arc::clone(&shared);
            let builder = std::thread::Builder::new()
                .name(format!("cellan-formula-{idx}"))
                .stack_size(8 * 1024 * 1024);
            let _ = builder.spawn(move || worker_loop(shared_clone));
        }
        shared
    })
}

fn worker_loop(shared: Arc<PoolShared>) {
    IN_POOL_WORKER.with(|flag| flag.set(true));
    loop {
        let claimed: (Arc<JobEntry>, *const (), fn(*const (), usize), usize) = {
            let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                let mut found = None;
                for job_arc in &q.jobs {
                    let mut job = job_arc.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !job.closed && job.helpers_joined < job.max_helpers {
                        job.helpers_joined += 1;
                        let tid = job.helpers_joined; // caller runs tid 0; helpers run 1..=max_helpers
                        found = Some((Arc::clone(job_arc), job.func_ptr, job.trampoline, tid));
                        break;
                    }
                }
                if let Some(item) = found {
                    break item;
                }
                q = shared.wake_cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };

        let (job_arc, func_ptr, trampoline, tid) = claimed;
        #[cfg(panic = "unwind")]
        let res = catch_unwind(AssertUnwindSafe(|| {
            trampoline(func_ptr, tid);
        }));
        #[cfg(not(panic = "unwind"))]
        trampoline(func_ptr, tid);

        let mut job = job_arc.state.lock().unwrap_or_else(|e| e.into_inner());
        #[cfg(panic = "unwind")]
        if res.is_err() {
            job.panicked.store(true, Ordering::Release);
        }
        job.helpers_finished += 1;
        if job.closed && job.helpers_finished == job.helpers_joined {
            job_arc.done_cv.notify_all();
        }
    }
}

/// Maximum number of threads available in the persistent formula pool (capped at 32).
#[inline]
pub(crate) fn max_workers() -> usize {
    shared_pool().max_threads
}

/// Execute `f(worker_id)` across up to `requested_threads` threads (the calling thread
/// participates as `worker_id = 0`, and persistent pool threads assist as `1..requested_threads`).
///
/// Callers MUST use dynamic work distribution (e.g. atomic chunk index or shared work queue)
/// so that completion does not require a specific number of helper threads to join.
pub(crate) fn run_parallel<F>(requested_threads: usize, f: F)
where
    F: Fn(usize) + Sync,
{
    let shared = shared_pool();
    let threads = requested_threads.min(shared.max_threads);
    if threads <= 1 || IN_POOL_WORKER.with(Cell::get) {
        f(0);
        return;
    }

    fn trampoline<F: Fn(usize) + Sync>(ptr: *const (), tid: usize) {
        // Safety: `ptr` points to `f: F` on caller's stack which stays alive until all helpers finish.
        let func = unsafe { &*(ptr as *const F) };
        func(tid);
    }

    let job_arc = Arc::new(JobEntry {
        state: Mutex::new(JobState {
            func_ptr: (&f as *const F) as *const (),
            trampoline: trampoline::<F>,
            max_helpers: threads - 1,
            helpers_joined: 0,
            helpers_finished: 0,
            closed: false,
            #[cfg(panic = "unwind")]
            panicked: AtomicBool::new(false),
        }),
        done_cv: Condvar::new(),
    });

    {
        let mut q = shared.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.jobs.push_back(Arc::clone(&job_arc));
        if threads == 2 {
            shared.wake_cv.notify_one();
        } else {
            shared.wake_cv.notify_all();
        }
    }

    // Ensure that even if caller's `f(0)` panics, we close the job and wait for all
    // active helpers to finish BEFORE unwinding `f`'s stack frame!
    struct CloseGuard<'a> {
        shared: &'a PoolShared,
        job_arc: Arc<JobEntry>,
    }

    impl Drop for CloseGuard<'_> {
        fn drop(&mut self) {
            // Remove from global queue first so no new helpers can claim it.
            {
                let mut q = self.shared.queue.lock().unwrap_or_else(|e| e.into_inner());
                let mut job = self.job_arc.state.lock().unwrap_or_else(|e| e.into_inner());
                job.closed = true;
                q.jobs.retain(|j| !Arc::ptr_eq(j, &self.job_arc));
            }
            // Wait until every helper that already joined finishes executing `trampoline`.
            let mut job = self.job_arc.state.lock().unwrap_or_else(|e| e.into_inner());
            while job.helpers_finished < job.helpers_joined {
                job = self
                    .job_arc
                    .done_cv
                    .wait(job)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    let guard = CloseGuard {
        shared,
        job_arc: Arc::clone(&job_arc),
    };

    #[cfg(panic = "unwind")]
    {
        let caller_res = catch_unwind(AssertUnwindSafe(|| {
            f(0);
        }));

        drop(guard);

        if let Err(payload) = caller_res {
            resume_unwind(payload);
        }
        let helper_panicked = job_arc
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .panicked
            .load(Ordering::Acquire);
        if helper_panicked {
            panic!("formula pool worker panicked");
        }
    }
    #[cfg(not(panic = "unwind"))]
    {
        // Under `panic = "abort"`, any panic terminates the process immediately without
        // unwinding `f`'s stack frame; on normal return `drop(guard)` joins all active helpers.
        f(0);
        drop(guard);
    }
}
