//! Tickets: how one node tells another where it is.
//!
//! `iroh://<endpoint id>?relay=<url>&addr=<ip:port>&addr=...`. The endpoint id
//! alone is enough where address lookup is on (the `"default"` relay mode); the
//! relay URL and direct addresses let a peer dial without it.

use moq_tokio::iroh::web_transport_iroh::iroh::{EndpointAddr, EndpointId, RelayUrl};
use url::Url;

use crate::Error;

pub(crate) fn format(addr: &EndpointAddr) -> String {
	let mut url = Url::parse(&format!("iroh://{}", addr.id)).expect("an endpoint id is a valid host");
	{
		let mut q = url.query_pairs_mut();
		for r in addr.relay_urls() {
			q.append_pair("relay", r.as_str());
		}
		for a in addr.ip_addrs() {
			q.append_pair("addr", &a.to_string());
		}
	}
	let s = url.to_string();
	s.strip_suffix('?').map(str::to_owned).unwrap_or(s)
}

pub(crate) fn parse(text: &str) -> Result<EndpointAddr, Error> {
	let text = text.trim();
	if !text.contains("://") {
		let id: EndpointId = text.parse().map_err(|e| Error::Arg(format!("endpoint id: {e}")))?;
		return Ok(EndpointAddr::new(id));
	}
	let url = Url::parse(text).map_err(|e| Error::Arg(format!("ticket: {e}")))?;
	if url.scheme() != "iroh" {
		return Err(Error::Arg("ticket: not an iroh:// ticket".into()));
	}
	let host = url
		.host_str()
		.ok_or_else(|| Error::Arg("ticket: no endpoint id".into()))?;
	let id: EndpointId = host.parse().map_err(|e| Error::Arg(format!("endpoint id: {e}")))?;
	let mut addr = EndpointAddr::new(id);
	for (k, v) in url.query_pairs() {
		match k.as_ref() {
			"relay" => {
				let r: RelayUrl = v.parse().map_err(|e| Error::Arg(format!("relay url: {e}")))?;
				addr = addr.with_relay_url(r);
			}
			"addr" => {
				let a = v.parse().map_err(|e| Error::Arg(format!("addr {v}: {e}")))?;
				addr = addr.with_ip_addr(a);
			}
			_ => {} // later fields are for later readers
		}
	}
	Ok(addr)
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_tokio::iroh::web_transport_iroh::iroh::SecretKey;

	#[test]
	fn round_trip() {
		let id = SecretKey::generate().public();
		let addr = EndpointAddr::new(id)
			.with_relay_url("https://relay.example.net./".parse().unwrap())
			.with_ip_addr("192.0.2.7:4433".parse().unwrap())
			.with_ip_addr("[2001:db8::1]:9".parse().unwrap());
		let t = format(&addr);
		assert!(t.starts_with(&format!("iroh://{id}?")), "{t}");
		let back = parse(&t).unwrap();
		assert_eq!(back.id, id);
		assert_eq!(back.relay_urls().count(), 1);
		assert_eq!(back.ip_addrs().count(), 2);
	}

	#[test]
	fn bare_id_and_no_query() {
		let id = SecretKey::generate().public();
		assert_eq!(parse(&id.to_string()).unwrap().id, id);
		let t = format(&EndpointAddr::new(id));
		assert_eq!(t, format!("iroh://{id}"));
		assert_eq!(parse(&t).unwrap().id, id);
	}

	#[test]
	fn rejects_garbage() {
		assert!(parse("https://example.com").is_err());
		assert!(parse("iroh://not-a-key").is_err());
		assert!(parse("nope").is_err());
		let id = SecretKey::generate().public();
		assert!(parse(&format!("iroh://{id}?addr=bogus")).is_err());
	}
}
