//! Registry schema. Authoritative state uses schema, constraints, and
//! transactions; the only JSON column is bounded audit metadata.
//!
//! Ported from finite-site's Postgres sketch (docs/schema/) with the nsite
//! event columns dropped and sharing/auth/publish-grant tables added. SQLite is
//! the v1 engine; types stay portable (TEXT ids, INTEGER unix seconds).

pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS allowed_pubkeys (
  pubkey TEXT PRIMARY KEY CHECK (length(pubkey) = 64),
  note TEXT NOT NULL DEFAULT '',
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS publish_grants (
  pubkey TEXT NOT NULL CHECK (length(pubkey) = 64),
  source TEXT NOT NULL CHECK (source IN ('operator', 'core')),
  note TEXT NOT NULL DEFAULT '',
  expires_at INTEGER CHECK (expires_at IS NULL OR expires_at > 0),
  granted_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY (pubkey, source),
  CHECK (revoked_at IS NULL OR revoked_at >= granted_at)
);

CREATE INDEX IF NOT EXISTS publish_grants_active_pubkey
  ON publish_grants(pubkey) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS sites (
  id TEXT PRIMARY KEY,
  owner_pubkey TEXT NOT NULL CHECK (length(owner_pubkey) = 64),
  owner_email TEXT,
  site_pubkey TEXT NOT NULL UNIQUE CHECK (length(site_pubkey) = 64),
  status TEXT NOT NULL CHECK (status IN ('claimed_unpublished', 'published', 'disabled', 'deleted')),
  visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared', 'public')),
  kind TEXT NOT NULL DEFAULT 'static' CHECK (kind IN ('static', 'app')),
  app_port INTEGER UNIQUE CHECK (app_port IS NULL OR (app_port >= 21000 AND app_port <= 29999)),
  active_version_id TEXT REFERENCES versions(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS sites_owner ON sites(owner_pubkey, created_at);

CREATE TABLE IF NOT EXISTS name_claims (
  id TEXT PRIMARY KEY,
  site_id TEXT NOT NULL REFERENCES sites(id),
  name TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active', 'released', 'blocked')),
  released_at INTEGER,
  created_at INTEGER NOT NULL,
  CHECK ((status = 'active' AND released_at IS NULL) OR (status IN ('released', 'blocked')))
);

CREATE UNIQUE INDEX IF NOT EXISTS name_claims_one_active_name
  ON name_claims(name) WHERE status = 'active';

CREATE UNIQUE INDEX IF NOT EXISTS name_claims_one_active_claim_per_site
  ON name_claims(site_id) WHERE status = 'active';

CREATE TABLE IF NOT EXISTS versions (
  id TEXT PRIMARY KEY,
  site_id TEXT NOT NULL REFERENCES sites(id),
  version_number INTEGER NOT NULL CHECK (version_number > 0),
  manifest_sha256 TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
  path_count INTEGER NOT NULL CHECK (path_count > 0),
  total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
  spa_fallback INTEGER NOT NULL DEFAULT 0 CHECK (spa_fallback IN (0, 1)),
  start_command TEXT,
  created_at INTEGER NOT NULL,
  UNIQUE (site_id, version_number)
);

CREATE TABLE IF NOT EXISTS version_files (
  version_id TEXT NOT NULL REFERENCES versions(id),
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0),
  PRIMARY KEY (version_id, path)
);

CREATE TABLE IF NOT EXISTS publishes (
  id TEXT PRIMARY KEY,
  site_id TEXT NOT NULL REFERENCES sites(id),
  status TEXT NOT NULL CHECK (status IN ('pending', 'finalized', 'aborted')),
  version_id TEXT REFERENCES versions(id),
  spa_fallback INTEGER NOT NULL DEFAULT 0 CHECK (spa_fallback IN (0, 1)),
  start_command TEXT,
  actor_pubkey TEXT CHECK (actor_pubkey IS NULL OR length(actor_pubkey) = 64),
  actor_email TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK ((status = 'finalized') = (version_id IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS publishes_site ON publishes(site_id, created_at);

CREATE TABLE IF NOT EXISTS publish_files (
  publish_id TEXT NOT NULL REFERENCES publishes(id),
  path TEXT NOT NULL,
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0),
  PRIMARY KEY (publish_id, path)
);

CREATE TABLE IF NOT EXISTS publish_sources (
  publish_id TEXT PRIMARY KEY REFERENCES publishes(id),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0)
);

CREATE TABLE IF NOT EXISTS version_sources (
  version_id TEXT PRIMARY KEY REFERENCES versions(id),
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0),
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS blobs (
  sha256 TEXT PRIMARY KEY CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0),
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS shares (
  site_id TEXT NOT NULL REFERENCES sites(id),
  email TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (site_id, email)
);

CREATE TABLE IF NOT EXISTS site_editors (
  site_id TEXT NOT NULL REFERENCES sites(id),
  email TEXT NOT NULL,
  added_by_pubkey TEXT NOT NULL CHECK (length(added_by_pubkey) = 64),
  added_at INTEGER NOT NULL,
  removed_at INTEGER,
  PRIMARY KEY (site_id, email),
  CHECK (removed_at IS NULL OR removed_at >= added_at)
);

CREATE INDEX IF NOT EXISTS site_editors_active
  ON site_editors(site_id, email) WHERE removed_at IS NULL;

CREATE TABLE IF NOT EXISTS email_keys (
  email TEXT NOT NULL,
  pubkey TEXT NOT NULL CHECK (length(pubkey) = 64),
  verified_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY (email, pubkey),
  CHECK (revoked_at IS NULL OR revoked_at >= verified_at)
);

CREATE INDEX IF NOT EXISTS email_keys_active
  ON email_keys(email, pubkey) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS email_login_tokens (
  token_hash TEXT PRIMARY KEY CHECK (length(token_hash) = 64),
  email TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS login_tokens (
  token_hash TEXT PRIMARY KEY CHECK (length(token_hash) = 64),
  site_id TEXT NOT NULL REFERENCES sites(id),
  email TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS site_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  site_id TEXT,
  action TEXT NOT NULL,
  actor_pubkey TEXT,
  metadata TEXT NOT NULL DEFAULT '{}',
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS site_events_site ON site_events(site_id, id);
";
