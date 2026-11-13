//! Web Push (VAPID) sender, SSRF-safe endpoint validation and the delivery step shared by the dispatcher
//! (`workers/push.rs`) and `POST /api/notifications/push/test`. OWNER: Subagent C2.
//!
//! Security model for outbound requests (the endpoint URL is attacker-controlled: any logged-in user may register one):
//! 1. [`validate_endpoint`] (syntactic): `https://`, <= 600 chars, ASCII only, no userinfo / backslash / percent-escapes in
//!    the authority, port 443 only, host must be a multi-label DNS name (every IP literal in any notation is refused, as are
//!    `localhost`, `*.local`, `*.internal`, ...). Run at subscribe time AND again at send time.
//! 2. At send time the host is resolved by us, **every** resolved address must be public ([`is_public_ip`]) and the HTTP
//!    client is pinned to exactly those addresses (`resolve_to_addrs`), so DNS-rebinding and `127.0.0.1.nip.io` style names
//!    cannot reach internal services.
//! 3. A dedicated client: `redirect(none)`, short connect/total timeouts, `no_proxy()` (an env proxy would bypass the
//!    pin), HTTPS only, and the response body is never read. `AppState::http` is deliberately NOT used (it follows redirects).
use crate::{config::Config, state::AppState};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::StreamExt;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::json;
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::Duration,
};
use uuid::Uuid;
use web_push_native::{jwt_simple::algorithms::ES256KeyPair, Auth, WebPushBuilder};

pub const MAX_ENDPOINT_LEN: usize = 600;
/// Active browser subscriptions kept per user (oldest are dropped).
pub const MAX_SUBSCRIPTIONS_PER_USER: i64 = 10;

// ---------------------------------------------------------------------------------------------------------------
// VAPID keys
// ---------------------------------------------------------------------------------------------------------------

/// `water-erp gen-vapid`: prints a fresh key pair in the format the server reads from its environment.
pub fn print_new_vapid_keys() -> anyhow::Result<()> {
    let (private, public) = generate_vapid();
    println!("VAPID_PRIVATE_KEY={private}");
    println!("VAPID_PUBLIC_KEY={public}");
    Ok(())
}

/// (private, public): base64url(no padding) of the raw 32-byte scalar and of the 65-byte uncompressed point.
pub fn generate_vapid() -> (String, String) {
    let sk = p256::SecretKey::random(&mut rand::rngs::OsRng);
    let private = URL_SAFE_NO_PAD.encode(sk.to_bytes());
    let public = URL_SAFE_NO_PAD.encode(sk.public_key().to_encoded_point(false).as_bytes());
    (private, public)
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(s.trim().trim_end_matches('=')).map_err(|_| "not valid base64url".to_string())
}

pub struct VapidKeys {
    pub key_pair: ES256KeyPair,
    /// Always derived from the private key, so `/push/key` can never advertise a mismatching public key.
    pub public_b64: String,
    pub subject: String,
}

impl VapidKeys {
    /// `None` when no (valid) private key is configured: Web Push is then disabled.
    pub fn from_config(cfg: &Config) -> Option<VapidKeys> {
        let raw = b64(cfg.vapid_private_key.as_deref()?).ok()?;
        let sk = p256::SecretKey::from_slice(&raw).ok()?;
        let key_pair = ES256KeyPair::from_bytes(&raw).ok()?;
        let public_b64 = URL_SAFE_NO_PAD.encode(sk.public_key().to_encoded_point(false).as_bytes());
        let s = cfg.vapid_subject.trim();
        let subject = if s.starts_with("mailto:") || s.starts_with("https://") { s.to_string() } else { "https://localhost".to_string() };
        Some(VapidKeys { key_pair, public_b64, subject })
    }
}

impl VapidKeys {
    /// Encrypt `payload` for the target's browser keys and sign it with VAPID. `Err(Gone)` = unusable stored keys/endpoint.
    pub fn build_request(&self, t: &PushTarget, payload: &[u8]) -> Result<http::Request<Vec<u8>>, PushOutcome> {
        let (Ok(dh), Ok(auth)) = (b64(&t.p256dh), b64(&t.auth)) else { return Err(PushOutcome::Gone) };
        let Ok(ua_public) = p256::PublicKey::from_sec1_bytes(&dh) else { return Err(PushOutcome::Gone) };
        if auth.len() != 16 { return Err(PushOutcome::Gone); }
        let Ok(uri) = t.endpoint.parse::<http::Uri>() else { return Err(PushOutcome::Gone) };
        WebPushBuilder::new(uri, ua_public, Auth::clone_from_slice(&auth))
            .with_vapid(&self.key_pair, &self.subject)
            .build(payload.to_vec())
            .map_err(|e| PushOutcome::Failed(format!("encryption failed: {e}")))
    }
}

