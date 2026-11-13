//! Realtime change feed (Server-Sent Events).
//!
//! DB triggers (migrations/0007) `pg_notify('erp_events', <topic>)` after any committed change. `spawn_listener` receives
//! them (one dedicated Postgres connection), coalesces bursts for `DEBOUNCE`, and broadcasts a `ChangeBatch` to every
//! open stream. Events carry **topic names only** – no row data – so clients re-fetch through the normal, authorised
//! REST endpoints. Each stream only receives topics the user may see (`topic_pages`).
//!
//! Browser side: `fetch('/api/events', {headers:{'X-Auth-Key':key}})` and parse the stream (EventSource cannot send
//! headers; keeping the key out of the URL avoids leaking it into logs/history).
//!
//! SSE frames: `event: hello` (once) · `event: change` data `{"id":N,"topics":["stock","qr"],"ts":"…"}` ·
//! `event: resync` (listener reconnected / client lagged → refresh everything) · `event: session_ended` (key revoked or
//! expired; stream closes) · `event: server_restarting` (graceful shutdown; client reconnects with back-off) · `: ping` comments every 20 s.
use crate::{auth::AuthUser, error::AppError, state::AppState};
use axum::{
    extract::{Query, State},
    http::{header, HeaderValue},
    response::{sse::{Event, KeepAlive, Sse}, IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures::stream::{self, Stream};
use serde_json::json;
use serde::Deserialize;
use sqlx::postgres::PgListener;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    convert::Infallible,
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;
use uuid::Uuid;

pub const CHANNEL: &str = "erp_events";
const DEBOUNCE: Duration = Duration::from_millis(150);
const MAX_STREAMS_PER_USER: usize = 8;
const MAX_STREAMS_TOTAL: usize = 500;

/// Which pages may see a topic. `None` = every logged-in user.
pub fn topic_pages(topic: &str) -> Option<&'static [&'static str]> {
    Some(match topic {
        "stock" => &["warehouse", "dashboard", "raw-materials", "production", "loading", "jars", "qr-labels"],
        "materials" => &["raw-materials", "dashboard", "production"],
        "production" => &["production", "dashboard", "qc", "qr-labels", "warehouse"],
        "qc" => &["qc", "production", "qr-labels", "loading", "dashboard"],
        "qr" => &["qr-labels", "loading"],
        "trips" => &["loading", "dashboard"],
        "jars" => &["jars", "dashboard"],
        "maintenance" => &["maintenance", "dashboard"],
        "admin" => &["admin"],
        // alerts (inbox badge), master (reference data), users (permissions changed), settings, resync: everyone
        _ => return None,
    })
}

/// Every topic the system can emit (validated for the optional `topics=` filter).
pub const ALL_TOPICS: &[&str] = &["stock", "materials", "production", "qc", "qr", "trips", "jars", "maintenance", "alerts", "master", "users", "settings", "admin"];

/// Page-specific subscription: the topics each page actually renders. `GET /api/events?page=<key>` narrows the stream to
/// these (still intersected with what the user may see), so a page is never refreshed by an unrelated event.
/// `alerts` (bell badge) and `users` (own permissions changed -> re-fetch /api/me) are on every page.
pub fn page_topics(page: &str) -> Option<&'static [&'static str]> {
    Some(match page {
        "dashboard"     => &["stock", "materials", "production", "qc", "trips", "jars", "maintenance", "alerts", "users"],
        "raw-materials" => &["materials", "stock", "master", "alerts", "users"],
        "production"    => &["production", "stock", "qc", "materials", "master", "alerts", "users"],
        "qc"            => &["qc", "production", "alerts", "users"],
        "warehouse"     => &["stock", "production", "qc", "master", "alerts", "users"],
        "qr-labels"     => &["qr", "qc", "production", "alerts", "users"],
        "loading"       => &["qr", "trips", "qc", "stock", "master", "alerts", "users"],
        "jars"          => &["jars", "stock", "master", "alerts", "users"],
        "maintenance"   => &["maintenance", "alerts", "users"],
        "admin"         => &["admin", "settings", "users", "master", "alerts"],
        "profile"       => &["alerts", "users", "settings"],
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Page key (see `page_topics`). Omitted = every topic the user may see.
    pub page: Option<String>,
    /// Optional comma-separated narrowing of the page's topics (e.g. `topics=qr,trips`).
    pub topics: Option<String>,
}

pub fn user_may_see(u: &AuthUser, topic: &str) -> bool {
    match topic_pages(topic) { None => true, Some(pages) => pages.iter().any(|p| u.can_access(p)) }
}

#[derive(Clone, Debug)]
pub struct ChangeBatch { pub id: u64, pub topics: Vec<String>, pub ts: DateTime<Utc> }

#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<Arc<ChangeBatch>>,
    seq: Arc<AtomicU64>,
    streams: Arc<Mutex<HashMap<Uuid, usize>>>,
    /// How often an open stream re-validates its session (seconds). Tests lower this.
    pub session_check_secs: u64,
}

impl EventHub {
    pub fn new() -> EventHub {
        let (tx, _) = broadcast::channel(256);
        EventHub { tx, seq: Arc::new(AtomicU64::new(1)), streams: Arc::new(Mutex::new(HashMap::new())), session_check_secs: 30 }
    }
    /// Publish topics to all streams (also usable from code that wants an event without a DB change).
    pub fn publish(&self, topics: Vec<String>) {
        if topics.is_empty() { return; }
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(Arc::new(ChangeBatch { id, topics, ts: Utc::now() }));
    }
    pub fn subscribers(&self) -> usize { self.tx.receiver_count() }
    fn acquire(&self, user: Uuid) -> Result<StreamGuard, AppError> {
        let mut m = self.streams.lock().unwrap();
        let total: usize = m.values().sum();
        let mine = m.entry(user).or_insert(0);
        if *mine >= MAX_STREAMS_PER_USER || total >= MAX_STREAMS_TOTAL {
            return Err(AppError::Locked("Too many open live-update connections".into()));
        }
        *mine += 1;
        Ok(StreamGuard { hub: self.clone(), user })
    }
}
impl Default for EventHub { fn default() -> Self { Self::new() } }

struct StreamGuard { hub: EventHub, user: Uuid }
impl Drop for StreamGuard {
    fn drop(&mut self) {
        let mut m = self.hub.streams.lock().unwrap();
        if let Some(n) = m.get_mut(&self.user) { *n = n.saturating_sub(1); if *n == 0 { m.remove(&self.user); } }
    }
}

/// Start the Postgres LISTEN task. Reconnects forever; after any interruption it emits `resync`.
pub fn spawn_listener(state: AppState) {
    tokio::spawn(async move {
        loop {
            match run_listener(&state).await {
                Ok(()) => {}
                Err(e) => tracing::warn!(error = ?e, "event listener interrupted; reconnecting"),
            }
            state.events.publish(vec!["resync".into()]);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn run_listener(state: &AppState) -> Result<(), sqlx::Error> {
    let mut l = PgListener::connect_with(&state.db).await?;
    l.listen(CHANNEL).await?;
    tracing::info!("realtime event listener attached");
    loop {
        let first = l.recv().await?;
        let mut topics: BTreeSet<String> = BTreeSet::new();
        topics.insert(first.payload().to_string());
        // coalesce a burst (a gate pass touches many tables)
        while let Ok(n) = tokio::time::timeout(DEBOUNCE, l.recv()).await {
            topics.insert(n?.payload().to_string());
            if topics.len() > 64 { break; }
        }
        topics.retain(|t| t.len() <= 32 && t.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        state.events.publish(topics.into_iter().collect());
    }
}

struct StreamState {
    rx: broadcast::Receiver<Arc<ChangeBatch>>,
    user: AuthUser,
    wanted: Option<HashSet<String>>,
    state: AppState,
    hello: bool,
    done: bool,
    tick: tokio::time::Interval,
    _guard: StreamGuard,
}

fn change_event(b: &ChangeBatch, topics: &[String]) -> Event {
    Event::default().event("change").id(b.id.to_string())
        .data(json!({ "id": b.id, "topics": topics, "ts": b.ts.to_rfc3339() }).to_string())
}

/// `GET /api/events` – authenticated SSE stream (use fetch + X-Auth-Key).
pub async fn stream_handler(State(state): State<AppState>, user: AuthUser, Query(q): Query<EventsQuery>) -> Result<Response, AppError> {
    let mut wanted: Option<HashSet<String>> = None;
    if let Some(page) = q.page.as_deref() {
        let pt = page_topics(page).ok_or_else(|| AppError::bad("unknown page"))?;
        if page != "profile" && !user.can_access(page) { return Err(AppError::forbidden("You do not have access to this page")); }
        wanted = Some(pt.iter().map(|t| t.to_string()).collect());
    }
    if let Some(list) = q.topics.as_deref() {
        let req: Vec<&str> = list.split(',').map(str::trim).filter(|t| !t.is_empty()).collect();
        if req.len() > 12 || req.iter().any(|t| !ALL_TOPICS.contains(t)) { return Err(AppError::bad("invalid topics filter")); }
        let req: HashSet<String> = req.into_iter().map(String::from).collect();
        wanted = Some(match wanted { Some(w) => w.intersection(&req).cloned().collect(), None => req });
    }
    let guard = state.events.acquire(user.id)?;
    let mut tick = tokio::time::interval(Duration::from_secs(state.events.session_check_secs.max(1)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.reset();
    let st = StreamState { rx: state.events.tx.subscribe(), user, wanted, state: state.clone(), hello: false, done: false, tick, _guard: guard };
    let s: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> = Box::pin(stream::unfold(st, |mut st| async move {
        if st.done { return None; }
        if !st.hello {
            st.hello = true;
            let ev = Event::default().event("hello").data(json!({ "server_time": Utc::now().to_rfc3339(), "user": st.user.username }).to_string());
            return Some((Ok(ev), st));
        }
        loop {
            tokio::select! {
                r = st.rx.recv() => match r {
                    Ok(b) => {
                        let mine: Vec<String> = b.topics.iter().filter(|t| *t == "resync" || (user_may_see(&st.user, t) && st.wanted.as_ref().map_or(true, |w| w.contains(*t)))).cloned().collect();
                        if mine.is_empty() { continue; }
                        if mine.iter().any(|t| t == "resync") { return Some((Ok(Event::default().event("resync").data("{}")), st)); }
                        return Some((Ok(change_event(&b, &mine)), st));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => return Some((Ok(Event::default().event("resync").data("{}")), st)),
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                _ = st.state.shutdown.wait() => {
                    st.done = true;
                    return Some((Ok(Event::default().event("server_restarting").data("{}")), st));
                }
                _ = st.tick.tick() => {
                    // a long-lived stream must not outlive its session or deactivated user
                    let alive: Option<i32> = sqlx::query_scalar(
                        "SELECT 1 FROM user_sessions s JOIN users u ON u.id = s.user_id
                         WHERE s.id = $1 AND s.expires_at > now() AND u.is_active")
                        .bind(st.user.session_id).fetch_optional(&st.state.db).await.ok().flatten();
                    if alive.is_none() {
                        st.done = true;
                        return Some((Ok(Event::default().event("session_ended").data("{}")), st));
                    }
                }
            }
        }
    }));
    let mut resp = Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)).text("ping")).into_response();
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-transform"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(resp)
}
