//! A node: one iroh endpoint, one moq origin served to every peer that connects,
//! and the record of its connections for display.

use std::collections::VecDeque;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::StreamExt;
use moq_tokio::iroh::web_transport_iroh::{
	self,
	iroh::{
		self, Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
		endpoint::{Connection, PathEvent, presets},
	},
};
use serde_json::{Value, json};

use crate::{Error, Publisher, Subscription};

/// Path events kept for the timeline.
const EVENTS_KEPT: usize = 48;
/// Closed connections kept on show, so a drop is seen before it goes.
const CLOSED_KEPT: usize = 8;

pub struct Conn {
	pub n: u64,
	/// "in": the peer dialed us (it subscribes to what we publish); "out": we dialed it.
	pub dir: &'static str,
	pub peer: EndpointId,
	pub conn: Connection,
	pub opened: Instant,
	pub closed: Option<(Instant, String)>,
}

pub struct Event {
	pub at: Instant,
	pub conn: u64,
	pub what: &'static str,
	pub relay: bool,
	pub addr: String,
}

pub struct Node {
	pub endpoint: Endpoint,
	pub origin: moq_net::origin::Producer,
	pub registry: moq_net::stats::Registry,
	pub started: Instant,
	pub relay_mode: String,
	pub conns: Mutex<Vec<Conn>>,
	pub events: Mutex<VecDeque<Event>>,
	next_conn: AtomicU64,
	accept: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn load_secret(path: Option<&str>) -> Result<SecretKey, Error> {
	let Some(path) = path else {
		return Ok(SecretKey::generate());
	};
	let path = Path::new(path);
	if path.exists() {
		let text = std::fs::read_to_string(path).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
		return SecretKey::from_str(text.trim()).map_err(|e| Error::Io(format!("{}: {e}", path.display())));
	}
	let key = SecretKey::generate();
	let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
	let mut opts = std::fs::OpenOptions::new();
	opts.write(true).create_new(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		opts.mode(0o600);
	}
	use std::io::Write;
	let mut f = opts
		.open(path)
		.map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
	f.write_all(hex.as_bytes())
		.map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
	Ok(key)
}

fn short(id: &EndpointId) -> String {
	id.fmt_short().to_string()
}

impl Node {
	pub(crate) async fn bind(secret_path: Option<String>, relay: String, port: u16) -> Result<Arc<Node>, Error> {
		let secret = load_secret(secret_path.as_deref())?;

		// moq-lite and moq-transport over raw QUIC; no WebTransport here.
		let alpns: Vec<Vec<u8>> = moq_net::ALPNS.iter().map(|a| a.as_bytes().to_vec()).collect();
		// moq opens a stream per group: raise iroh's low default, as moq-tokio does.
		let transport = iroh::endpoint::QuicTransportConfig::builder()
			.max_concurrent_uni_streams(iroh::endpoint::VarInt::from_u32(1024))
			.max_concurrent_bidi_streams(iroh::endpoint::VarInt::from_u32(1024))
			.build();

		let builder = match relay.as_str() {
			"default" => Endpoint::builder(presets::N0),
			"off" => Endpoint::builder(presets::N0DisableRelay),
			url => {
				let url: RelayUrl = url.parse().map_err(|e| Error::Arg(format!("relay {url}: {e}")))?;
				let b = Endpoint::builder(presets::N0)
					.relay_mode(RelayMode::Custom(RelayMap::from(url)))
					.clear_address_lookup();
				// the tests' in-process relay has a self-signed certificate
				#[cfg(test)]
				let b = b.ca_tls_config(iroh::tls::CaTlsConfig::insecure_skip_verify());
				b
			}
		};
		let mut builder = builder.secret_key(secret).alpns(alpns).transport_config(transport);
		let bind: std::net::SocketAddr = (std::net::Ipv4Addr::UNSPECIFIED, port).into();
		builder = builder
			.bind_addr(bind)
			.map_err(|e| Error::Io(format!("bind {bind}: {e}")))?;
		let endpoint = builder.bind().await.map_err(|e| Error::Io(format!("bind: {e}")))?;

		let node = Arc::new(Node {
			endpoint,
			origin: moq_tokio::origin::spawn(),
			registry: moq_net::stats::Registry::new(moq_net::stats::Config::new()),
			started: Instant::now(),
			relay_mode: relay,
			conns: Mutex::new(Vec::new()),
			events: Mutex::new(VecDeque::new()),
			next_conn: AtomicU64::new(1),
			accept: Mutex::new(None),
		});
		let weak = Arc::downgrade(&node);
		let ep = node.endpoint.clone();
		let task = tokio::spawn(async move {
			while let Some(incoming) = ep.accept().await {
				let Some(node) = weak.upgrade() else { break };
				tokio::spawn(async move {
					if let Err(e) = node.serve(incoming).await {
						tracing::debug!(%e, "incoming session failed");
					}
				});
			}
		});
		*node.accept.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
		Ok(node)
	}

