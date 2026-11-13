# Notes – Subagent C2 (jars / maintenance / notifications / Web Push)

Deviations from `API_REFERENCE.md`, decisions and hand-offs. Everything here is additive or stricter unless stated otherwise.

## Tier 1 – Notifications & Web Push

* **Stricter SSRF rules than section 13** (`push::validate_endpoint`): *every* IP literal in any notation is refused (not only
  private ranges: `8.8.8.8`, `[2001:...]`, `2130706433`, `0x7f.1`, octal), only port 443, ASCII only, no `\ @ %` in the
  authority, host needs >= 2 labels, blocked TLDs `localhost local internal lan home corp intranet localdomain arpa invalid test example onion private`.
  Run at subscribe time and again at send time (a stored endpoint that no longer validates is deactivated).
* **Send-time DNS check** (names like `127.0.0.1.nip.io` pass the syntactic check): the host is resolved by the server, a send is
  refused when ANY resolved address is non-public (`push::is_public_ip`: loopback, private, link-local, CGNAT, TEST-NETs,
  multicast, reserved, IPv6 ULA/link-local/site-local/Teredo/doc, IPv4-mapped/compatible, NAT64 and 6to4 with embedded private v4), and the HTTP client
  is pinned to the checked addresses (`resolve_to_addrs`) so DNS rebinding cannot swap the target.
* **Dedicated push HTTP client** (not `AppState::http`, whose default redirect policy follows redirects): `redirect(none)`,
  connect 5 s / total 10 s, `https_only`, response body never read, and **`no_proxy()`** – an environment `HTTPS_PROXY` would
  bypass the address pin. A plant that can only reach the internet through an HTTP proxy therefore cannot use Web Push
  (documented trade-off; the in-app inbox still works).
* **VAPID**: `VAPID_PRIVATE_KEY` = base64url of the raw 32-byte scalar; the public key is always *derived* from it (the configured
  `VAPID_PUBLIC_KEY` is ignored) so `/push/key` can never advertise a mismatching key. Without a valid private key `enabled=false`.
  `POST /push/test` answers `409 FEATURE_DISABLED` then; it is limited to 5 per user per minute (`429 LOCKED`).
* **Subscribe**: body also tolerates `expirationTime` (what `PushSubscription.toJSON()` emits). `p256dh` must be a valid 65-byte P-256 point,
  `auth` 16 bytes. Re-registering an endpoint that belongs to another user *moves* it to the caller (shared browser/device) – the
  encrypted payload goes to the keys the caller supplied, so the previous owner loses it. Max 10 subscriptions per user (oldest deleted).
  `unsubscribe` only removes the caller's own rows and is always `204` (no probing).
* **Dispatcher** (`workers::push::dispatch_once` / `dispatch_with(state, Option<&dyn PushSender>)`): claim (`FOR UPDATE SKIP LOCKED`),
  recipient selection, `push_deliveries` insert (`ON CONFLICT DO NOTHING`) and `dispatched_at` are one transaction; HTTP sends happen
  after commit => at-most-once. Recipients = `users.is_active` AND subscribed to the event type AND owning >= 1 active subscription.
  Alerts older than 6 h are marked dispatched without a push. Push payload `url` is forced to a same-origin relative path.
* **Inbox** is filtered by the caller's *current* subscriptions in SQL on every call; resolved alerts remain visible for 30 days
  (open alerts always). `POST /inbox/{id}/read` on an alert outside the subscriptions (or unknown) is `404`.
  Response items additionally carry `resolved`. `POST /inbox/read-all` -> `{marked:n}`.
* Shared extractors `routes::notifications::extract::{ApiJson, ApiQuery, is_dry_run, opt_uuid, opt_date, de_int}` return the
  standard error envelope for malformed bodies/queries (axum's defaults are plain text). Used by all C2 routes.
* Observation for the Lead (not changed): users may subscribe to any event type regardless of `allowed_pages`, so a user without the
  `jars` page can subscribe to `JAR_OVERDUE` and read customer names in alert titles. The contract says subscriptions are self-managed;
  if that is a concern, restrict `PUT /subscriptions` by page (map event type -> page) in a later pass.
