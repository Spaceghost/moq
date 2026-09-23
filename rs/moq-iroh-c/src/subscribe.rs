//! Subscribing: dial a peer, subscribe to one track, queue its objects for the host.
//!
//! moq decides what a late subscriber gets: with a `max_age` budget, a group
//! that has fallen further behind the newest than that is skipped, and one
//! being read when that happens is cut short (its next read ends with
//! `Error::Old`). Both are counted here, and the object after them is flagged
//! `MOQI_OBJ_GAP`, so the host knows to wait for a group start (a key frame).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use moq_tokio::iroh::web_transport_iroh::iroh::EndpointAddr;
use serde_json::{Value, json};

use crate::{Error, MOQI_OBJ_GAP, MOQI_OBJ_GROUP_START, Node, moqi_object, now_us, wake};

/// Objects queued for a host that is not reading. Past this the oldest group
/// goes (whole: a group without its start is useless) and the next object is a gap.
const QUEUE_MAX: usize = 256;

struct Obj {
	info: moqi_object,
	data: Bytes,
}

#[derive(Default)]
struct Inner {
	queue: VecDeque<Obj>,
	state: &'static str,
	why: String,
	conn: u64,
	group: u64,
	index: u64,
	latest: u64,
	objects: u64,
	groups: u64,
	skipped: u64,
	cut: u64,
	late: u64,
	dropped: u64,
	bytes: u64,
	lat_us: u64,
	lat_avg_us: f64,
	gap: bool,
}