// ---------------------------------------------------------------------------------------------------------------
// SSRF defence
// ---------------------------------------------------------------------------------------------------------------

const BLOCKED_SUFFIXES: &[&str] = &[
    "localhost", "local", "internal", "lan", "home", "corp", "intranet", "localdomain", "arpa", "invalid", "test", "example", "onion", "private",
];

/// Syntactic validation of a push endpoint. Returns a human-readable reason on rejection.
pub fn validate_endpoint(url: &str) -> Result<(), String> {
    if url.is_empty() { return Err("endpoint is required".into()); }
    if url.len() > MAX_ENDPOINT_LEN { return Err(format!("endpoint is too long (max {MAX_ENDPOINT_LEN} characters)")); }
    if !url.bytes().all(|b| b.is_ascii_graphic()) { return Err("endpoint contains invalid characters".into()); }
    if url.contains('\\') { return Err("endpoint contains invalid characters".into()); }
    let rest = match url.get(..8) {
        Some(p) if p.eq_ignore_ascii_case("https://") => &url[8..],
        _ => return Err("endpoint must start with https://".into()),
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..auth_end];
    if authority.is_empty() { return Err("endpoint has no host".into()); }
    if authority.contains('@') { return Err("endpoint must not contain credentials".into()); }
    if authority.contains('%') { return Err("endpoint host must not contain escapes".into()); }
    if authority.starts_with('[') || authority.contains('[') || authority.contains(']') { return Err("IP address endpoints are not allowed".into()); }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    if host.contains(':') { return Err("IP address endpoints are not allowed".into()); }
    if let Some(p) = port {
        if p != "443" { return Err("only port 443 is allowed".into()); }
    }
    validate_host(host)
}

fn validate_host(host: &str) -> Result<(), String> {
    let host = host.to_ascii_lowercase();
    let host = host.as_str();
    // a trailing dot ("example.com.") is a different key for DNS pinning than "example.com": refuse it outright
    if host.is_empty() || host.len() > 253 { return Err("invalid host name".into()); }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 { return Err("host must be a fully-qualified DNS name".into()); }
    for l in &labels {
        if l.is_empty() || l.len() > 63 { return Err("invalid host name".into()); }
        if !l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') { return Err("invalid host name".into()); }
        if l.starts_with('-') || l.ends_with('-') { return Err("invalid host name".into()); }
    }
    let tld = labels[labels.len() - 1];
    // numeric / hex / octal IPv4 notations (127.0.0.1, 2130706433, 0x7f.1, 017700000001 ...) all end in a numeric-looking label
    if tld.bytes().all(|b| b.is_ascii_digit()) || tld.starts_with("0x") || !tld.bytes().any(|b| b.is_ascii_lowercase()) {
        return Err("IP address endpoints are not allowed".into());
    }
    if BLOCKED_SUFFIXES.contains(&tld) { return Err("endpoint host is not a public push service".into()); }
    // dotted-quad-looking prefixes such as 127.0.0.1.nip.io are caught at send time by the resolved-address check
    Ok(())
}

/// `true` only for globally routable unicast addresses.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(a: Ipv4Addr) -> bool {
    let o = a.octets();
    if a.is_unspecified() || a.is_loopback() || a.is_private() || a.is_link_local() || a.is_broadcast() || a.is_multicast() { return false; }
    !(o[0] == 0                                           // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xC0) == 64)           // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)        // 192.0.0.0/24 IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 2)        // TEST-NET-1
        || (o[0] == 198 && (o[1] & 0xFE) == 18)           // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && o[1] == 51 && o[2] == 100)     // TEST-NET-2
        || (o[0] == 203 && o[1] == 0 && o[2] == 113)      // TEST-NET-3
        || o[0] >= 240)                                   // 240.0.0.0/4 reserved
}

fn is_public_v6(a: Ipv6Addr) -> bool {
    if a.is_unspecified() || a.is_loopback() || a.is_multicast() { return false; }
    let s = a.segments();
    // embedded IPv4 forms must pass the IPv4 rules: ::ffff:a.b.c.d, ::a.b.c.d, 64:ff9b::/96 (NAT64), 2002::/16 (6to4)
    let embedded = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
    if s[0..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff { return is_public_v4(embedded(s[6], s[7])); }
    if s[0..6] == [0, 0, 0, 0, 0, 0] { return false; }                                  // IPv4-compatible / loopback / unspecified
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] { return is_public_v4(embedded(s[6], s[7])); }
    if s[0] == 0x2002 { return is_public_v4(embedded(s[1], s[2])); }
    !((s[0] & 0xFE00) == 0xFC00                           // fc00::/7 unique local
        || (s[0] & 0xFFC0) == 0xFE80                      // fe80::/10 link local
        || (s[0] & 0xFFC0) == 0xFEC0                      // fec0::/10 site local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8)             // documentation
        || (s[0] == 0x2001 && s[1] == 0)                  // Teredo
        || s[0] == 0x0100)                                // 100::/64 discard
}

