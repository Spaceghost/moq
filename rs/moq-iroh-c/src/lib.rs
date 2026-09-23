//! Media over QUIC between iroh endpoints, behind a small C ABI for a host that
//! runs its own `poll()` loop.
//!
//! A *node* is one iroh endpoint and one moq origin. It accepts moq-lite sessions
//! from any iroh peer and serves the broadcasts published on it; it can also dial
//! a peer by ticket and subscribe to one of its tracks. Every call returns at
//! once. The library runs its own small tokio runtime; what it has for the host
//! (a subscribed object, a subscription ending) is queued, and the socket
//! [`moqi_wake_fd`] returns becomes readable, so the host can sleep in `poll()`
//! with that descriptor among its own and drain the queues with [`moqi_read`].
//! Nothing ever calls back into the host, so no host code runs on a library
//! thread.
//!
//! What the host can show comes out as JSON from [`moqi_stats`]: the endpoint
//! id, its home relay, every connection with its network paths (relay or direct,
//! which one is selected, the round-trip time of each), a timeline of path
//! events (a relay path first, then direct paths as holepunching succeeds, and
//! the switch between them), the moq traffic per peer, and each subscription's
//! position, latency and skipped groups.
//!
//! Handles are positive `int32_t`s in one number space; errors are negative
//! (`MOQI_ERR_*`), with the reason from [`moqi_last_error`] on the same thread.

#![allow(non_camel_case_types)]
#![allow(clippy::missing_safety_doc)]

mod node;
mod publish;
mod subscribe;
mod ticket;
mod wake;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::{Arc, LazyLock, Mutex};

pub use node::Node;
pub use publish::Publisher;
pub use subscribe::Subscription;

/// A bad argument: a null or non-UTF-8 string, an unknown relay mode, a bad ticket.
pub const MOQI_ERR_ARG: i32 = -1;
/// No such handle (or a handle of another kind).
pub const MOQI_ERR_HANDLE: i32 = -2;
/// The buffer is too small; `moqi_object.len` (or the return of the stats call) says how big it must be.
pub const MOQI_ERR_SMALL: i32 = -3;
/// The endpoint could not be bound, or a file could not be read or written.
pub const MOQI_ERR_IO: i32 = -4;
/// The subscription has ended and everything it received has been read.
pub const MOQI_ERR_ENDED: i32 = -5;
/// moq refused (a broadcast or track that cannot be created).
pub const MOQI_ERR_MOQ: i32 = -6;
/// The library panicked inside the call; the reason is in `moqi_last_error`.
pub const MOQI_ERR_PANIC: i32 = -7;

/// `moqi_object.flags`: the first object of its group.
pub const MOQI_OBJ_GROUP_START: u32 = 1;
/// `moqi_object.flags`: groups were skipped (or one was cut short) before this object.
pub const MOQI_OBJ_GAP: u32 = 2;

/// How a node is set up. Strings are UTF-8 and NUL-terminated; they are copied.
#[repr(C)]
pub struct moqi_node_config {
	/// The endpoint's secret key file (64 hex digits). Created, owner-only,
	/// when missing, so the endpoint id stays the same across runs. NULL: a new
	/// key (and id) every time.
	pub secret_path: *const c_char,
	/// `"default"` (n0's public relays and address lookup), `"off"` (direct
	/// paths only), or a relay URL (only that relay, no address lookup: peers
	/// find each other through tickets). NULL means `"default"`.
	pub relay: *const c_char,
	/// UDP port to bind on every IPv4 interface; 0 lets the system pick.
	pub port: u16,
}

/// One object of a subscribed track, as [`moqi_read`] hands it over.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct moqi_object {
	/// The moq group sequence number.
	pub group: u64,
	/// The object's index within its group (0 for the first).
	pub index: u64,
	/// The publisher's wall clock when it published the object, in microseconds since the Unix epoch.
	pub sent_us: u64,
	/// This host's wall clock when moq handed the object over, in microseconds since the Unix epoch.
	pub recv_us: u64,
	/// Payload bytes.
	pub len: u64,
	/// `MOQI_OBJ_*` bits.
	pub flags: u32,
}

pub(crate) enum Handle {
	Node(Arc<Node>),
	Publisher(Arc<Publisher>),
	Subscription(Arc<Subscription>),
}

