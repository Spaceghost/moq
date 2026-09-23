//! Publishing: one broadcast with one track, objects appended group by group.
//!
//! Each object's payload goes out as `sent_us: u64 LE` then the host's bytes,
//! so a subscriber can tell how old it is on arrival whichever moq version the
//! session negotiated (not every one carries timestamps on the wire). The moq
//! timestamp is still set, in microseconds since the track started: moq-lite
//! measures a group's age against it to decide what a late subscriber skips.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use crate::{Error, Node, now_us};

pub struct Publisher {
	pub node: Arc<Node>,
	name: String,
	track_name: String,
	broadcast: moq_net::broadcast::Producer,
	track: Mutex<moq_net::track::Producer>,
	group: Mutex<Option<moq_net::group::Producer>>,
	started: Instant,
	groups: AtomicU64,
	objects: AtomicU64,
	bytes: AtomicU64,
	last_group: AtomicU64,
	last_index: AtomicU64,
}

impl Publisher {
	pub(crate) fn new(node: Arc<Node>, name: &str, track: &str) -> Result<Publisher, Error> {
		let broadcast = node
			.origin
			.create_broadcast(name)
			.map_err(|e| Error::Moq(format!("broadcast {name}: {e}")))?;
		broadcast
			.announce(Default::default())
			.map_err(|e| Error::Moq(format!("announce {name}: {e}")))?;
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO);
		let producer = broadcast
			.create_track(track, info)
			.map_err(|e| Error::Moq(format!("track {track}: {e}")))?;
		Ok(Publisher {
			node,
			name: name.to_owned(),
			track_name: track.to_owned(),
			broadcast,
			track: Mutex::new(producer),
			group: Mutex::new(None),
			started: Instant::now(),
			groups: AtomicU64::new(0),
			objects: AtomicU64::new(0),
			bytes: AtomicU64::new(0),
			last_group: AtomicU64::new(0),
			last_index: AtomicU64::new(0),
		})
	}

	/// Append one object, in a new group when asked (or when none is open).
	pub(crate) fn object(&self, data: &[u8], new_group: bool) -> Result<u64, Error> {
		let mut group = self.group.lock().unwrap_or_else(|e| e.into_inner());
		if new_group || group.is_none() {
			if let Some(g) = group.take() {
				let _ = g.finish();
			}
			let g = self
				.track
				.lock()
				.unwrap_or_else(|e| e.into_inner())
				.append_group()
				.map_err(|e| Error::Moq(format!("group: {e}")))?;
			self.last_group.store(g.sequence, Ordering::Relaxed);
			self.last_index.store(0, Ordering::Relaxed);
			self.groups.fetch_add(1, Ordering::Relaxed);
			*group = Some(g);
		} else {
			self.last_index.fetch_add(1, Ordering::Relaxed);
		}
		let g = group.as_mut().expect("a group is open");
		let mut payload = Vec::with_capacity(8 + data.len());
		payload.extend_from_slice(&now_us().to_le_bytes());
		payload.extend_from_slice(data);
		let ts = moq_net::Timestamp::from_micros(self.started.elapsed().as_micros() as u64)
			.map_err(|e| Error::Moq(format!("timestamp: {e}")))?;
		g.write_frame(ts, bytes::Bytes::from(payload))
			.map_err(|e| Error::Moq(format!("frame: {e}")))?;
		self.objects.fetch_add(1, Ordering::Relaxed);
		self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
		Ok(self.last_group.load(Ordering::Relaxed))
	}

	pub(crate) fn finish(&self) {
		if let Some(g) = self.group.lock().unwrap_or_else(|e| e.into_inner()).take() {
			let _ = g.finish();
		}
		let _ = self.track.lock().unwrap_or_else(|e| e.into_inner()).finish();
		self.broadcast.finish();
	}

	pub(crate) fn stats_json(&self, handle: i32) -> Value {
		json!({
			"h": handle,
			"broadcast": self.name,
			"track": self.track_name,
			"group": self.last_group.load(Ordering::Relaxed),
			"index": self.last_index.load(Ordering::Relaxed),
			"groups": self.groups.load(Ordering::Relaxed),
			"objects": self.objects.load(Ordering::Relaxed),
			"bytes": self.bytes.load(Ordering::Relaxed),
		})
	}
}