	/// An incoming connection: serve our origin to it until it goes.
	async fn serve(self: Arc<Self>, incoming: iroh::endpoint::Incoming) -> Result<(), String> {
		let conn = incoming
			.accept()
			.map_err(|e| e.to_string())?
			.await
			.map_err(|e| e.to_string())?;
		let n = self.track_conn("in", conn.clone());
		let session = web_transport_iroh::QuicRequest::accept(conn).ok();
		let stats = self
			.registry
			.tier(moq_net::stats::Tier::new(format!("c{n}")))
			.session("");
		let server = moq_net::Server::new()
			.with_publisher(self.origin.consume())
			.with_stats(stats);
		let (session, driver) = server
			.accept(
				tokio::time::Instant::now().into_std(),
				moq_tokio::transport::Session::new(session),
			)
			.await
			.map_err(|e| e.to_string())?;
		tokio::spawn(moq_net::time::run(driver));
		let err = session.closed().await;
		tracing::debug!(%err, conn = n, "session ended");
		Ok(())
	}

	/// Dial a peer for moq; the connection is on show from then on.
	pub(crate) async fn dial(
		self: &Arc<Self>,
		addr: EndpointAddr,
	) -> Result<(u64, web_transport_iroh::Session), String> {
		let alpn = moq_net::ALPNS[0].as_bytes();
		let more: Vec<Vec<u8>> = moq_net::ALPNS[1..].iter().map(|a| a.as_bytes().to_vec()).collect();
		let opts = iroh::endpoint::ConnectOptions::new().with_additional_alpns(more);
		let connecting = self
			.endpoint
			.connect_with_opts(addr, alpn, opts)
			.await
			.map_err(|e| e.to_string())?;
		let conn = connecting.await.map_err(|e| e.to_string())?;
		let n = self.track_conn("out", conn.clone());
		Ok((n, web_transport_iroh::Session::raw(conn)))
	}

	/// The stats session for traffic on our connection `n`.
	pub(crate) fn conn_stats(&self, n: u64) -> moq_net::stats::Session {
		self.registry
			.tier(moq_net::stats::Tier::new(format!("c{n}")))
			.session("")
	}

	fn track_conn(self: &Arc<Self>, dir: &'static str, conn: Connection) -> u64 {
		let n = self.next_conn.fetch_add(1, Ordering::Relaxed);
		let now = Instant::now();
		// what the connection starts on, before the first path event
		for p in conn.paths().iter() {
			self.event(n, "opened", p.is_relay(), p.remote_addr().to_string());
			if p.is_selected() {
				self.event(n, "selected", p.is_relay(), p.remote_addr().to_string());
			}
		}
		let mut events = conn.path_events();
		let peer = conn.remote_id();
		{
			let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
			conns.push(Conn {
				n,
				dir,
				peer,
				conn: conn.clone(),
				opened: now,
				closed: None,
			});
		}
		let weak = Arc::downgrade(self);
		tokio::spawn(async move {
			while let Some(ev) = events.next().await {
				let Some(node) = weak.upgrade() else { return };
				match ev {
					PathEvent::Opened { remote_addr, .. } => {
						node.event(n, "opened", remote_addr.is_relay(), remote_addr.to_string())
					}
					PathEvent::Closed { remote_addr, .. } => {
						node.event(n, "closed", remote_addr.is_relay(), remote_addr.to_string())
					}
					PathEvent::Selected { remote_addr, .. } => {
						node.event(n, "selected", remote_addr.is_relay(), remote_addr.to_string())
					}
					_ => {}
				}
			}
			let Some(node) = weak.upgrade() else { return };
			let why = conn
				.close_reason()
				.map(|r| r.to_string())
				.unwrap_or_else(|| "closed".into());
			node.event(n, "gone", false, why.clone());
			let mut conns = node.conns.lock().unwrap_or_else(|e| e.into_inner());
			if let Some(c) = conns.iter_mut().find(|c| c.n == n) {
				c.closed = Some((Instant::now(), why));
			}
			// keep the last few closed ones on show
			let closed = conns.iter().filter(|c| c.closed.is_some()).count();
			if closed > CLOSED_KEPT
				&& let Some(i) = conns.iter().position(|c| c.closed.is_some())
			{
				conns.remove(i);
			}
		});
		n
	}

