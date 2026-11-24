//! Termination signals sent from outside (`kill`, `timeout`, closing the terminal or the
//! tmux pane). Instead of dying with the terminal still in raw mode (and, in monitor, its
//! command still running), the program cleans up first. The same file in track, monitor and
//! git-graph.
//!
//! A thread waits for the signals. On SIGTERM, SIGINT or SIGQUIT the main loop sees
//! [`Signals::received`] and quits through its normal cleanup; if it hasn't within a moment
//! (stuck somewhere), the thread cleans up and exits itself. On SIGHUP the terminal is
//! already gone, and reading it can leave the main loop spinning, so the thread does it at
//! once.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

/// How long the main loop has to quit by itself after SIGTERM, SIGINT or SIGQUIT.
const GRACE: Duration = Duration::from_secs(2);

pub struct Signals {
    caught: Arc<AtomicUsize>,
}

impl Signals {
    /// Starts catching the signals; `cleanup` (stopping child processes, say) runs when the
    /// thread has to end the program itself. Call it when the UI starts: before that, Ctrl-C
    /// should still end the program the usual way.
    pub fn catch(cleanup: impl Fn() + Send + 'static) -> io::Result<Self> {
        let caught = Arc::new(AtomicUsize::new(0));
        let flag = Arc::clone(&caught);
        let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT, SIGQUIT])?;
        thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                if let Some(sig) = signals.forever().next() {
                    flag.store(sig as usize, Ordering::SeqCst);
                    if sig != SIGHUP {
                        thread::sleep(GRACE);
                    }
                    crate::terminal::restore();
                    cleanup();
                    std::process::exit(exit_status(sig));
                }
            })?;
        Ok(Self { caught })
    }

    /// The signal that asked the program to quit, if one has arrived.
    pub fn received(&self) -> Option<i32> {
        match self.caught.load(Ordering::SeqCst) {
            0 => None,
            sig => Some(sig as i32),
        }
    }
}

/// The exit status after quitting because of `sig`: 128 + its number, as shells report it.
pub fn exit_status(sig: i32) -> i32 {
    128 + sig
}
