//! Explicit limits for every bounded loop, payload, and fanout in the system.
//!
//! Limits live in one place so reviews can see the whole bounded surface.
//! Each limit notes why it has its value.

/// One manifest may not list more than this many files. Generous for static
/// sites (here.now caps similar flows around the low thousands) while keeping
/// publish sessions and missing-blob scans visibly bounded.
pub const MAX_MANIFEST_FILES: u32 = 2_000;

/// One file may not exceed 25 MiB. Matches the Workers static asset ceiling,
/// which is a reasonable proxy for "static site asset" vs "video hosting".
pub const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;

/// One site version may not exceed 512 MiB total.
pub const MAX_SITE_BYTES: u64 = 512 * 1024 * 1024;

/// Manifest paths are bounded so the registry never stores unbounded strings.
pub const MAX_PATH_BYTES: u32 = 512;

/// One owner pubkey may claim at most this many sites. Publishing-granted
/// users get "unlimited within reason"; this is the reason.
pub const MAX_SITES_PER_OWNER: u32 = 100;

/// One site may be shared with at most this many emails. Sharing is
/// Google-Doc-shaped (a few collaborators), not a mailing list.
pub const MAX_SHARES_PER_SITE: u32 = 50;

/// One site may have this many email-keyed editors. This matches the
/// document-collaboration shape and keeps per-publish editor checks cheap.
pub const MAX_EDITORS_PER_SITE: u32 = 50;

/// One verified email may keep a handful of active local keys, enough for a
/// laptop plus agent boxes without becoming an unbounded credential list.
pub const MAX_EMAIL_KEYS_PER_EMAIL: u32 = 5;

/// Emails are bounded at the wire boundary before validation.
pub const MAX_EMAIL_BYTES: u32 = 254;

/// NIP-98 events older or newer than this many seconds are rejected.
/// 60s is the spec-suggested window.
pub const NIP98_MAX_SKEW_SECONDS: u64 = 60;

/// Magic-link tokens expire after 15 minutes: long enough for slow email
/// delivery, short enough that a leaked link goes stale quickly.
pub const LOGIN_TOKEN_TTL_SECONDS: u64 = 15 * 60;

/// Viewer cookies last 7 days, then the viewer re-authenticates.
pub const VIEWER_COOKIE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// JSON API request bodies are small control-plane messages. The largest is a
/// full manifest: 2k files * ~600 bytes/entry stays under this with slack.
pub const MAX_API_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// Sharing mutations may add or remove at most this many emails per request.
pub const MAX_EMAILS_PER_SHARING_REQUEST: u32 = 20;

/// A claim or auth header is rejected above this size before any parsing.
pub const MAX_AUTH_HEADER_BYTES: u32 = 8 * 1024;

/// App bundles (tier 2) ship as one tar.gz blob, so they get their own
/// ceiling instead of MAX_FILE_BYTES. 256 MiB fits a Next.js standalone
/// output with room to spare.
pub const MAX_APP_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

/// Source snapshots are optional editor-handoff archives, not dependency
/// mirrors. Excluding vendor/build output keeps this comfortably under app
/// bundle size while allowing real project source.
pub const MAX_SOURCE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

/// Source snapshots may include larger project trees than static manifests,
/// but remain bounded so archive creation and extraction are reviewable.
pub const MAX_SOURCE_SNAPSHOT_FILES: u32 = 5_000;

/// Empty/generated directory trees should not make source walking unbounded.
pub const MAX_SOURCE_SNAPSHOT_DIRECTORIES: u32 = 5_000;

/// Start commands are one shell line, not scripts.
pub const MAX_START_COMMAND_BYTES: u32 = 1024;

/// App listen ports are allocated from this range, one per app site.
pub const APP_PORT_RANGE_START: u16 = 21000;
pub const APP_PORT_RANGE_END: u16 = 29999;