/// Host part of an already validated endpoint.
pub fn endpoint_host(url: &str) -> Option<String> {
    let rest = url.get(8..)?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let a = &rest[..end];
    let h = a.rsplit_once(':').map(|(h, _)| h).unwrap_or(a);
    Some(h.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PushTarget {
    pub id: Uuid,
    pub user_id: Uuid,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    Delivered,
    /// 404 / 410 (or an endpoint that no longer passes validation): the subscription must be deactivated.
    Gone,
    Failed(String),
}

/// The delivery step is injectable so the dispatcher can be tested without network access.
#[async_trait]
pub trait PushSender: Send + Sync {
    async fn send(&self, target: &PushTarget, payload: &[u8]) -> PushOutcome;
}

pub struct WebPushSender { keys: VapidKeys }

impl WebPushSender {
    pub fn from_state(state: &AppState) -> Option<WebPushSender> {
        VapidKeys::from_config(&state.cfg).map(|keys| WebPushSender { keys })
    }
}

#[async_trait]
impl PushSender for WebPushSender {
    async fn send(&self, t: &PushTarget, payload: &[u8]) -> PushOutcome {
        if validate_endpoint(&t.endpoint).is_err() { return PushOutcome::Gone; }
        let Some(host) = endpoint_host(&t.endpoint) else { return PushOutcome::Gone };
        // 1. resolve ourselves; refuse when ANY address is non-public
        let addrs: Vec<std::net::SocketAddr> = match tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host.as_str(), 443))).await {
            Ok(Ok(it)) => it.collect(),
            Ok(Err(_)) => return PushOutcome::Failed("DNS lookup failed".into()),
            Err(_) => return PushOutcome::Failed("DNS lookup timed out".into()),
        };
        if addrs.is_empty() { return PushOutcome::Failed("DNS lookup returned no addresses".into()); }
        if addrs.iter().any(|a| !is_public_ip(a.ip())) {
            tracing::warn!(host = %host, "push endpoint resolves to a non-public address; refused");
            return PushOutcome::Gone;
        }
        // 2. build the encrypted request
        let req = match self.keys.build_request(t, payload) { Ok(r) => r, Err(o) => return o };
        // 3. dedicated, pinned, redirect-free client
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .https_only(true)
            .user_agent("water-erp-push/1")
            .resolve_to_addrs(&host, &addrs)
            .build()
        {
            Ok(c) => c,
            Err(_) => return PushOutcome::Failed("http client error".into()),
        };
        let (parts, body) = req.into_parts();
        let mut rb = client.post(parts.uri.to_string());
        for (k, v) in parts.headers.iter() { rb = rb.header(k, v); }
        match rb.body(body).send().await {
            Ok(r) => {
                let s = r.status().as_u16();
                match s {
                    200..=299 => PushOutcome::Delivered,
                    404 | 410 => PushOutcome::Gone,
                    _ => PushOutcome::Failed(format!("HTTP {s}")),
                }
            }
            Err(e) if e.is_timeout() => PushOutcome::Failed("timeout".into()),
            Err(_) => PushOutcome::Failed("connection failed".into()),
        }
    }
}

/// JSON payload `{title,body,url,tag,alert_id}`; `url` is forced to a same-origin relative path, text is length-bounded
/// (Web Push payloads are limited to ~4 KB once encrypted).
pub fn build_payload(title: &str, body: &str, url: &str, tag: &str, alert_id: Option<Uuid>) -> Vec<u8> {
    let cut = |s: &str, n: usize| -> String { s.chars().filter(|c| !c.is_control()).take(n).collect() };
    let url = if url.starts_with('/') && !url.starts_with("//") && !url.contains('\\') && url.len() <= 300 { url.to_string() } else { "/".to_string() };
    json!({ "title": cut(title, 120), "body": cut(body, 400), "url": url, "tag": cut(tag, 100), "alert_id": alert_id }).to_string().into_bytes()
}