/// A node's publishers and subscriptions with their handles, in handle order.
pub(crate) type Publishers = Vec<(i32, Arc<Publisher>)>;
pub(crate) type Subscriptions = Vec<(i32, Arc<Subscription>)>;

#[derive(Default)]
pub(crate) struct State {
	next: i32,
	handles: HashMap<i32, Handle>,
}

pub(crate) static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

/// The library's runtime: two workers, started on first use and kept for the process.
pub(crate) static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
	tokio::runtime::Builder::new_multi_thread()
		.worker_threads(2)
		.thread_name("moq-iroh")
		.enable_all()
		.build()
		.expect("tokio runtime")
});

impl State {
	pub(crate) fn lock() -> std::sync::MutexGuard<'static, State> {
		STATE.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn insert(&mut self, h: Handle) -> i32 {
		loop {
			self.next = if self.next >= i32::MAX - 1 { 1 } else { self.next + 1 };
			if !self.handles.contains_key(&self.next) {
				self.handles.insert(self.next, h);
				return self.next;
			}
		}
	}

	fn node(&self, id: i32) -> Result<Arc<Node>, Error> {
		match self.handles.get(&id) {
			Some(Handle::Node(n)) => Ok(n.clone()),
			_ => Err(Error::Handle),
		}
	}

	fn publisher(&self, id: i32) -> Result<Arc<Publisher>, Error> {
		match self.handles.get(&id) {
			Some(Handle::Publisher(p)) => Ok(p.clone()),
			_ => Err(Error::Handle),
		}
	}

	fn subscription(&self, id: i32) -> Result<Arc<Subscription>, Error> {
		match self.handles.get(&id) {
			Some(Handle::Subscription(s)) => Ok(s.clone()),
			_ => Err(Error::Handle),
		}
	}

	/// Publishers and subscriptions of `node`, for its stats.
	pub(crate) fn children(&self, node: &Arc<Node>) -> (Publishers, Subscriptions) {
		let mut pubs = Vec::new();
		let mut subs = Vec::new();
		for (id, h) in &self.handles {
			match h {
				Handle::Publisher(p) if Arc::ptr_eq(&p.node, node) => pubs.push((*id, p.clone())),
				Handle::Subscription(s) if Arc::ptr_eq(&s.node, node) => subs.push((*id, s.clone())),
				_ => {}
			}
		}
		pubs.sort_by_key(|(id, _)| *id);
		subs.sort_by_key(|(id, _)| *id);
		(pubs, subs)
	}
}

#[derive(Debug)]
pub(crate) enum Error {
	Arg(String),
	Handle,
	Small(usize),
	Io(String),
	Ended(String),
	Moq(String),
}

impl Error {
	fn code(&self) -> i32 {
		match self {
			Error::Arg(_) => MOQI_ERR_ARG,
			Error::Handle => MOQI_ERR_HANDLE,
			Error::Small(_) => MOQI_ERR_SMALL,
			Error::Io(_) => MOQI_ERR_IO,
			Error::Ended(_) => MOQI_ERR_ENDED,
			Error::Moq(_) => MOQI_ERR_MOQ,
		}
	}

	fn message(&self) -> String {
		match self {
			Error::Arg(s) => format!("bad argument: {s}"),
			Error::Handle => "no such handle".into(),
			Error::Small(n) => format!("buffer too small: {n} bytes needed"),
			Error::Io(s) => s.clone(),
			Error::Ended(why) => format!("ended: {why}"),
			Error::Moq(s) => format!("moq: {s}"),
		}
	}
}

thread_local! {
	static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_error(msg: String) {
	LAST_ERROR.with(|e| *e.borrow_mut() = msg);
}

/// Run `f`, turning its error or a panic into a negative code and the reason.
fn guard(f: impl FnOnce() -> Result<i32, Error>) -> i32 {
	match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
		Ok(Ok(v)) => v,
		Ok(Err(e)) => {
			set_error(e.message());
			e.code()
		}
		Err(p) => {
			let why = p
				.downcast_ref::<&str>()
				.map(|s| s.to_string())
				.or_else(|| p.downcast_ref::<String>().cloned())
				.unwrap_or_else(|| "panic".into());
			set_error(format!("panic: {why}"));
			MOQI_ERR_PANIC
		}
	}
}

