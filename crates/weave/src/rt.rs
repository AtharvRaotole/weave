//! Runtime shims so the scheduler can run on native threads or, for wasm32
//! and the `virtual-time` feature, on one thread with a virtual clock.
//!
//! Native: a multi-threaded tokio runtime, real timers, blocking tools on
//! threads. Virtual: a current-thread tokio runtime that is only driven inside
//! [`drive`]. When no task made progress since the last check, the clock jumps
//! to the earliest pending [`sleep`] deadline. Tool latency, timeouts and the
//! wall-clock limit all use that clock, so runs are deterministic and a 300ms
//! mock call takes no real time.

use std::future::Future;
use std::time::Duration;

#[cfg(not(weave_vt))]
pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(64)
        .enable_all()
        .thread_name("weave")
        .build()
        .expect("create tokio runtime")
}

#[cfg(weave_vt)]
pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("create tokio runtime")
}

/// Wait for `d`, on the real or the virtual clock.
#[cfg(not(weave_vt))]
pub(crate) async fn sleep(d: Duration) {
    tokio::time::sleep(d).await
}

#[cfg(weave_vt)]
pub(crate) async fn sleep(d: Duration) {
    vclock::sleep(d).await
}

/// Time since an arbitrary fixed point, on the real or the virtual clock.
#[cfg(not(weave_vt))]
pub(crate) fn now() -> Duration {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

#[cfg(weave_vt)]
pub(crate) fn now() -> Duration {
    vclock::now()
}

/// Spawn a task. In virtual mode the task counts its polls so [`drive`] can
/// tell when everything is waiting on the clock.
pub(crate) fn spawn<F>(
    handle: &tokio::runtime::Handle,
    fut: F,
) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    #[cfg(weave_vt)]
    {
        handle.spawn(vclock::Counted(Box::pin(fut)))
    }
    #[cfg(not(weave_vt))]
    {
        handle.spawn(fut)
    }
}

/// Run blocking work. Virtual mode has no threads, so it runs inline.
pub(crate) fn spawn_blocking<F, R>(
    handle: &tokio::runtime::Handle,
    f: F,
) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    #[cfg(weave_vt)]
    {
        spawn(handle, async move { f() })
    }
    #[cfg(not(weave_vt))]
    {
        handle.spawn_blocking(f)
    }
}

/// Run a blocking closure and deliver its result on a oneshot channel. Native
/// uses a detached thread so a timeout can abandon it. Virtual runs it inline.
pub(crate) fn detach<F, R>(f: F) -> tokio::sync::oneshot::Receiver<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    #[cfg(weave_vt)]
    {
        let _ = tx.send(f());
    }
    #[cfg(not(weave_vt))]
    {
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
    rx
}

/// Drive a virtual-time runtime until `poll_done` returns a value.
///
/// Returns `None` when every task is blocked and no timer is pending, which
/// means the run can never finish.
#[cfg(weave_vt)]
pub(crate) fn drive<T>(
    runtime: &tokio::runtime::Runtime,
    mut poll_done: impl FnMut() -> Option<T>,
) -> Option<T> {
    runtime.block_on(async move {
        loop {
            let seen = vclock::polls();
            // Let every runnable task go first. If none ran, all of them are
            // waiting on the clock (or on nothing).
            tokio::task::yield_now().await;
            if let Some(value) = poll_done() {
                return Some(value);
            }
            if vclock::polls() == seen && !vclock::advance() {
                return None;
            }
        }
    })
}

#[cfg(weave_vt)]
pub mod vclock {
    //! Deterministic virtual clock used by wasm32 and `virtual-time` builds.
    //!
    //! State is per thread. A session resets it when it opens, so run one
    //! session per thread at a time.

    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    thread_local! {
        static NOW: Cell<Duration> = const { Cell::new(Duration::ZERO) };
        static POLLS: Cell<u64> = const { Cell::new(0) };
        static NEXT_ID: Cell<u64> = const { Cell::new(0) };
        static TIMERS: RefCell<Vec<(Duration, u64, Waker)>> = const { RefCell::new(Vec::new()) };
    }

    /// Current virtual time.
    pub fn now() -> Duration {
        NOW.with(Cell::get)
    }

    /// Reset the clock to zero and drop pending timers.
    pub fn reset() {
        NOW.with(|n| n.set(Duration::ZERO));
        TIMERS.with(|t| t.borrow_mut().clear());
    }

    pub(crate) fn polls() -> u64 {
        POLLS.with(Cell::get)
    }

    /// Jump to the earliest pending deadline and wake those timers.
    pub(crate) fn advance() -> bool {
        let next = TIMERS.with(|t| t.borrow().iter().map(|(d, _, _)| *d).min());
        let Some(next) = next else { return false };
        if next > now() {
            NOW.with(|n| n.set(next));
        }
        let due: Vec<Waker> = TIMERS.with(|t| {
            t.borrow()
                .iter()
                .filter(|(d, _, _)| *d <= next)
                .map(|(_, _, w)| w.clone())
                .collect()
        });
        for waker in due {
            waker.wake();
        }
        true
    }

    /// Sleep on the virtual clock.
    pub fn sleep(d: Duration) -> Sleep {
        Sleep {
            deadline: now().saturating_add(d),
            id: None,
        }
    }

    pub struct Sleep {
        deadline: Duration,
        id: Option<u64>,
    }

    impl Future for Sleep {
        type Output = ();
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if now() >= self.deadline {
                if let Some(id) = self.id.take() {
                    remove(id);
                }
                return Poll::Ready(());
            }
            let id = match self.id {
                Some(id) => id,
                None => {
                    let id = NEXT_ID.with(|n| {
                        let id = n.get();
                        n.set(id + 1);
                        id
                    });
                    self.id = Some(id);
                    id
                }
            };
            let deadline = self.deadline;
            TIMERS.with(|t| {
                let mut timers = t.borrow_mut();
                if let Some(slot) = timers.iter_mut().find(|(_, i, _)| *i == id) {
                    slot.2 = cx.waker().clone();
                } else {
                    timers.push((deadline, id, cx.waker().clone()));
                }
            });
            Poll::Pending
        }
    }

    impl Drop for Sleep {
        fn drop(&mut self) {
            if let Some(id) = self.id.take() {
                remove(id);
            }
        }
    }

    fn remove(id: u64) {
        TIMERS.with(|t| t.borrow_mut().retain(|(_, i, _)| *i != id));
    }

    /// A task wrapper that counts polls, so the driver can detect a stall.
    pub(crate) struct Counted<F>(pub(crate) Pin<Box<F>>);

    impl<F: Future> Future for Counted<F> {
        type Output = F::Output;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
            POLLS.with(|p| p.set(p.get() + 1));
            self.0.as_mut().poll(cx)
        }
    }
}