/// Send `payload` to every target (bounded concurrency) and record the result on the subscription rows:
/// success stamps `last_success_at`, 404/410 deactivates, other failures only record `last_error`.
pub async fn deliver(state: &AppState, sender: &dyn PushSender, targets: Vec<PushTarget>, payload: &[u8]) -> Vec<(PushTarget, PushOutcome)> {
    let results: Vec<(PushTarget, PushOutcome)> = futures::stream::iter(targets)
        .map(|t| async move { let o = sender.send(&t, payload).await; (t, o) })
        .buffer_unordered(8)
        .collect()
        .await;
    for (t, o) in &results {
        let r = match o {
            PushOutcome::Delivered => sqlx::query("UPDATE push_subscriptions SET last_success_at = now(), last_error = NULL WHERE id = $1").bind(t.id).execute(&state.db).await,
            PushOutcome::Gone => sqlx::query("UPDATE push_subscriptions SET active = false, deactivated_at = now(), last_error = 'endpoint gone (404/410) or rejected' WHERE id = $1").bind(t.id).execute(&state.db).await,
            PushOutcome::Failed(m) => sqlx::query("UPDATE push_subscriptions SET last_error = $2 WHERE id = $1").bind(t.id).bind(m.chars().take(200).collect::<String>()).execute(&state.db).await,
        };
        if let Err(e) = r { tracing::warn!(error = %e, "could not record push result"); }
    }
    results
}

