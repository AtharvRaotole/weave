//! Cancellation tokens for host tools.
//!
//! A token becomes cancelled when its own source fires or when the session
//! that spawned it is cancelled. Async tools should `.await token.cancelled()`
//! or `select!` on it; dropping their future abandons the call. Cooperative
//! tools should poll [`CancelToken::is_cancelled`] and return.

use tokio::sync::watch;

/// Signals that a single tool attempt should stop.
#[derive(Clone, Debug)]
pub struct CancelToken {
    local: watch::Receiver<bool>,
    session: watch::Receiver<bool>,
}

/// Fires a [`CancelToken`] without cancelling the whole session.
#[derive(Clone, Debug)]
pub struct CancelSource {
    tx: watch::Sender<bool>,
}

impl CancelToken {
    pub fn pair(session: watch::Receiver<bool>) -> (CancelSource, Self) {
        let (tx, local) = watch::channel(false);
        (
            CancelSource { tx },
            Self { local, session },
        )
    }

    pub fn is_cancelled(&self) -> bool {
        *self.local.borrow() || *self.session.borrow()
    }

    /// Resolves when this attempt or the session is cancelled.
    pub async fn cancelled(&self) {
        let mut local = self.local.clone();
        let mut session = self.session.clone();
        loop {
            if *local.borrow() || *session.borrow() {
                return;
            }
            tokio::select! {
                result = local.changed() => {
                    if result.is_err() {
                        return;
                    }
                }
                result = session.changed() => {
                    if result.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

impl CancelSource {
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}
