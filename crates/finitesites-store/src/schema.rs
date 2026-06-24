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

CREATE TABLE IF NOT EXISTS principals (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('native', 'external')),
  email TEXT,
  pubkey TEXT CHECK (pubkey IS NULL OR length(pubkey) = 64),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK (
    (kind = 'native' AND pubkey IS NOT NULL AND email IS NULL) OR
    (kind = 'external' AND email IS NOT NULL AND pubkey IS NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS principals_email_unique
  ON principals(email) WHERE email IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS principals_pubkey_unique
  ON principals(pubkey) WHERE pubkey IS NOT NULL;

CREATE TABLE IF NOT EXISTS agent_keys (
  id TEXT PRIMARY KEY,
  principal_id TEXT NOT NULL REFERENCES principals(id),
  pubkey TEXT NOT NULL CHECK (length(pubkey) = 64),
  label TEXT,
  verified_at INTEGER NOT NULL,
  revoked_at INTEGER,
  created_at INTEGER NOT NULL,
  CHECK (revoked_at IS NULL OR revoked_at >= verified_at),
  UNIQUE (principal_id, pubkey)
);

CREATE INDEX IF NOT EXISTS agent_keys_active
  ON agent_keys(principal_id, pubkey) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS sites (
  id TEXT PRIMARY KEY,
  owner_pubkey TEXT NOT NULL CHECK (length(owner_pubkey) = 64),
  status TEXT NOT NULL CHECK (status IN ('claimed_unpublished', 'published', 'disabled', 'deleted')),
  visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared', 'public')),
  kind TEXT NOT NULL DEFAULT 'static' CHECK (kind IN ('static', 'app')),
  app_port INTEGER UNIQUE CHECK (app_port IS NULL OR (app_port >= 21000 AND app_port <= 29999)),
  active_version_id TEXT REFERENCES versions(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS sites_owner ON sites(owner_pubkey, created_at);

CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,
  owner_principal_id TEXT NOT NULL REFERENCES principals(id),
  visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared', 'public')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS projects_owner
  ON projects(owner_principal_id, created_at);

CREATE TABLE IF NOT EXISTS project_collaborators (
  project_id TEXT NOT NULL REFERENCES projects(id),
  principal_id TEXT NOT NULL REFERENCES principals(id),
  role TEXT NOT NULL CHECK (role IN ('owner', 'editor', 'viewer')),
  added_by_principal_id TEXT REFERENCES principals(id),
  added_at INTEGER NOT NULL,
  removed_at INTEGER,
  PRIMARY KEY (project_id, principal_id),
  CHECK (removed_at IS NULL OR removed_at >= added_at)
);

CREATE INDEX IF NOT EXISTS project_collaborators_active
  ON project_collaborators(project_id, principal_id) WHERE removed_at IS NULL;

CREATE TABLE IF NOT EXISTS project_outputs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  output_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('site')),
  site_id TEXT NOT NULL REFERENCES sites(id),
  site_name TEXT NOT NULL,
  branch TEXT NOT NULL,
  output_path TEXT NOT NULL,
  spa_fallback INTEGER NOT NULL DEFAULT 0 CHECK (spa_fallback IN (0, 1)),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (project_id, output_id),
  UNIQUE (site_id)
);

CREATE INDEX IF NOT EXISTS project_outputs_project
  ON project_outputs(project_id, output_id);

CREATE TABLE IF NOT EXISTS git_credentials (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  principal_id TEXT NOT NULL REFERENCES principals(id),
  token_hash TEXT NOT NULL UNIQUE CHECK (length(token_hash) = 64),
  created_at INTEGER NOT NULL,
  expires_at INTEGER,
  revoked_at INTEGER,
  last_used_at INTEGER,
  CHECK (expires_at IS NULL OR expires_at > created_at),
  CHECK (revoked_at IS NULL OR revoked_at >= created_at)
);

CREATE INDEX IF NOT EXISTS git_credentials_project_principal
  ON git_credentials(project_id, principal_id) WHERE revoked_at IS NULL;

CREATE TABLE IF NOT EXISTS git_ref_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id),
  ref_name TEXT NOT NULL,
  old_sha TEXT NOT NULL CHECK (length(old_sha) = 40),
  new_sha TEXT NOT NULL CHECK (length(new_sha) = 40),
  actor_principal_id TEXT NOT NULL REFERENCES principals(id),
  actor_agent_key_id TEXT REFERENCES agent_keys(id),
  git_credential_id TEXT NOT NULL REFERENCES git_credentials(id),
  project_output_id TEXT REFERENCES project_outputs(id),
  status TEXT NOT NULL CHECK (status IN ('pending', 'deployed', 'ignored', 'failed')),
  version_id TEXT REFERENCES versions(id),
  error TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (project_id, ref_name, old_sha, new_sha)
);

CREATE INDEX IF NOT EXISTS git_ref_events_pending
  ON git_ref_events(status, id) WHERE status = 'pending';

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
  git_ref_event_id INTEGER,
  created_at INTEGER NOT NULL,
  UNIQUE (site_id, version_number)
);

CREATE UNIQUE INDEX IF NOT EXISTS versions_git_ref_event
  ON versions(git_ref_event_id) WHERE git_ref_event_id IS NOT NULL;

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

CREATE TABLE IF NOT EXISTS blobs (
  sha256 TEXT PRIMARY KEY CHECK (length(sha256) = 64),
  size INTEGER NOT NULL CHECK (size >= 0),
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS shares (
  site_id TEXT NOT NULL REFERENCES sites(id),
  principal_id TEXT NOT NULL REFERENCES principals(id),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (site_id, principal_id)
);

CREATE TABLE IF NOT EXISTS native_viewer_nonces (
  site_id TEXT NOT NULL REFERENCES sites(id),
  pubkey TEXT NOT NULL CHECK (length(pubkey) = 64),
  nonce TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  PRIMARY KEY (site_id, pubkey, nonce),
  CHECK (expires_at > created_at)
);

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