/// Active subscriptions of exactly these users.
pub async fn targets_for_users(state: &AppState, users: &[Uuid]) -> Result<Vec<PushTarget>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, user_id, endpoint, p256dh, auth FROM push_subscriptions WHERE active AND user_id = ANY($1) ORDER BY created_at")
        .bind(users).fetch_all(&state.db).await?;
    Ok(rows.into_iter().map(|(id, user_id, endpoint, p256dh, auth)| PushTarget { id, user_id, endpoint, p256dh, auth }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_endpoints() {
        let bad = [
            "", "http://fcm.googleapis.com/x", "ftp://example.com/x", "https://", "https:///x", "//example.com", "example.com/x",
            "https://localhost/x", "https://localhost:443/x", "https://foo.localhost/x", "https://printer.local/x", "https://foo.internal/push",
            "https://metadata.google.internal/", "https://intranet/x", "https://host/x", "https://corp.lan/x", "https://nas.home.arpa/x",
            "https://127.0.0.1/x", "https://127.0.0.1:443/x", "https://10.0.0.5/x", "https://192.168.1.10/x", "https://172.16.0.1/x",
            "https://169.254.169.254/latest/meta-data", "https://100.64.0.1/x", "https://0.0.0.0/x", "https://8.8.8.8/x",
            "https://[::1]/x", "https://[::1]:443/x", "https://[fd00::1]/x", "https://[fe80::1]/x", "https://[::ffff:127.0.0.1]/x", "https://[2001:4860:4860::8888]/x",
            "https://2130706433/x", "https://0x7f000001/x", "https://0x7f.0.0.1/x", "https://017700000001/x", "https://127.1/x", "https://1.2.3.4.5/x",
            "https://user@example.com/x", "https://user:pw@example.com/x", "https://example.com@evil.com/x", "https://evil.com\\@example.com/x",
            "https://example.com:8443/x", "https://example.com:80/x", "https://example.com:/x", "https://example.com:443@127.0.0.1/",
            "https://exa mple.com/x", "https://example.com/\n", "https://exämple.com/x", "https://exa%6Dple.com/x", "https://-bad.example.com/x",
            "https://bad..example.com/x", "https://.example.com/x", "https://fcm.googleapis.com./fcm/send/x", "https://example.com.:443/x", "https://exa_mple.com/x",
        ];
        for u in bad { assert!(validate_endpoint(u).is_err(), "should reject {u:?}"); }
        let long = format!("https://example.com/{}", "a".repeat(600));
        assert!(validate_endpoint(&long).is_err(), "over-long");
    }

    #[test]
    fn accepts_real_push_services() {
        for u in [
            "https://fcm.googleapis.com/fcm/send/abc:DEF-123_x",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAA",
            "https://web.push.apple.com/QGxyz",
            "https://wns2-par02p.notify.windows.com/w/?token=abc",
            "https://HTTPS-Push.Example.com:443/x?y=z#frag",
        ] { assert!(validate_endpoint(u).is_ok(), "should accept {u}: {:?}", validate_endpoint(u)); }
    }

    #[test]
    fn pinned_host_equals_the_host_reqwest_will_connect_to() {
        for u in ["https://fcm.googleapis.com/fcm/send/x", "https://HTTPS-Push.Example.com:443/x?y=z#frag", "https://web.push.apple.com/Q"] {
            assert!(validate_endpoint(u).is_ok());
            let uri: http::Uri = u.parse().unwrap();
            assert_eq!(endpoint_host(u).unwrap(), uri.host().unwrap().to_ascii_lowercase(), "{u}");
        }
    }

    #[test]
    fn public_ip_classifier() {
        let pub_ = ["8.8.8.8", "1.1.1.1", "142.250.80.46", "2001:4860:4860::8888", "2606:4700:4700::1111"];
        let non = [
            "127.0.0.1", "127.255.255.254", "0.0.0.0", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.0.1", "169.254.169.254", "100.64.0.1", "100.127.255.255",
            "224.0.0.1", "255.255.255.255", "240.0.0.1", "198.18.0.1", "192.0.2.1", "203.0.113.9", "0.1.2.3",
            "::1", "::", "fd00::1", "fc00::1", "fe80::1", "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:169.254.169.254", "::127.0.0.1",
            "64:ff9b::7f00:1", "2002:7f00:1::", "2001:db8::1",
        ];
        for ip in pub_ { assert!(is_public_ip(ip.parse().unwrap()), "{ip} should be public"); }
        for ip in non { assert!(!is_public_ip(ip.parse().unwrap()), "{ip} should NOT be public"); }
    }

    #[test]
    fn payload_url_is_same_origin_relative() {
        for evil in ["https://evil.example/x", "//evil.example", "javascript:alert(1)", "\\\\evil", ""] {
            let p: serde_json::Value = serde_json::from_slice(&build_payload("t", "b", evil, "tag", None)).unwrap();
            assert_eq!(p["url"], "/");
        }
        let p: serde_json::Value = serde_json::from_slice(&build_payload("t", "b", "/jars.html#x", "tag", None)).unwrap();
        assert_eq!(p["url"], "/jars.html#x");
    }

    #[test]
    fn request_is_encrypted_for_the_browser_and_signed() {
        let (private, _) = generate_vapid();
        let cfg = crate::config::Config {
            mode: crate::config::Mode::Local, domain: None, bind: "127.0.0.1:1".parse().unwrap(), database_url: String::new(), tls_cert: None, tls_key: None,
            static_dir: "static".into(), data_dir: "data".into(), vapid_private_key: Some(private), vapid_public_key: None,
            vapid_subject: "mailto:ops@example.com".into(), setup_admin: None, insecure_http: true,
        };
        let keys = VapidKeys::from_config(&cfg).unwrap();
        let ua = p256::SecretKey::random(&mut rand::rngs::OsRng);
        let mut auth = [0u8; 16];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut auth);
        let t = PushTarget {
            id: Uuid::nil(), user_id: Uuid::nil(), endpoint: "https://fcm.googleapis.com/fcm/send/x".into(),
            p256dh: URL_SAFE_NO_PAD.encode(ua.public_key().to_encoded_point(false).as_bytes()), auth: URL_SAFE_NO_PAD.encode(auth),
        };
        let req = keys.build_request(&t, b"{\"title\":\"hi\"}").unwrap();
        assert!(req.headers().get("authorization").unwrap().to_str().unwrap().starts_with("vapid "));
        assert_eq!(req.headers().get("content-encoding").unwrap(), "aes128gcm");
        let plain = web_push_native::decrypt(req.into_body(), &ua, &Auth::clone_from_slice(&auth)).unwrap();
        assert_eq!(plain, b"{\"title\":\"hi\"}");
        let bad = PushTarget { p256dh: "AAAA".into(), ..t };
        assert_eq!(keys.build_request(&bad, b"x").unwrap_err(), PushOutcome::Gone);
    }

    #[test]
    fn vapid_generation_roundtrips() {
        let (private, public) = generate_vapid();
        assert_eq!(URL_SAFE_NO_PAD.decode(&private).unwrap().len(), 32);
        assert_eq!(URL_SAFE_NO_PAD.decode(&public).unwrap().len(), 65);
        let mut cfg = crate::config::Config {
            mode: crate::config::Mode::Local, domain: None, bind: "127.0.0.1:1".parse().unwrap(), database_url: String::new(), tls_cert: None, tls_key: None,
            static_dir: "static".into(), data_dir: "data".into(), vapid_private_key: Some(private), vapid_public_key: None,
            vapid_subject: "https://localhost".into(), setup_admin: None, insecure_http: true,
        };
        let k = VapidKeys::from_config(&cfg).expect("keys load");
        assert_eq!(k.public_b64, public);
        cfg.vapid_private_key = Some("garbage".into());
        assert!(VapidKeys::from_config(&cfg).is_none());
        cfg.vapid_private_key = None;
        assert!(VapidKeys::from_config(&cfg).is_none());
    }
}