pub struct Subscription {
	pub node: Arc<Node>,
	peer: String,
	broadcast: String,
	track: String,
	max_age_ms: u32,
	throttle_ms: AtomicU32,
	inner: Mutex<Inner>,
	task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Subscription {
	pub(crate) fn start(
		_id: i32,
		node: Arc<Node>,
		addr: EndpointAddr,
		broadcast: String,
		track: String,
		max_age_ms: u32,
	) -> Arc<Subscription> {
		let s = Arc::new(Subscription {
			node,
			peer: addr.id.to_string(),
			broadcast,
			track,
			max_age_ms,
			throttle_ms: AtomicU32::new(0),
			inner: Mutex::new(Inner {
				state: "dialing",
				..Default::default()
			}),
			task: Mutex::new(None),
		});
		let me = s.clone();
		let task = crate::RUNTIME.spawn(async move {
			let res = me.clone().run(addr).await;
			let mut i = me.lock();
			i.state = "ended";
			i.why = match res {
				Ok(()) => "finished".into(),
				Err(e) => e,
			};
			drop(i);
			wake::signal();
		});
		*s.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
		s
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
		self.inner.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn set_state(&self, state: &'static str) {
		self.lock().state = state;
	}

	async fn run(self: Arc<Self>, addr: EndpointAddr) -> Result<(), String> {
		let (n, session) = self.node.dial(addr).await?;
		{
			let mut i = self.lock();
			i.conn = n;
			i.state = "connected";
		}
		let origin = moq_tokio::origin::spawn();
		let client = moq_net::Client::new()
			.with_subscriber(origin.clone())
			.with_stats(self.node.conn_stats(n));
		let (session, driver) = client
			.connect(
				tokio::time::Instant::now().into_std(),
				moq_tokio::transport::Session::new(session),
			)
			.await
			.map_err(|e| format!("moq session: {e}"))?;
		tokio::spawn(moq_net::time::run(driver));

		let consumer = origin.consume();
		let broadcast = tokio::select! {
			biased;
			err = session.closed() => return Err(format!("session closed: {err}")),
			// waits for the peer's announce, which follows the session's setup
			bc = consumer.routed_broadcast(self.broadcast.as_str()) => bc.map_err(|e| format!("broadcast {}: {e}", self.broadcast))?,
		};
		self.set_state("subscribing");
		let want = moq_net::track::Subscription::default().with_max_age(Duration::from_millis(self.max_age_ms as u64));
		let mut sub = broadcast
			.track(&self.track)
			.map_err(|e| format!("track {}: {e}", self.track))?
			.subscribe(want)
			.await
			.map_err(|e| format!("subscribe {}: {e}", self.track))?;
		self.set_state("live");

		let mut last: Option<u64> = None;
		loop {
			let group = tokio::select! {
				biased;
				err = session.closed() => return Err(format!("session closed: {err}")),
				g = sub.recv_group() => g.map_err(|e| format!("track: {e}"))?,
			};
			let Some(mut group) = group else { return Ok(()) };
			let seq = group.sequence;
			if let Some(l) = last
				&& seq <= l
			{
				// arrived after a newer one: moq hands it over anyway, it is of no use to a live view
				self.lock().late += 1;
				continue;
			}
			{
				let mut i = self.lock();
				if let Some(l) = last
					&& seq > l + 1
				{
					i.skipped += seq - l - 1;
					i.gap = true;
				}
				i.latest = sub.latest().unwrap_or(seq);
			}
			last = Some(seq);
			let mut index: u64 = 0;
			loop {
				let frame = tokio::select! {
					biased;
					err = session.closed() => return Err(format!("session closed: {err}")),
					f = group.read_frame() => f,
				};
				match frame {
					Ok(Some(frame)) => {
						let delay = self.throttle_ms.load(Ordering::Relaxed);
						if delay > 0 {
							tokio::time::sleep(Duration::from_millis(delay as u64)).await;
						}
						self.push(seq, index, frame.payload);
						index += 1;
					}
					Ok(None) => {
						self.lock().groups += 1;
						break;
					}
					Err(e) => {
						// moq gave up on this group (a newer one is past the budget)
						tracing::debug!(%e, group = seq, "group cut");
						let mut i = self.lock();
						i.cut += 1;
						i.gap = true;
						break;
					}
				}
			}
		}
	}

	fn push(&self, group: u64, index: u64, payload: Bytes) {
		let recv_us = now_us();
		let (sent_us, data) = if payload.len() >= 8 {
			let mut b = [0u8; 8];
			b.copy_from_slice(&payload[..8]);
			(u64::from_le_bytes(b), payload.slice(8..))
		} else {
			(recv_us, payload)
		};
		let lat = recv_us.saturating_sub(sent_us);
		let mut i = self.lock();
		if i.queue.len() >= QUEUE_MAX {
			// the host is not reading: drop the oldest group whole
			let first = i.queue.front().map(|o| o.info.group);
			while i.queue.front().map(|o| o.info.group) == first && !i.queue.is_empty() {
				i.queue.pop_front();
				i.dropped += 1;
			}
			if let Some(o) = i.queue.front_mut() {
				o.info.flags |= MOQI_OBJ_GAP;
			} else {
				i.gap = true;
			}
		}
		let mut flags = 0;
		if index == 0 {
			flags |= MOQI_OBJ_GROUP_START;
		}
		if i.gap {
			flags |= MOQI_OBJ_GAP;
			i.gap = false;
		}
		i.group = group;
		i.index = index;
		i.objects += 1;
		i.bytes += data.len() as u64;
		i.lat_us = lat;
		i.lat_avg_us = if i.objects == 1 {
			lat as f64
		} else {
			i.lat_avg_us * 0.9 + lat as f64 * 0.1
		};
		i.queue.push_back(Obj {
			info: moqi_object {
				group,
				index,
				sent_us,
				recv_us,
				len: data.len() as u64,
				flags,
			},
			data,
		});
		drop(i);
		wake::signal();
	}

	pub(crate) fn read(&self, out: &mut moqi_object, buf: *mut u8, cap: usize) -> Result<i32, Error> {
		let mut i = self.lock();
		let Some(front) = i.queue.front() else {
			if i.state == "ended" {
				return Err(Error::Ended(i.why.clone()));
			}
			return Ok(0);
		};
		*out = front.info;
		let n = front.data.len();
		if n > cap || (buf.is_null() && n > 0) {
			return Err(Error::Small(n));
		}
		let obj = i.queue.pop_front().expect("front exists");
		if n > 0 {
			// SAFETY: `buf` holds `cap >= n` bytes, per the caller.
			unsafe { std::ptr::copy_nonoverlapping(obj.data.as_ptr(), buf, n) };
		}
		Ok(1)
	}

	pub(crate) fn set_throttle(&self, ms: u32) {
		self.throttle_ms.store(ms, Ordering::Relaxed);
	}

	pub(crate) fn close(&self) {
		if let Some(t) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
			t.abort();
		}
		let mut i = self.lock();
		i.state = "ended";
		i.why = "closed".into();
	}

	pub(crate) fn stats_json(&self, handle: i32) -> Value {
		let i = self.lock();
		json!({
			"h": handle,
			"peer": self.peer,
			"broadcast": self.broadcast,
			"track": self.track,
			"state": i.state,
			"why": i.why,
			"conn": i.conn,
			"group": i.group,
			"index": i.index,
			"latest": i.latest,
			"behind": i.latest.saturating_sub(i.group),
			"objects": i.objects,
			"groups": i.groups,
			"skipped": i.skipped,
			"cut": i.cut,
			"late": i.late,
			"dropped": i.dropped,
			"bytes": i.bytes,
			"queued": i.queue.len(),
			"lat_us": i.lat_us,
			"lat_avg_us": i.lat_avg_us as u64,
			"max_age_ms": self.max_age_ms,
			"throttle_ms": self.throttle_ms.load(Ordering::Relaxed),
		})
	}
}
