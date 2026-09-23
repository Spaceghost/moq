//! The descriptor the host polls: readable while something is queued for it.
//!
//! One byte is written when the first thing is queued after a clear, never
//! more, so the socket cannot fill however long the host takes.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
struct Wake {
	rx: std::os::unix::net::UnixStream,
	tx: std::os::unix::net::UnixStream,
	pending: AtomicBool,
}

#[cfg(unix)]
static WAKE: LazyLock<Option<Wake>> = LazyLock::new(|| {
	let (rx, tx) = std::os::unix::net::UnixStream::pair().ok()?;
	rx.set_nonblocking(true).ok()?;
	tx.set_nonblocking(true).ok()?;
	Some(Wake {
		rx,
		tx,
		pending: AtomicBool::new(false),
	})
});

#[cfg(unix)]
pub(crate) fn fd() -> i32 {
	use std::os::fd::AsRawFd;
	WAKE.as_ref().map(|w| w.rx.as_raw_fd()).unwrap_or(-1)
}

#[cfg(unix)]
pub(crate) fn signal() {
	use std::io::Write;
	if let Some(w) = WAKE.as_ref()
		&& !w.pending.swap(true, Ordering::AcqRel)
	{
		let _ = (&w.tx).write(&[1]);
	}
}

#[cfg(unix)]
pub(crate) fn clear() {
	use std::io::Read;
	if let Some(w) = WAKE.as_ref() {
		w.pending.store(false, Ordering::Release);
		let mut buf = [0u8; 64];
		while let Ok(n) = (&w.rx).read(&mut buf) {
			if n == 0 {
				break;
			}
		}
	}
}

#[cfg(not(unix))]
static PENDING: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(false));

#[cfg(not(unix))]
pub(crate) fn fd() -> i32 {
	-1
}

#[cfg(not(unix))]
pub(crate) fn signal() {
	PENDING.store(true, Ordering::Release);
}

#[cfg(not(unix))]
pub(crate) fn clear() {
	PENDING.store(false, Ordering::Release);
}