unsafe fn opt_str<'a>(p: *const c_char) -> Result<Option<&'a str>, Error> {
	if p.is_null() {
		return Ok(None);
	}
	// SAFETY: the caller passes a NUL-terminated string that outlives the call.
	let s = unsafe { CStr::from_ptr(p) };
	s.to_str().map(Some).map_err(|_| Error::Arg("not UTF-8".into()))
}

unsafe fn req_str<'a>(p: *const c_char, what: &str) -> Result<&'a str, Error> {
	match unsafe { opt_str(p) }? {
		Some(s) if !s.is_empty() => Ok(s),
		_ => Err(Error::Arg(format!("{what} is empty"))),
	}
}

/// Copy `text` and a NUL into `buf`; its length, or `Small` with the size needed.
unsafe fn write_text(text: &str, buf: *mut c_char, cap: usize) -> Result<i32, Error> {
	let need = text.len() + 1;
	if buf.is_null() || cap < need {
		return Err(Error::Small(need));
	}
	// SAFETY: `buf` holds at least `cap >= need` bytes, per the caller.
	unsafe {
		std::ptr::copy_nonoverlapping(text.as_ptr(), buf as *mut u8, text.len());
		*buf.add(text.len()) = 0;
	}
	i32::try_from(text.len()).map_err(|_| Error::Small(need))
}

/// Microseconds since the Unix epoch on this host's wall clock.
pub(crate) fn now_us() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_micros() as u64)
		.unwrap_or(0)
}

// Nodes ---------------------------------------------------------------------------------

/// Bind an iroh endpoint and start serving its origin. Blocks while the socket
/// is bound (not while relays are reached). A node handle, or a negative error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_node_new(config: *const moqi_node_config) -> i32 {
	guard(|| {
		if config.is_null() {
			return Err(Error::Arg("config is NULL".into()));
		}
		// SAFETY: non-null, and the caller's record for the duration of the call.
		let config = unsafe { &*config };
		let secret = unsafe { opt_str(config.secret_path) }?.map(str::to_owned);
		let relay = unsafe { opt_str(config.relay) }?.unwrap_or("default").to_owned();
		let port = config.port;
		let node = RUNTIME.block_on(Node::bind(secret, relay, port))?;
		Ok(State::lock().insert(Handle::Node(node)))
	})
}

/// Close a node: its connections, publishers and subscriptions end.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_node_close(node: i32) -> i32 {
	guard(|| {
		let (n, children) = {
			let mut st = State::lock();
			let n = st.node(node)?;
			let (pubs, subs) = st.children(&n);
			let mut ids: Vec<i32> = pubs.iter().map(|(id, _)| *id).collect();
			ids.extend(subs.iter().map(|(id, _)| *id));
			for id in &ids {
				st.handles.remove(id);
			}
			st.handles.remove(&node);
			(n, subs)
		};
		for (_, s) in children {
			s.close();
		}
		RUNTIME.block_on(n.close());
		Ok(0)
	})
}

/// The node's ticket, `iroh://<endpoint id>?relay=<url>&addr=<ip:port>...`:
/// everything a peer needs to dial it. Its length, or `MOQI_ERR_SMALL` when
/// `cap` cannot hold it and its NUL (`moqi_last_error` says how much).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_node_ticket(node: i32, buf: *mut c_char, cap: usize) -> i32 {
	guard(|| {
		let n = State::lock().node(node)?;
		let t = ticket::format(&n.endpoint.addr());
		unsafe { write_text(&t, buf, cap) }
	})
}

/// A JSON snapshot of the node for display (see the crate README for its
/// fields). Its length; `MOQI_ERR_SMALL` when `cap` is too small.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_stats(node: i32, buf: *mut c_char, cap: usize) -> i32 {
	guard(|| {
		let n = State::lock().node(node)?;
		let (pubs, subs) = State::lock().children(&n);
		let json = n.stats_json(&pubs, &subs).to_string();
		unsafe { write_text(&json, buf, cap) }
	})
}

// Publishing ----------------------------------------------------------------------------