	fn event(&self, conn: u64, what: &'static str, relay: bool, addr: String) {
		let mut ev = self.events.lock().unwrap_or_else(|e| e.into_inner());
		if ev.len() >= EVENTS_KEPT {
			ev.pop_front();
		}
		ev.push_back(Event {
			at: Instant::now(),
			conn,
			what,
			relay,
			addr,
		});
	}

	pub(crate) async fn close(&self) {
		if let Some(t) = self.accept.lock().unwrap_or_else(|e| e.into_inner()).take() {
			t.abort();
		}
		self.endpoint.close().await;
	}

	fn ms(&self, at: Instant) -> u64 {
		at.saturating_duration_since(self.started).as_millis() as u64
	}

	pub(crate) fn stats_json(&self, pubs: &[(i32, Arc<Publisher>)], subs: &[(i32, Arc<Subscription>)]) -> Value {
		let now = Instant::now();
		let addr = self.endpoint.addr();
		let home: Vec<Value> = self
			.endpoint
			.home_relay_status()
			.get()
			.iter()
			.map(|r| json!({ "url": r.url().to_string(), "connected": r.is_connected() }))
			.collect();

		// moq traffic per connection, from moq-net's own counters
		let snapshot = self.registry.snapshot();
		let traffic = |n: u64, role: moq_net::stats::Role| -> Value {
			let label = format!("c{n}");
			let mut t = moq_net::stats::Traffic::default();
			for (tier, r, tr) in snapshot.traffic() {
				if r == role && tier.as_str() == label {
					t.add(tr);
				}
			}
			json!({
				"bytes": t.bytes, "frames": t.frames, "groups": t.groups,
				"subs": t.active_subscriptions(),
				"stale_groups": t.stale.groups, "stale_frames": t.stale.frames,
			})
		};

		let conns: Vec<Value> = self
			.conns
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.map(|c| {
				let paths: Vec<Value> = if c.closed.is_some() {
					Vec::new()
				} else {
					c.conn
						.paths()
						.iter()
						.map(|p| {
							json!({
								"id": format!("{:?}", p.id()),
								"kind": if p.is_relay() { "relay" } else { "direct" },
								"addr": p.remote_addr().to_string(),
								"selected": p.is_selected(),
								"rtt_us": p.rtt().as_micros() as u64,
							})
						})
						.collect()
				};
				let role = if c.dir == "in" {
					moq_net::stats::Role::Publisher
				} else {
					moq_net::stats::Role::Subscriber
				};
				json!({
					"n": c.n,
					"dir": c.dir,
					"peer": c.peer.to_string(),
					"peer_short": short(&c.peer),
					"age_ms": now.saturating_duration_since(c.opened).as_millis() as u64,
					"open": c.closed.is_none(),
					"closed_why": c.closed.as_ref().map(|(_, w)| w.clone()),
					"paths": paths,
					"moq": traffic(c.n, role),
				})
			})
			.collect();

		let events: Vec<Value> = self
			.events
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.iter()
			.map(|e| {
				json!({ "t_ms": self.ms(e.at), "conn": e.conn, "what": e.what,
					"kind": if e.relay { "relay" } else { "direct" }, "addr": e.addr })
			})
			.collect();

		json!({
			"t_ms": self.ms(now),
			"id": addr.id.to_string(),
			"id_short": short(&addr.id),
			"ticket": crate::ticket::format(&addr),
			"relay_mode": self.relay_mode,
			"relays": home,
			"addrs": addr.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
			"conns": conns,
			"events": events,
			"pubs": pubs.iter().map(|(h, p)| p.stats_json(*h)).collect::<Vec<_>>(),
			"subs": subs.iter().map(|(h, s)| s.stats_json(*h)).collect::<Vec<_>>(),
		})
	}
}