/// Publish broadcast `broadcast` on `node` with one track, `track`. Peers that
/// connect to the node can subscribe to it. A publisher handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_publish(node: i32, broadcast: *const c_char, track: *const c_char) -> i32 {
	guard(|| {
		let n = State::lock().node(node)?;
		let broadcast = unsafe { req_str(broadcast, "broadcast") }?.to_owned();
		let track = unsafe { req_str(track, "track") }?.to_owned();
		let _rt = RUNTIME.enter();
		let p = Publisher::new(n, &broadcast, &track)?;
		Ok(State::lock().insert(Handle::Publisher(Arc::new(p))))
	})
}

/// Publish one object. `new_group` finishes the group in progress and starts
/// the next one with this object; a subscriber that joins or falls behind
/// starts at a group, so put a key frame there. The group's sequence number.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_publish_object(publisher: i32, data: *const u8, len: usize, new_group: bool) -> i32 {
	guard(|| {
		let p = State::lock().publisher(publisher)?;
		if data.is_null() && len > 0 {
			return Err(Error::Arg("data is NULL".into()));
		}
		let bytes = if len == 0 {
			&[][..]
		} else {
			// SAFETY: `data` holds `len` bytes, per the caller.
			unsafe { std::slice::from_raw_parts(data, len) }
		};
		let _rt = RUNTIME.enter();
		let seq = p.object(bytes, new_group)?;
		Ok(i32::try_from(seq % (i32::MAX as u64)).unwrap_or(0))
	})
}

/// Stop publishing: the track and broadcast finish.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_publish_close(publisher: i32) -> i32 {
	guard(|| {
		let p = {
			let mut st = State::lock();
			let p = st.publisher(publisher)?;
			st.handles.remove(&publisher);
			p
		};
		let _rt = RUNTIME.enter();
		p.finish();
		Ok(0)
	})
}

// Subscribing ---------------------------------------------------------------------------

/// Dial the peer a ticket names (or a bare endpoint id) from `node` and
/// subscribe to `track` of its broadcast `broadcast`. Groups older than
/// `max_age_ms` behind the newest are skipped, not queued. Returns at once; the
/// connection is made in the background (`moqi_stats` shows how it goes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_subscribe(
	node: i32,
	ticket: *const c_char,
	broadcast: *const c_char,
	track: *const c_char,
	max_age_ms: u32,
) -> i32 {
	guard(|| {
		let n = State::lock().node(node)?;
		let addr = ticket::parse(unsafe { req_str(ticket, "ticket") }?)?;
		let broadcast = unsafe { req_str(broadcast, "broadcast") }?.to_owned();
		let track = unsafe { req_str(track, "track") }?.to_owned();
		let mut st = State::lock();
		let id = st.insert(Handle::Node(n.clone())); // reserve the number
		let s = Subscription::start(id, n, addr, broadcast, track, max_age_ms);
		st.handles.insert(id, Handle::Subscription(s));
		Ok(id)
	})
}

/// Make a subscription read slowly on purpose: it waits `delay_ms` before it
/// takes each object from moq, so it falls behind the live edge and moq skips
/// groups for it. 0 reads at full speed again.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_subscribe_throttle(subscription: i32, delay_ms: u32) -> i32 {
	guard(|| {
		let s = State::lock().subscription(subscription)?;
		s.set_throttle(delay_ms);
		Ok(0)
	})
}

/// Take the next object of a subscription: 1 and the object (its payload in
/// `buf`), 0 when none is waiting, `MOQI_ERR_SMALL` when `cap` is below
/// `out->len` (the object stays queued), `MOQI_ERR_ENDED` once the
/// subscription has ended and its queue is empty (`moqi_last_error` says why).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_read(subscription: i32, out: *mut moqi_object, buf: *mut u8, cap: usize) -> i32 {
	guard(|| {
		if out.is_null() {
			return Err(Error::Arg("out is NULL".into()));
		}
		let s = State::lock().subscription(subscription)?;
		// SAFETY: `out` is the caller's record; `buf` holds `cap` bytes.
		let out = unsafe { &mut *out };
		s.read(out, buf, cap)
	})
}

/// End a subscription and forget its handle.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_subscribe_close(subscription: i32) -> i32 {
	guard(|| {
		let s = {
			let mut st = State::lock();
			let s = st.subscription(subscription)?;
			st.handles.remove(&subscription);
			s
		};
		s.close();
		Ok(0)
	})
}

// Waking the host -----------------------------------------------------------------------

/// A descriptor that becomes readable when an object or an ending is queued
/// for the host (POSIX); -1 where there is none (Windows: poll on a timer).
/// Reading is [`moqi_wake_clear`]'s job; the host only polls it.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_wake_fd() -> i32 {
	wake::fd()
}

/// Empty the wake descriptor. Call before draining the queues, so a wake that
/// comes during the drain is not lost.
#[unsafe(no_mangle)]
pub extern "C" fn moqi_wake_clear() {
	wake::clear();
}

/// The reason for this thread's last negative return, NUL-terminated and cut
/// to fit `cap`. Its full length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moqi_last_error(buf: *mut c_char, cap: usize) -> i32 {
	LAST_ERROR.with(|e| {
		let e = e.borrow();
		if !buf.is_null() && cap > 0 {
			let n = e.len().min(cap - 1);
			// SAFETY: `buf` holds `cap` bytes, per the caller.
			unsafe {
				std::ptr::copy_nonoverlapping(e.as_ptr(), buf as *mut u8, n);
				*buf.add(n) = 0;
			}
		}
		e.len() as i32
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::ffi::CString;
	use std::time::{Duration, Instant};

	fn node(relay: &str) -> i32 {
		let relay = CString::new(relay).unwrap();
		let cfg = moqi_node_config {
			secret_path: std::ptr::null(),
			relay: relay.as_ptr(),
			port: 0,
		};
		let n = unsafe { moqi_node_new(&cfg) };
		assert!(n > 0, "node: {}", last_error());
		n
	}

	fn last_error() -> String {
		let mut buf = vec![0 as c_char; 512];
		unsafe { moqi_last_error(buf.as_mut_ptr(), buf.len()) };
		unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
	}

	fn ticket(n: i32) -> CString {
		let mut buf = vec![0 as c_char; 2048];
		let len = unsafe { moqi_node_ticket(n, buf.as_mut_ptr(), buf.len()) };
		assert!(len > 0, "ticket: {}", last_error());
		CString::new(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_bytes()).unwrap()
	}

	fn stats(n: i32) -> serde_json::Value {
		let need = unsafe { moqi_stats(n, std::ptr::null_mut(), 0) };
		assert_eq!(need, MOQI_ERR_SMALL);
		let mut buf = vec![0 as c_char; 1 << 20];
		let len = unsafe { moqi_stats(n, buf.as_mut_ptr(), buf.len()) };
		assert!(len > 0, "stats: {}", last_error());
		serde_json::from_str(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap()).unwrap()
	}

	fn subscribe(n: i32, t: &CString, max_age_ms: u32) -> i32 {
		let b = CString::new("netlab").unwrap();
		let tr = CString::new("frames").unwrap();
		let s = unsafe { moqi_subscribe(n, t.as_ptr(), b.as_ptr(), tr.as_ptr(), max_age_ms) };
		assert!(s > 0, "subscribe: {}", last_error());
		s
	}

	fn publish(n: i32) -> i32 {
		let b = CString::new("netlab").unwrap();
		let tr = CString::new("frames").unwrap();
		let p = unsafe { moqi_publish(n, b.as_ptr(), tr.as_ptr()) };
		assert!(p > 0, "publish: {}", last_error());
		p
	}

	fn object(p: i32, text: &str, new_group: bool) -> i32 {
		unsafe { moqi_publish_object(p, text.as_ptr(), text.len(), new_group) }
	}

	/// Read what is queued: (object, payload) pairs.
	fn drain(s: i32) -> Vec<(moqi_object, String)> {
		let mut got = Vec::new();
		let mut buf = vec![0u8; 4096];
		loop {
			let mut o = moqi_object::default();
			let r = unsafe { moqi_read(s, &mut o, buf.as_mut_ptr(), buf.len()) };
			if r != 1 {
				return got;
			}
			got.push((o, String::from_utf8_lossy(&buf[..o.len as usize]).into_owned()));
		}
	}

	fn wait_live(sub_node: i32, s: i32) {
		let deadline = Instant::now() + Duration::from_secs(20);
		loop {
			let st = stats(sub_node);
			let me = st["subs"]
				.as_array()
				.unwrap()
				.iter()
				.find(|x| x["h"] == s)
				.cloned()
				.unwrap();
			if me["state"] == "live" {
				return;
			}
			assert_ne!(me["state"], "ended", "subscription ended: {me}");
			assert!(Instant::now() < deadline, "never live: {me}");
			std::thread::sleep(Duration::from_millis(20));
		}
	}

	/// Two nodes on this host, direct paths only: objects arrive in order, each
	/// group's first flagged, and both sides' stats show the connection, its
	/// selected path with a round-trip time, and moq's traffic.
	#[test]
	fn publish_and_subscribe_direct() {
		let a = node("off");
		let b = node("off");
		let p = publish(a);
		let t = ticket(a);
		assert!(t.to_str().unwrap().contains("addr="), "{t:?}");
		let s = subscribe(b, &t, 2000);
		wait_live(b, s);

		let mut got = Vec::new();
		let deadline = Instant::now() + Duration::from_secs(20);
		let mut sent = 0;
		while got.len() < 12 {
			assert!(Instant::now() < deadline, "only {} objects arrived", got.len());
			if sent < 12 {
				assert!(object(p, &format!("o{sent}"), sent % 4 == 0) >= 0, "{}", last_error());
				sent += 1;
			}
			std::thread::sleep(Duration::from_millis(15));
			got.extend(drain(s));
		}
		let texts: Vec<&str> = got.iter().map(|(_, t)| t.as_str()).collect();
		assert_eq!(texts, (0..12).map(|i| format!("o{i}")).collect::<Vec<_>>());
		for (i, (o, _)) in got.iter().enumerate() {
			assert_eq!(o.index, (i % 4) as u64);
			assert_eq!(o.flags & MOQI_OBJ_GROUP_START != 0, i % 4 == 0, "object {i}");
			assert_eq!(o.flags & MOQI_OBJ_GAP, 0);
			assert!(o.recv_us >= o.sent_us && o.recv_us - o.sent_us < 5_000_000);
		}
		assert!(got[4].0.group > got[0].0.group);

		let sb = stats(b);
		let sub = &sb["subs"][0];
		assert_eq!(sub["objects"], 12);
		assert_eq!(sub["skipped"], 0);
		let conn = &sb["conns"][0];
		assert_eq!(conn["dir"], "out");
		assert_eq!(conn["open"], true);
		let paths = conn["paths"].as_array().unwrap();
		assert!(
			paths.iter().any(|p| p["selected"] == true && p["kind"] == "direct"),
			"{paths:?}"
		);
		assert!(paths.iter().all(|p| p["rtt_us"].as_u64().is_some()));
		assert!(conn["moq"]["frames"].as_u64().unwrap() >= 12, "{conn}");

		let sa = stats(a);
		assert_eq!(sa["pubs"][0]["objects"], 12);
		assert_eq!(sa["pubs"][0]["groups"], 3);
		let inc = sa["conns"]
			.as_array()
			.unwrap()
			.iter()
			.find(|c| c["dir"] == "in")
			.cloned()
			.unwrap();
		assert_eq!(inc["peer"], sb["id"]);
		assert!(inc["moq"]["frames"].as_u64().unwrap() >= 12, "{inc}");
		assert!(sa["events"].as_array().unwrap().iter().any(|e| e["what"] == "selected"));

		assert_eq!(moqi_subscribe_close(s), 0);
		assert_eq!(moqi_publish_close(p), 0);
		assert_eq!(moqi_node_close(b), 0);
		assert_eq!(moqi_node_close(a), 0);
		assert_eq!(moqi_node_close(a), MOQI_ERR_HANDLE);
	}

	/// A subscriber that reads slower than the publisher writes falls behind,
	/// and moq skips (or cuts short) groups for it instead of queueing them;
	/// the object after the hole says so. A full-speed subscriber to the same
	/// broadcast misses nothing.
	#[test]
	fn slow_subscriber_skips_groups() {
		let a = node("off");
		let b = node("off");
		let p = publish(a);
		let t = ticket(a);
		let fast = subscribe(b, &t, 500);
		let slow = subscribe(b, &t, 100);
		assert_eq!(moqi_subscribe_throttle(slow, 40), 0);
		wait_live(b, fast);
		wait_live(b, slow);

		let mut fast_got = Vec::new();
		let mut slow_got = Vec::new();
		// 25 groups of 5 objects, 10 ms apart: 50 ms a group, 200 ms a group for the slow reader
		for g in 0..25 {
			for i in 0..5 {
				assert!(object(p, &format!("g{g}o{i}"), i == 0) >= 0, "{}", last_error());
				std::thread::sleep(Duration::from_millis(10));
				fast_got.extend(drain(fast));
				slow_got.extend(drain(slow));
			}
		}
		let deadline = Instant::now() + Duration::from_secs(10);
		while fast_got.len() < 125 && Instant::now() < deadline {
			std::thread::sleep(Duration::from_millis(20));
			fast_got.extend(drain(fast));
			slow_got.extend(drain(slow));
		}
		assert_eq!(fast_got.len(), 125);
		assert!(fast_got.iter().all(|(o, _)| o.flags & MOQI_OBJ_GAP == 0));

		let st = stats(b);
		let s = st["subs"]
			.as_array()
			.unwrap()
			.iter()
			.find(|x| x["h"] == slow)
			.cloned()
			.unwrap();
		let skipped = s["skipped"].as_u64().unwrap() + s["cut"].as_u64().unwrap();
		assert!(skipped > 0, "the slow subscriber skipped nothing: {s}");
		assert!(slow_got.len() < 125, "the slow subscriber got everything");
		assert!(
			slow_got.iter().any(|(o, _)| o.flags & MOQI_OBJ_GAP != 0),
			"no gap flagged"
		);
		assert_eq!(s["throttle_ms"], 40);

		moqi_node_close(b);
		moqi_node_close(a);
	}

	/// Dialed with only the relay in the ticket, a connection starts on the
	/// relay path; iroh then finds a direct one and moves the traffic to it.
	/// The timeline and the path list show both, in that order.
	#[test]
	fn relay_first_then_direct() {
		let (_map, url, _server) = RUNTIME.block_on(iroh::test_utils::run_relay_server()).unwrap();
		let a = node(url.as_str());
		let b = node(url.as_str());
		let p = publish(a);
		// wait for a's home relay, then hand b a ticket without direct addresses
		let deadline = Instant::now() + Duration::from_secs(20);
		while stats(a)["relays"]
			.as_array()
			.unwrap()
			.iter()
			.all(|r| r["connected"] != true)
		{
			assert!(Instant::now() < deadline, "never reached the relay: {}", stats(a));
			std::thread::sleep(Duration::from_millis(20));
		}
		let id = stats(a)["id"].as_str().unwrap().to_owned();
		let t = CString::new(format!("iroh://{id}?relay={}", url.as_str())).unwrap();
		let s = subscribe(b, &t, 2000);
		wait_live(b, s);

		let deadline = Instant::now() + Duration::from_secs(20);
		let mut n = 0;
		loop {
			object(p, &format!("o{n}"), n % 5 == 0);
			n += 1;
			drain(s);
			let st = stats(b);
			let paths = st["conns"][0]["paths"].as_array().cloned().unwrap_or_default();
			if paths.iter().any(|p| p["kind"] == "direct" && p["selected"] == true) {
				let ev: Vec<(String, String)> = st["events"]
					.as_array()
					.unwrap()
					.iter()
					.map(|e| {
						(
							e["what"].as_str().unwrap().to_owned(),
							e["kind"].as_str().unwrap().to_owned(),
						)
					})
					.collect();
				let relay = ev.iter().position(|e| e.1 == "relay").expect("a relay path event");
				let direct = ev
					.iter()
					.position(|e| e.0 == "selected" && e.1 == "direct")
					.expect("the direct path selected");
				assert!(relay < direct, "{ev:?}");
				break;
			}
			assert!(Instant::now() < deadline, "never went direct: {st}");
			std::thread::sleep(Duration::from_millis(20));
		}
		moqi_node_close(b);
		moqi_node_close(a);
	}

	#[test]
	fn bad_arguments() {
		let a = node("off");
		let bad = CString::new("iroh://nope").unwrap();
		let x = CString::new("x").unwrap();
		assert_eq!(
			unsafe { moqi_subscribe(a, bad.as_ptr(), x.as_ptr(), x.as_ptr(), 0) },
			MOQI_ERR_ARG
		);
		assert!(last_error().contains("endpoint id"), "{}", last_error());
		assert_eq!(
			unsafe { moqi_subscribe(a, std::ptr::null(), x.as_ptr(), x.as_ptr(), 0) },
			MOQI_ERR_ARG
		);
		assert_eq!(unsafe { moqi_publish(9999, x.as_ptr(), x.as_ptr()) }, MOQI_ERR_HANDLE);
		assert_eq!(moqi_publish_close(a), MOQI_ERR_HANDLE, "a node is not a publisher");
		let relay = CString::new("not a url").unwrap();
		let cfg = moqi_node_config {
			secret_path: std::ptr::null(),
			relay: relay.as_ptr(),
			port: 0,
		};
		assert_eq!(unsafe { moqi_node_new(&cfg) }, MOQI_ERR_ARG);
		let mut small = [0 as c_char; 4];
		assert_eq!(
			unsafe { moqi_node_ticket(a, small.as_mut_ptr(), small.len()) },
			MOQI_ERR_SMALL
		);
		moqi_node_close(a);
	}

	/// A reader with too small a buffer is told the size and keeps the object.
	#[test]
	fn small_buffer_keeps_the_object() {
		let a = node("off");
		let b = node("off");
		let p = publish(a);
		let s = subscribe(b, &ticket(a), 1000);
		wait_live(b, s);
		let body = "x".repeat(100);
		object(p, &body, true);
		let mut o = moqi_object::default();
		let mut tiny = [0u8; 10];
		let deadline = Instant::now() + Duration::from_secs(10);
		let r = loop {
			let r = unsafe { moqi_read(s, &mut o, tiny.as_mut_ptr(), tiny.len()) };
			if r != 0 || Instant::now() > deadline {
				break r;
			}
			std::thread::sleep(Duration::from_millis(10));
		};
		assert_eq!(r, MOQI_ERR_SMALL);
		assert_eq!(o.len, 100);
		assert_eq!(drain(s).len(), 1);
		moqi_node_close(b);
		moqi_node_close(a);
	}

	/// The secret file keeps the endpoint id across nodes.
	#[test]
	fn secret_file_keeps_the_id() {
		let dir = tempfile::tempdir().unwrap();
		let path = CString::new(dir.path().join("key").to_str().unwrap()).unwrap();
		let relay = CString::new("off").unwrap();
		let cfg = moqi_node_config {
			secret_path: path.as_ptr(),
			relay: relay.as_ptr(),
			port: 0,
		};
		let a = unsafe { moqi_node_new(&cfg) };
		let id = stats(a)["id"].clone();
		moqi_node_close(a);
		let b = unsafe { moqi_node_new(&cfg) };
		assert_eq!(stats(b)["id"], id);
		moqi_node_close(b);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let mode = std::fs::metadata(dir.path().join("key")).unwrap().permissions().mode();
			assert_eq!(mode & 0o077, 0, "the key file is readable by others");
		}
	}

	/// The committed header is what cbindgen makes of the source.
	#[test]
	fn header_is_current() {
		let generated = include_str!(concat!(env!("OUT_DIR"), "/moq_iroh.h"));
		let committed = include_str!("../include/moq_iroh.h");
		assert!(
			generated == committed,
			"include/moq_iroh.h is stale: cp $OUT_DIR/moq_iroh.h rs/moq-iroh-c/include/"
		);
	}

	#[cfg(unix)]
	#[test]
	fn wake_fd_is_readable_after_an_object() {
		let fd = moqi_wake_fd();
		assert!(fd >= 0);
		moqi_wake_clear();
		wake::signal();
		wake::signal(); // one byte however many signals
		let mut pfd = [0u8; 1];
		use std::io::Read;
		use std::os::fd::FromRawFd;
		let f = std::mem::ManuallyDrop::new(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) });
		assert_eq!((&*f).read(&mut pfd).unwrap(), 1);
		assert!((&*f).read(&mut pfd).is_err(), "only one byte for two signals");
		moqi_wake_clear();
	}
}
