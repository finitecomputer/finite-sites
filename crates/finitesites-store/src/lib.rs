//! SQLite-backed registry store for Finite Sites.
//!
//! The store exposes typed reads plus transactional composites for every
//! mutation that must be atomic (allocating output names, finalizing versions).
//! Database and corruption errors are surfaced as typed errors, never hidden
//! behind `Option`.

mod schema;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use finitesites_proto::{ManifestFile, ids};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("corrupt state: {0}")]
    CorruptState(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteStatus {
    ClaimedUnpublished,
    Published,
    Disabled,
    Deleted,
}

impl SiteStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SiteStatus::ClaimedUnpublished => "claimed_unpublished",
            SiteStatus::Published => "published",
            SiteStatus::Disabled => "disabled",
            SiteStatus::Deleted => "deleted",
        }
    }

    fn from_db(value: &str) -> Result<SiteStatus, StoreError> {
        match value {
            "claimed_unpublished" => Ok(SiteStatus::ClaimedUnpublished),
            "published" => Ok(SiteStatus::Published),
            "disabled" => Ok(SiteStatus::Disabled),
            "deleted" => Ok(SiteStatus::Deleted),
            _ => Err(StoreError::CorruptState("unknown site status in db")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Shared,
    Public,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Shared => "shared",
            Visibility::Public => "public",
        }
    }

    pub fn parse(value: &str) -> Option<Visibility> {
        match value {
            "private" => Some(Visibility::Private),
            "shared" => Some(Visibility::Shared),
            "public" => Some(Visibility::Public),
            _ => None,
        }
    }

    fn from_db(value: &str) -> Result<Visibility, StoreError> {
        Visibility::parse(value).ok_or(StoreError::CorruptState("unknown visibility in db"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    Static,
    App,
}

impl SiteKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SiteKind::Static => "static",
            SiteKind::App => "app",
        }
    }

    fn from_db(value: &str) -> Result<SiteKind, StoreError> {
        match value {
            "static" => Ok(SiteKind::Static),
            "app" => Ok(SiteKind::App),
            _ => Err(StoreError::CorruptState("unknown site kind in db")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SiteRecord {
    pub id: String,
    pub name: String,
    pub owner_pubkey: String,
    pub status: SiteStatus,
    pub visibility: Visibility,
    pub active_version_id: Option<String>,
    pub active_version_number: Option<u32>,
    /// True when the active version was published as a single-page app:
    /// lookup misses serve `/index.html` instead of a 404.
    pub active_version_spa: bool,
    /// Static file site or tier-2 app site. Fixed by the first publish.
    pub kind: SiteKind,
    /// Loopback port assigned to this app site's process, if kind is app.
    pub app_port: Option<u16>,
    /// The active version's start command, if kind is app.
    pub active_version_start: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStatus {
    Pending,
    Finalized,
    Aborted,
}

impl PublishStatus {
    fn from_db(value: &str) -> Result<PublishStatus, StoreError> {
        match value {
            "pending" => Ok(PublishStatus::Pending),
            "finalized" => Ok(PublishStatus::Finalized),
            "aborted" => Ok(PublishStatus::Aborted),
            _ => Err(StoreError::CorruptState("unknown publish status in db")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PublishRecord {
    pub id: String,
    pub site_id: String,
    pub status: PublishStatus,
    pub version_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FinalizedVersion {
    pub version_id: String,
    pub version_number: u32,
    pub path_count: u32,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishGrantSource {
    Operator,
    Core,
}

impl PublishGrantSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublishGrantSource::Operator => "operator",
            PublishGrantSource::Core => "core",
        }
    }

    fn from_db(value: &str) -> Result<PublishGrantSource, StoreError> {
        match value {
            "operator" => Ok(PublishGrantSource::Operator),
            "core" => Ok(PublishGrantSource::Core),
            _ => Err(StoreError::CorruptState(
                "unknown publish grant source in db",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PublishGrant {
    pub pubkey: String,
    pub source: PublishGrantSource,
    pub note: String,
    pub expires_at: Option<u64>,
    pub granted_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    Native,
    External,
}

impl PrincipalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrincipalKind::Native => "native",
            PrincipalKind::External => "external",
        }
    }

    fn from_db(value: &str) -> Result<PrincipalKind, StoreError> {
        match value {
            "native" => Ok(PrincipalKind::Native),
            "external" => Ok(PrincipalKind::External),
            _ => Err(StoreError::CorruptState("unknown principal kind in db")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalRecord {
    pub id: String,
    pub kind: PrincipalKind,
    pub email: Option<String>,
    pub pubkey: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectCollaboratorRole {
    Owner,
    Editor,
    Viewer,
}

impl ProjectCollaboratorRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectCollaboratorRole::Owner => "owner",
            ProjectCollaboratorRole::Editor => "editor",
            ProjectCollaboratorRole::Viewer => "viewer",
        }
    }

    pub fn parse(value: &str) -> Result<ProjectCollaboratorRole, StoreError> {
        match value {
            "owner" => Ok(ProjectCollaboratorRole::Owner),
            "editor" => Ok(ProjectCollaboratorRole::Editor),
            "viewer" => Ok(ProjectCollaboratorRole::Viewer),
            _ => Err(StoreError::Conflict("unknown project collaborator role")),
        }
    }

    fn from_db(value: &str) -> Result<ProjectCollaboratorRole, StoreError> {
        match value {
            "owner" => Ok(ProjectCollaboratorRole::Owner),
            "editor" => Ok(ProjectCollaboratorRole::Editor),
            "viewer" => Ok(ProjectCollaboratorRole::Viewer),
            _ => Err(StoreError::CorruptState(
                "unknown project collaborator role in db",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    pub id: String,
    pub slug: String,
    pub owner_principal_id: String,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAccessRecord {
    pub project: ProjectRecord,
    pub role: ProjectCollaboratorRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOutputRecord {
    pub id: String,
    pub project_id: String,
    pub output_id: String,
    pub kind: String,
    pub site_id: String,
    pub site_name: String,
    pub branch: String,
    pub path: String,
    pub spa: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCollaboratorRecord {
    pub project_id: String,
    pub principal_id: String,
    pub role: ProjectCollaboratorRole,
    pub email: Option<String>,
    pub pubkey: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOutputApply {
    pub output_id: String,
    pub kind: String,
    pub site_name: String,
    pub branch: String,
    pub path: String,
    pub spa: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCollaboratorApply {
    pub email: String,
    pub role: ProjectCollaboratorRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedProjectOutput {
    pub record: ProjectOutputRecord,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedProjectCollaborator {
    pub record: ProjectCollaboratorRecord,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedProjectCollaborator {
    pub email: String,
    pub removed: bool,
    pub revoked_git_credentials: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInitStoreOutcome {
    pub project: ProjectRecord,
    pub created: bool,
    pub outputs: Vec<AppliedProjectOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCredentialRecord {
    pub id: String,
    pub project_id: String,
    pub principal_id: String,
    pub token_hash: String,
    pub expires_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRefEventStatus {
    Pending,
    Deployed,
    Ignored,
    Failed,
}

impl GitRefEventStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            GitRefEventStatus::Pending => "pending",
            GitRefEventStatus::Deployed => "deployed",
            GitRefEventStatus::Ignored => "ignored",
            GitRefEventStatus::Failed => "failed",
        }
    }

    fn from_db(value: &str) -> Result<GitRefEventStatus, StoreError> {
        match value {
            "pending" => Ok(GitRefEventStatus::Pending),
            "deployed" => Ok(GitRefEventStatus::Deployed),
            "ignored" => Ok(GitRefEventStatus::Ignored),
            "failed" => Ok(GitRefEventStatus::Failed),
            _ => Err(StoreError::CorruptState("unknown git ref event status")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefEventRecord {
    pub id: i64,
    pub project_id: String,
    pub ref_name: String,
    pub old_sha: String,
    pub new_sha: String,
    pub actor_principal_id: String,
    pub actor_agent_key_id: Option<String>,
    pub git_credential_id: String,
    pub project_output_id: Option<String>,
    pub status: GitRefEventStatus,
    pub version_id: Option<String>,
    pub error: Option<String>,
}

const SITE_SELECT: &str = "
    SELECT s.id, c.name, s.owner_pubkey, s.status, s.visibility,
           s.active_version_id, v.version_number, COALESCE(v.spa_fallback, 0),
           s.kind, s.app_port, v.start_command
    FROM sites s
    JOIN name_claims c ON c.site_id = s.id AND c.status = 'active'
    LEFT JOIN versions v ON v.id = s.active_version_id
";

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        let conn = Connection::open(path)?;
        Self::initialize(conn)
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Self::initialize(Connection::open_in_memory()?)
    }

    fn initialize(conn: Connection) -> Result<Store, StoreError> {
        // WAL lets the operator `finitesitesd allow` while the server runs.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(schema::SCHEMA_SQL)?;
        // Migrations for databases created before a column existed. The
        // schema uses IF NOT EXISTS, so new columns must be added here too.
        Self::ensure_column(
            &conn,
            "versions",
            "spa_fallback",
            "spa_fallback INTEGER NOT NULL DEFAULT 0 CHECK (spa_fallback IN (0, 1))",
        )?;
        Self::ensure_column(
            &conn,
            "publishes",
            "spa_fallback",
            "spa_fallback INTEGER NOT NULL DEFAULT 0 CHECK (spa_fallback IN (0, 1))",
        )?;
        Self::ensure_column(
            &conn,
            "sites",
            "kind",
            "kind TEXT NOT NULL DEFAULT 'static' CHECK (kind IN ('static', 'app'))",
        )?;
        Self::ensure_column(
            &conn,
            "sites",
            "app_port",
            "app_port INTEGER CHECK (app_port IS NULL OR (app_port >= 21000 AND app_port <= 29999))",
        )?;
        Self::ensure_column(&conn, "versions", "start_command", "start_command TEXT")?;
        Self::ensure_column(
            &conn,
            "versions",
            "git_ref_event_id",
            "git_ref_event_id INTEGER",
        )?;
        Self::ensure_column(&conn, "publishes", "start_command", "start_command TEXT")?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS versions_git_ref_event
             ON versions(git_ref_event_id) WHERE git_ref_event_id IS NOT NULL",
            [],
        )?;
        Self::migrate_legacy_sites_shape(&conn)?;
        Self::migrate_legacy_allowed_pubkeys(&conn)?;
        Ok(Store { conn })
    }

    /// Add a column to an existing table when it is missing. Probing with a
    /// zero-row select is cheap and avoids parsing pragma output.
    fn ensure_column(
        conn: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), StoreError> {
        let probe = format!("SELECT {column} FROM {table} LIMIT 0");
        if conn.prepare(&probe).is_ok() {
            return Ok(());
        }
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition}"))?;
        // Paired check: the column must exist after the migration.
        conn.prepare(&probe)?;
        Ok(())
    }

    fn table_column_names(conn: &Connection, table: &str) -> Result<Vec<String>, StoreError> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let mut columns = Vec::new();
        // Bounded by the schema of one local table.
        for row in rows {
            columns.push(row?);
        }
        Ok(columns)
    }

    fn migrate_legacy_sites_shape(conn: &Connection) -> Result<(), StoreError> {
        let columns = Self::table_column_names(conn, "sites")?;
        let has_owner_email = columns.iter().any(|column| column == "owner_email");
        let has_site_pubkey = columns.iter().any(|column| column == "site_pubkey");
        if !has_owner_email && !has_site_pubkey {
            return Ok(());
        }
        let has_owner_pubkey = columns.iter().any(|column| column == "owner_pubkey");
        let owner_expr = if has_owner_pubkey {
            "owner_pubkey"
        } else if has_site_pubkey {
            "site_pubkey"
        } else {
            return Err(StoreError::CorruptState(
                "legacy sites owner column missing",
            ));
        };
        let kind_expr = if columns.iter().any(|column| column == "kind") {
            "kind"
        } else {
            "'static'"
        };
        let app_port_expr = if columns.iter().any(|column| column == "app_port") {
            "app_port"
        } else {
            "NULL"
        };

        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let result: Result<(), StoreError> = (|| {
            conn.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE sites_new (
                   id TEXT PRIMARY KEY,
                   owner_pubkey TEXT NOT NULL CHECK (length(owner_pubkey) = 64),
                   status TEXT NOT NULL CHECK (status IN ('claimed_unpublished', 'published', 'disabled', 'deleted')),
                   visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared', 'public')),
                   kind TEXT NOT NULL DEFAULT 'static' CHECK (kind IN ('static', 'app')),
                   app_port INTEGER UNIQUE CHECK (app_port IS NULL OR (app_port >= 21000 AND app_port <= 29999)),
                   active_version_id TEXT REFERENCES versions(id),
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );",
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO sites_new
                        (id, owner_pubkey, status, visibility, kind, app_port, active_version_id, created_at, updated_at)
                     SELECT id, {owner_expr}, status, visibility, {kind_expr}, {app_port_expr},
                            active_version_id, created_at, updated_at
                     FROM sites"
                ),
                [],
            )?;
            conn.execute_batch(
                "DROP TABLE sites;
                 ALTER TABLE sites_new RENAME TO sites;
                 CREATE INDEX IF NOT EXISTS sites_owner ON sites(owner_pubkey, created_at);
                 COMMIT;",
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        result?;
        let violations: i64 =
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if violations > 0 {
            return Err(StoreError::CorruptState(
                "foreign key violation after sites migration",
            ));
        }
        Ok(())
    }

    fn migrate_legacy_allowed_pubkeys(conn: &Connection) -> Result<(), StoreError> {
        conn.execute(
            "INSERT OR IGNORE INTO publish_grants
                (pubkey, source, note, expires_at, granted_at, updated_at, revoked_at)
             SELECT pubkey, 'operator', note, NULL, created_at, created_at, NULL
             FROM allowed_pubkeys",
            [],
        )?;
        Ok(())
    }

    // ---- publishing grants ----------------------------------------------

    pub fn allow_pubkey(&mut self, pubkey: &str, note: &str, now: u64) -> Result<(), StoreError> {
        self.grant_publish_access(pubkey, PublishGrantSource::Operator, note, None, now)
    }

    pub fn disallow_pubkey(&mut self, pubkey: &str) -> Result<bool, StoreError> {
        self.revoke_publish_access(pubkey, PublishGrantSource::Operator, 0)
    }

    pub fn is_pubkey_allowed(&self, pubkey: &str) -> Result<bool, StoreError> {
        self.has_publish_access(pubkey, 0)
    }

    pub fn list_allowed(&self) -> Result<Vec<(String, String)>, StoreError> {
        let grants = self.list_publish_grants(0)?;
        let mut out = Vec::with_capacity(grants.len());
        // Bounded: the publish grant cache is operator/Core curated.
        for grant in grants {
            out.push((grant.pubkey, grant.note));
        }
        Ok(out)
    }

    pub fn grant_publish_access(
        &mut self,
        pubkey: &str,
        source: PublishGrantSource,
        note: &str,
        expires_at: Option<u64>,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(pubkey.len() == 64);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO publish_grants
                (pubkey, source, note, expires_at, granted_at, updated_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, NULL)
             ON CONFLICT(pubkey, source) DO UPDATE SET
                note = ?3,
                expires_at = ?4,
                updated_at = ?5,
                revoked_at = NULL",
            params![pubkey, source.as_str(), note, expires_at, now],
        )?;
        if source == PublishGrantSource::Operator {
            tx.execute(
                "INSERT INTO allowed_pubkeys (pubkey, note, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(pubkey) DO UPDATE SET note = ?2",
                params![pubkey, note, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn revoke_publish_access(
        &mut self,
        pubkey: &str,
        source: PublishGrantSource,
        now: u64,
    ) -> Result<bool, StoreError> {
        assert!(pubkey.len() == 64);
        let tx = self.conn.transaction()?;
        let revoked = tx.execute(
            "UPDATE publish_grants
             SET revoked_at = CASE WHEN ?3 >= granted_at THEN ?3 ELSE granted_at END,
                 updated_at = CASE WHEN ?3 >= granted_at THEN ?3 ELSE granted_at END
             WHERE pubkey = ?1 AND source = ?2 AND revoked_at IS NULL",
            params![pubkey, source.as_str(), now],
        )?;
        let legacy_removed = if source == PublishGrantSource::Operator {
            tx.execute(
                "DELETE FROM allowed_pubkeys WHERE pubkey = ?1",
                params![pubkey],
            )?
        } else {
            0
        };
        tx.commit()?;
        Ok(revoked > 0 || legacy_removed > 0)
    }

    pub fn has_publish_access(&self, pubkey: &str, now: u64) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1
                 FROM publish_grants
                 WHERE pubkey = ?1
                   AND revoked_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?2)
                 LIMIT 1",
                params![pubkey, now],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    pub fn list_publish_grants(&self, now: u64) -> Result<Vec<PublishGrant>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT pubkey, source, note, expires_at, granted_at, updated_at
             FROM publish_grants
             WHERE revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
             ORDER BY granted_at, pubkey, source",
        )?;
        let rows = stmt.query_map(params![now], |row| {
            let source_raw: String = row.get(1)?;
            let expires_at_raw: Option<i64> = row.get(3)?;
            let granted_at_raw: i64 = row.get(4)?;
            let updated_at_raw: i64 = row.get(5)?;
            let grant = PublishGrant {
                pubkey: row.get(0)?,
                source: PublishGrantSource::from_db(&source_raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                note: row.get(2)?,
                expires_at: expires_at_raw.map(|value| value as u64),
                granted_at: granted_at_raw as u64,
                updated_at: updated_at_raw as u64,
            };
            Ok(grant)
        })?;
        let mut out = Vec::new();
        // Bounded: the publish grant cache is operator/Core curated.
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    // ---- principals and projects -----------------------------------------

    pub fn principal_by_email(&self, email: &str) -> Result<Option<PrincipalRecord>, StoreError> {
        self.principal_query("WHERE email = ?1", email)
    }

    pub fn principal_by_pubkey(&self, pubkey: &str) -> Result<Option<PrincipalRecord>, StoreError> {
        self.principal_query("WHERE pubkey = ?1", pubkey)
    }

    fn principal_query(
        &self,
        where_clause: &str,
        value: &str,
    ) -> Result<Option<PrincipalRecord>, StoreError> {
        let sql = format!("SELECT id, kind, email, pubkey FROM principals {where_clause}");
        let row = self
            .conn
            .query_row(&sql, params![value], Self::row_to_principal)
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    fn row_to_principal(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<PrincipalRecord, StoreError>> {
        let kind_raw: String = row.get(1)?;
        Ok(Ok(PrincipalRecord {
            id: row.get(0)?,
            kind: PrincipalKind::from_db(&kind_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            email: row.get(2)?,
            pubkey: row.get(3)?,
        }))
    }

    pub fn project_by_slug(&self, slug: &str) -> Result<Option<ProjectRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, slug, owner_principal_id, visibility
                 FROM projects WHERE slug = ?1",
                params![slug],
                Self::row_to_project,
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn project_by_id(&self, project_id: &str) -> Result<Option<ProjectRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, slug, owner_principal_id, visibility
                 FROM projects WHERE id = ?1",
                params![project_id],
                Self::row_to_project,
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn project_access_by_slug(
        &self,
        principal_id: &str,
        slug: &str,
    ) -> Result<Option<ProjectAccessRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT p.id, p.slug, p.owner_principal_id, p.visibility, pc.role
                 FROM projects p
                 JOIN project_collaborators pc ON pc.project_id = p.id
                 WHERE p.slug = ?1
                   AND pc.principal_id = ?2
                   AND pc.removed_at IS NULL",
                params![slug, principal_id],
                Self::row_to_project_access,
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn projects_for_principal(
        &self,
        principal_id: &str,
    ) -> Result<Vec<ProjectAccessRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.slug, p.owner_principal_id, p.visibility, pc.role
             FROM projects p
             JOIN project_collaborators pc ON pc.project_id = p.id
             WHERE pc.principal_id = ?1
               AND pc.removed_at IS NULL
             ORDER BY p.created_at, p.slug",
        )?;
        let rows = stmt.query_map(params![principal_id], Self::row_to_project_access)?;
        let mut out = Vec::new();
        // Bounded by Project Output publishing limits and collaborator grants.
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn project_outputs(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectOutputRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, output_id, kind, site_id, site_name, branch, output_path, spa_fallback
             FROM project_outputs
             WHERE project_id = ?1
             ORDER BY output_id",
        )?;
        let rows = stmt.query_map(params![project_id], Self::row_to_project_output)?;
        let mut out = Vec::new();
        // Bounded by MAX_PROJECT_OUTPUTS, enforced by Project Config.
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn project_output_by_site_id(
        &self,
        site_id: &str,
    ) -> Result<Option<(ProjectRecord, ProjectOutputRecord)>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT p.id, p.slug, p.owner_principal_id, p.visibility,
                        po.id, po.project_id, po.output_id, po.kind, po.site_id,
                        po.site_name, po.branch, po.output_path, po.spa_fallback
                 FROM project_outputs po
                 JOIN projects p ON p.id = po.project_id
                 WHERE po.site_id = ?1",
                params![site_id],
                |row| {
                    let project_visibility_raw: String = row.get(3)?;
                    let spa_raw: i64 = row.get(12)?;
                    Ok::<Result<(ProjectRecord, ProjectOutputRecord), StoreError>, rusqlite::Error>(
                        Ok((
                            ProjectRecord {
                                id: row.get(0)?,
                                slug: row.get(1)?,
                                owner_principal_id: row.get(2)?,
                                visibility: Visibility::from_db(&project_visibility_raw).map_err(
                                    |error| {
                                        rusqlite::Error::FromSqlConversionFailure(
                                            3,
                                            rusqlite::types::Type::Text,
                                            Box::new(error),
                                        )
                                    },
                                )?,
                            },
                            ProjectOutputRecord {
                                id: row.get(4)?,
                                project_id: row.get(5)?,
                                output_id: row.get(6)?,
                                kind: row.get(7)?,
                                site_id: row.get(8)?,
                                site_name: row.get(9)?,
                                branch: row.get(10)?,
                                path: row.get(11)?,
                                spa: spa_raw != 0,
                            },
                        )),
                    )
                },
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn active_project_collaborator_by_email(
        &self,
        project_id: &str,
        email: &str,
    ) -> Result<Option<ProjectCollaboratorRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT pc.project_id, pc.principal_id, pc.role, p.email, p.pubkey
                 FROM project_collaborators pc
                 JOIN principals p ON p.id = pc.principal_id
                 WHERE pc.project_id = ?1
                   AND p.email = ?2
                   AND pc.removed_at IS NULL",
                params![project_id, email],
                Self::row_to_project_collaborator,
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn active_project_collaborator_by_principal(
        &self,
        project_id: &str,
        principal_id: &str,
    ) -> Result<Option<ProjectCollaboratorRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT pc.project_id, pc.principal_id, pc.role, p.email, p.pubkey
                 FROM project_collaborators pc
                 JOIN principals p ON p.id = pc.principal_id
                 WHERE pc.project_id = ?1
                   AND pc.principal_id = ?2
                   AND pc.removed_at IS NULL",
                params![project_id, principal_id],
                Self::row_to_project_collaborator,
            )
            .optional()?;
        match row {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    pub fn active_project_collaborators(
        &self,
        project_id: &str,
    ) -> Result<Vec<ProjectCollaboratorRecord>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT pc.project_id, pc.principal_id, pc.role, p.email, p.pubkey
             FROM project_collaborators pc
             JOIN principals p ON p.id = pc.principal_id
             WHERE pc.project_id = ?1
               AND pc.removed_at IS NULL
             ORDER BY pc.role, p.email, p.pubkey",
        )?;
        let rows = stmt.query_map(params![project_id], Self::row_to_project_collaborator)?;
        let mut out = Vec::new();
        // Bounded by the project collaborator cap enforced by the engine.
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    #[allow(clippy::too_many_lines)]
    pub fn init_project(
        &mut self,
        owner_pubkey: &str,
        slug: &str,
        outputs: &[ProjectOutputApply],
        now: u64,
    ) -> Result<ProjectInitStoreOutcome, StoreError> {
        assert!(owner_pubkey.len() == 64);
        assert!(!slug.is_empty());
        let tx = self.conn.transaction()?;

        let owner_principal_id = ensure_native_principal(
            &tx,
            owner_pubkey,
            ids::new_id(ids::PRINCIPAL_ID_PREFIX),
            now,
        )?;

        let existing_project = tx
            .query_row(
                "SELECT id, slug, owner_principal_id, visibility
                 FROM projects WHERE slug = ?1",
                params![slug],
                Self::row_to_project,
            )
            .optional()?;
        let (project, project_created) = match existing_project {
            Some(result) => {
                let project = result?;
                if project.owner_principal_id != owner_principal_id {
                    return Err(StoreError::Conflict("project slug already exists"));
                }
                tx.execute(
                    "UPDATE projects SET updated_at = ?1 WHERE id = ?2",
                    params![now, project.id],
                )?;
                (project, false)
            }
            None => {
                let project_id = ids::new_id(ids::PROJECT_ID_PREFIX);
                tx.execute(
                    "INSERT INTO projects (id, slug, owner_principal_id, visibility, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'private', ?4, ?4)",
                    params![project_id, slug, owner_principal_id, now],
                )?;
                (
                    ProjectRecord {
                        id: project_id,
                        slug: slug.to_string(),
                        owner_principal_id: owner_principal_id.clone(),
                        visibility: Visibility::Private,
                    },
                    true,
                )
            }
        };

        tx.execute(
            "INSERT INTO project_collaborators
                (project_id, principal_id, role, added_by_principal_id, added_at, removed_at)
             VALUES (?1, ?2, 'owner', ?2, ?3, NULL)
             ON CONFLICT(project_id, principal_id) DO UPDATE SET
                role = 'owner',
                removed_at = NULL",
            params![project.id, owner_principal_id, now],
        )?;

        let mut applied_outputs = Vec::with_capacity(outputs.len());
        // Bounded by MAX_PROJECT_OUTPUTS, validated before this call.
        for output in outputs {
            let existing = tx
                .query_row(
                    "SELECT id, project_id, output_id, kind, site_id, site_name, branch, output_path, spa_fallback
                     FROM project_outputs
                     WHERE project_id = ?1 AND output_id = ?2",
                    params![project.id, output.output_id],
                    Self::row_to_project_output,
                )
                .optional()?;
            let (record, created) = match existing {
                Some(result) => {
                    let record = result?;
                    if record.kind != output.kind
                        || record.site_name != output.site_name
                        || record.branch != output.branch
                        || record.path != output.path
                        || record.spa != output.spa
                    {
                        return Err(StoreError::Conflict(
                            "project output config cannot change during init",
                        ));
                    }
                    (record, false)
                }
                None => {
                    let claimed: Option<i64> = tx
                        .query_row(
                            "SELECT 1 FROM name_claims WHERE name = ?1 AND status = 'active'",
                            params![output.site_name],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if claimed.is_some() {
                        return Err(StoreError::Conflict("site name already claimed"));
                    }

                    let site_id = ids::new_id(ids::SITE_ID_PREFIX);
                    let claim_id = ids::new_id(ids::CLAIM_ID_PREFIX);
                    let project_output_id = ids::new_id(ids::PROJECT_OUTPUT_ID_PREFIX);
                    tx.execute(
                        "INSERT INTO sites
                            (id, owner_pubkey, status, visibility, active_version_id, created_at, updated_at)
                         VALUES (?1, ?2, 'claimed_unpublished', 'private', NULL, ?3, ?3)",
                        params![site_id, owner_pubkey, now],
                    )?;
                    tx.execute(
                        "INSERT INTO name_claims (id, site_id, name, status, released_at, created_at)
                         VALUES (?1, ?2, ?3, 'active', NULL, ?4)",
                        params![claim_id, site_id, output.site_name, now],
                    )?;
                    tx.execute(
                        "INSERT INTO project_outputs
                            (id, project_id, output_id, kind, site_id, site_name, branch, output_path, spa_fallback, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                        params![
                            project_output_id,
                            project.id,
                            output.output_id,
                            output.kind,
                            site_id,
                            output.site_name,
                            output.branch,
                            output.path,
                            output.spa,
                            now
                        ],
                    )?;
                    tx.execute(
                        "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
                         VALUES (?1, 'project_output_created', ?2, '{}', ?3)",
                        params![site_id, owner_pubkey, now],
                    )?;
                    (
                        ProjectOutputRecord {
                            id: project_output_id,
                            project_id: project.id.clone(),
                            output_id: output.output_id.clone(),
                            kind: output.kind.clone(),
                            site_id,
                            site_name: output.site_name.clone(),
                            branch: output.branch.clone(),
                            path: output.path.clone(),
                            spa: output.spa,
                        },
                        true,
                    )
                }
            };
            applied_outputs.push(AppliedProjectOutput { record, created });
        }

        let output_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_outputs WHERE project_id = ?1",
            params![project.id],
            |row| row.get(0),
        )?;
        if output_count == 0 {
            return Err(StoreError::CorruptState(
                "project has no outputs after apply",
            ));
        }

        tx.commit()?;
        Ok(ProjectInitStoreOutcome {
            project,
            created: project_created,
            outputs: applied_outputs,
        })
    }

    pub fn add_project_collaborator(
        &mut self,
        project_id: &str,
        owner_principal_id: &str,
        collaborator: &ProjectCollaboratorApply,
        now: u64,
    ) -> Result<AppliedProjectCollaborator, StoreError> {
        assert!(!project_id.is_empty());
        assert!(!owner_principal_id.is_empty());
        assert!(!collaborator.email.is_empty());

        let tx = self.conn.transaction()?;
        let owner_matches: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM projects
                 WHERE id = ?1 AND owner_principal_id = ?2",
                params![project_id, owner_principal_id],
                |row| row.get(0),
            )
            .optional()?;
        if owner_matches.is_none() {
            return Err(StoreError::Conflict("project owner principal mismatch"));
        }
        let active_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM project_collaborators
             WHERE project_id = ?1 AND removed_at IS NULL",
            params![project_id],
            |row| row.get(0),
        )?;
        let principal_id = ensure_external_principal(
            &tx,
            &collaborator.email,
            ids::new_id(ids::PRINCIPAL_ID_PREFIX),
            now,
        )?;
        let existed: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM project_collaborators
                 WHERE project_id = ?1 AND principal_id = ?2 AND removed_at IS NULL",
                params![project_id, principal_id],
                |row| row.get(0),
            )
            .optional()?;
        if existed.is_none()
            && active_count >= finitesites_proto::limits::MAX_PROJECT_COLLABORATORS as i64
        {
            return Err(StoreError::Conflict("too many project collaborators"));
        }
        tx.execute(
            "INSERT INTO project_collaborators
                (project_id, principal_id, role, added_by_principal_id, added_at, removed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)
             ON CONFLICT(project_id, principal_id) DO UPDATE SET
                role = ?3,
                added_by_principal_id = ?4,
                removed_at = NULL",
            params![
                project_id,
                principal_id,
                collaborator.role.as_str(),
                owner_principal_id,
                now
            ],
        )?;
        tx.commit()?;
        Ok(AppliedProjectCollaborator {
            record: ProjectCollaboratorRecord {
                project_id: project_id.to_string(),
                principal_id,
                role: collaborator.role,
                email: Some(collaborator.email.clone()),
                pubkey: None,
            },
            created: existed.is_none(),
        })
    }

    pub fn remove_project_collaborator(
        &mut self,
        project_id: &str,
        owner_principal_id: &str,
        email: &str,
        now: u64,
    ) -> Result<RemovedProjectCollaborator, StoreError> {
        assert!(!project_id.is_empty());
        assert!(!owner_principal_id.is_empty());
        assert!(!email.is_empty());

        let tx = self.conn.transaction()?;
        let owner_matches: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM projects
                 WHERE id = ?1 AND owner_principal_id = ?2",
                params![project_id, owner_principal_id],
                |row| row.get(0),
            )
            .optional()?;
        if owner_matches.is_none() {
            return Err(StoreError::Conflict("project owner principal mismatch"));
        }

        let principal_id: Option<String> = tx
            .query_row(
                "SELECT id FROM principals WHERE email = ?1",
                params![email],
                |row| row.get(0),
            )
            .optional()?;
        let Some(principal_id) = principal_id else {
            tx.commit()?;
            return Ok(RemovedProjectCollaborator {
                email: email.to_string(),
                removed: false,
                revoked_git_credentials: 0,
            });
        };

        let active_role: Option<String> = tx
            .query_row(
                "SELECT role FROM project_collaborators
                 WHERE project_id = ?1
                   AND principal_id = ?2
                   AND removed_at IS NULL",
                params![project_id, principal_id],
                |row| row.get(0),
            )
            .optional()?;
        if active_role.as_deref() == Some("owner") {
            return Err(StoreError::Conflict("project owner cannot be removed"));
        }

        let removed_rows = tx.execute(
            "UPDATE project_collaborators
             SET removed_at = CASE WHEN ?3 >= added_at THEN ?3 ELSE added_at END
             WHERE project_id = ?1
               AND principal_id = ?2
               AND removed_at IS NULL",
            params![project_id, principal_id, now],
        )?;
        let revoked_rows = tx.execute(
            "UPDATE git_credentials
             SET revoked_at = CASE WHEN ?3 >= created_at THEN ?3 ELSE created_at END
             WHERE project_id = ?1
               AND principal_id = ?2
               AND revoked_at IS NULL",
            params![project_id, principal_id, now],
        )?;
        tx.commit()?;
        Ok(RemovedProjectCollaborator {
            email: email.to_string(),
            removed: removed_rows > 0,
            revoked_git_credentials: revoked_rows as u64,
        })
    }

    pub fn create_git_credential(
        &mut self,
        credential_id: &str,
        project_id: &str,
        principal_id: &str,
        token_hash: &str,
        expires_at: Option<u64>,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(token_hash.len() == 64);
        self.conn.execute(
            "INSERT INTO git_credentials
                (id, project_id, principal_id, token_hash, created_at, expires_at, revoked_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
            params![credential_id, project_id, principal_id, token_hash, now, expires_at],
        )?;
        Ok(())
    }

    pub fn git_credential_by_id(
        &self,
        credential_id: &str,
    ) -> Result<Option<GitCredentialRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, project_id, principal_id, token_hash, expires_at, revoked_at
                 FROM git_credentials WHERE id = ?1",
                params![credential_id],
                |row| {
                    Ok(GitCredentialRecord {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        principal_id: row.get(2)?,
                        token_hash: row.get(3)?,
                        expires_at: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
                        revoked_at: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_git_ref_event(
        &mut self,
        project_id: &str,
        ref_name: &str,
        old_sha: &str,
        new_sha: &str,
        actor_principal_id: &str,
        actor_agent_key_id: Option<&str>,
        git_credential_id: &str,
        now: u64,
    ) -> Result<(GitRefEventRecord, bool), StoreError> {
        assert!(old_sha.len() == 40);
        assert!(new_sha.len() == 40);
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO git_ref_events
                (project_id, ref_name, old_sha, new_sha, actor_principal_id,
                 actor_agent_key_id, git_credential_id, project_output_id,
                 status, version_id, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 'pending', NULL, NULL, ?8, ?8)",
            params![
                project_id,
                ref_name,
                old_sha,
                new_sha,
                actor_principal_id,
                actor_agent_key_id,
                git_credential_id,
                now
            ],
        )?;
        let event = tx.query_row(
            "SELECT id, project_id, ref_name, old_sha, new_sha, actor_principal_id,
                    actor_agent_key_id, git_credential_id, project_output_id,
                    status, version_id, error
             FROM git_ref_events
             WHERE project_id = ?1 AND ref_name = ?2 AND old_sha = ?3 AND new_sha = ?4",
            params![project_id, ref_name, old_sha, new_sha],
            Self::row_to_git_ref_event,
        )??;
        tx.commit()?;
        Ok((event, inserted > 0))
    }

    pub fn pending_git_ref_events(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<GitRefEventRecord>, StoreError> {
        let (sql, params_box): (&str, Vec<String>) = match project_id {
            Some(project_id) => (
                "SELECT id, project_id, ref_name, old_sha, new_sha, actor_principal_id,
                        actor_agent_key_id, git_credential_id, project_output_id,
                        status, version_id, error
                 FROM git_ref_events
                 WHERE status = 'pending' AND project_id = ?1
                 ORDER BY id",
                vec![project_id.to_string()],
            ),
            None => (
                "SELECT id, project_id, ref_name, old_sha, new_sha, actor_principal_id,
                        actor_agent_key_id, git_credential_id, project_output_id,
                        status, version_id, error
                 FROM git_ref_events
                 WHERE status = 'pending'
                 ORDER BY id",
                Vec::new(),
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if params_box.is_empty() {
            stmt.query_map([], Self::row_to_git_ref_event)?
        } else {
            stmt.query_map(params![params_box[0]], Self::row_to_git_ref_event)?
        };
        let mut out = Vec::new();
        // Bounded by the number of pending pushed refs in the registry.
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn mark_git_ref_event_deployed(
        &mut self,
        event_id: i64,
        project_output_id: &str,
        version_id: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE git_ref_events
             SET status = 'deployed',
                 project_output_id = ?1,
                 version_id = ?2,
                 error = NULL,
                 updated_at = ?3
             WHERE id = ?4 AND status = 'pending'",
            params![project_output_id, version_id, now, event_id],
        )?;
        if updated == 0 {
            return Err(StoreError::Conflict("git ref event is not pending"));
        }
        Ok(())
    }

    pub fn mark_git_ref_event_ignored(
        &mut self,
        event_id: i64,
        now: u64,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE git_ref_events
             SET status = 'ignored', updated_at = ?1
             WHERE id = ?2 AND status = 'pending'",
            params![now, event_id],
        )?;
        if updated == 0 {
            return Err(StoreError::Conflict("git ref event is not pending"));
        }
        Ok(())
    }

    pub fn mark_git_ref_event_failed(
        &mut self,
        event_id: i64,
        error: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE git_ref_events
             SET status = 'failed', error = ?1, updated_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![error, now, event_id],
        )?;
        if updated == 0 {
            return Err(StoreError::Conflict("git ref event is not pending"));
        }
        Ok(())
    }

    // ---- sites and claims ------------------------------------------------

    pub fn count_sites_by_owner(&self, owner_pubkey: &str) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sites WHERE owner_pubkey = ?1 AND status != 'deleted'",
            params![owner_pubkey],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn site_by_name(&self, name: &str) -> Result<Option<SiteRecord>, StoreError> {
        self.site_query("WHERE c.name = ?1", name)
    }

    pub fn site_by_id(&self, site_id: &str) -> Result<Option<SiteRecord>, StoreError> {
        self.site_query("WHERE s.id = ?1", site_id)
    }

    fn site_query(
        &self,
        where_clause: &str,
        value: &str,
    ) -> Result<Option<SiteRecord>, StoreError> {
        let sql = format!("{SITE_SELECT} {where_clause}");
        let record = self
            .conn
            .query_row(&sql, params![value], Self::row_to_site)
            .optional()?;
        match record {
            Some(result) => Ok(Some(result?)),
            None => Ok(None),
        }
    }

    /// App sites with an active version, for supervisor reconciliation.
    pub fn app_sites(&self) -> Result<Vec<SiteRecord>, StoreError> {
        let sql = format!(
            "{SITE_SELECT} WHERE s.kind = 'app' AND s.active_version_id IS NOT NULL AND s.status = 'published'"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], Self::row_to_site)?;
        let mut out = Vec::new();
        // Bounded by the app port range (one port per app site).
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn sites_by_owner(&self, owner_pubkey: &str) -> Result<Vec<SiteRecord>, StoreError> {
        let sql = format!(
            "{SITE_SELECT} WHERE s.owner_pubkey = ?1 AND s.status != 'deleted' ORDER BY s.created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![owner_pubkey], Self::row_to_site)?;
        let mut out = Vec::new();
        // Bounded by MAX_SITES_PER_OWNER, enforced at claim time.
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    fn row_to_site(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<SiteRecord, StoreError>> {
        let status_raw: String = row.get(3)?;
        let visibility_raw: String = row.get(4)?;
        let version_number: Option<i64> = row.get(6)?;
        let spa_raw: i64 = row.get(7)?;
        let kind_raw: String = row.get(8)?;
        let app_port: Option<i64> = row.get(9)?;
        Ok((|| {
            Ok(SiteRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                owner_pubkey: row.get(2)?,
                status: SiteStatus::from_db(&status_raw)?,
                visibility: Visibility::from_db(&visibility_raw)?,
                active_version_id: row.get(5)?,
                active_version_number: version_number.map(|n| n as u32),
                active_version_spa: spa_raw != 0,
                kind: SiteKind::from_db(&kind_raw)?,
                app_port: app_port.map(|p| p as u16),
                active_version_start: row.get(10)?,
            })
        })())
    }

    /// Atomically create a site and its active name claim. A unique-index
    /// hit on the name surfaces as `Conflict`, so claim races are decided by
    /// the database, not by a check-then-insert.
    pub fn create_site_with_claim(
        &mut self,
        site_id: &str,
        claim_id: &str,
        name: &str,
        owner_pubkey: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(owner_pubkey.len() == 64);
        let tx = self.conn.transaction()?;
        let site_insert = tx.execute(
            "INSERT INTO sites (id, owner_pubkey, status, visibility, active_version_id, created_at, updated_at)
             VALUES (?1, ?2, 'claimed_unpublished', 'private', NULL, ?3, ?3)",
            params![site_id, owner_pubkey, now],
        );
        if let Err(error) = site_insert {
            return Err(map_unique_violation(error, "site already registered"));
        }
        let claim_insert = tx.execute(
            "INSERT INTO name_claims (id, site_id, name, status, released_at, created_at)
             VALUES (?1, ?2, ?3, 'active', NULL, ?4)",
            params![claim_id, site_id, name, now],
        );
        if let Err(error) = claim_insert {
            return Err(map_unique_violation(error, "name already claimed"));
        }
        tx.execute(
            "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
             VALUES (?1, 'claim_succeeded', ?2, '{}', ?3)",
            params![site_id, owner_pubkey, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- publishes ---------------------------------------------------------

    pub fn create_publish(
        &mut self,
        publish_id: &str,
        site_id: &str,
        files: &[ManifestFile],
        spa_fallback: bool,
        start_command: Option<&str>,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(!files.is_empty());
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO publishes (id, site_id, status, version_id, spa_fallback, start_command, created_at, updated_at)
             VALUES (?1, ?2, 'pending', NULL, ?4, ?5, ?3, ?3)",
            params![publish_id, site_id, now, spa_fallback, start_command],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO publish_files (publish_id, path, sha256, size) VALUES (?1, ?2, ?3, ?4)",
            )?;
            // Bounded by MAX_MANIFEST_FILES, validated before this call.
            for file in files {
                stmt.execute(params![publish_id, file.path, file.sha256, file.size])?;
            }
        }
        tx.execute(
            "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
             VALUES (?1, 'publish_started', NULL, '{}', ?2)",
            params![site_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn publish_by_id(&self, publish_id: &str) -> Result<Option<PublishRecord>, StoreError> {
        let record = self
            .conn
            .query_row(
                "SELECT id, site_id, status, version_id
                 FROM publishes WHERE id = ?1",
                params![publish_id],
                |row| {
                    let status_raw: String = row.get(2)?;
                    Ok((
                        PublishRecord {
                            id: row.get(0)?,
                            site_id: row.get(1)?,
                            status: PublishStatus::Pending,
                            version_id: row.get(3)?,
                        },
                        status_raw,
                    ))
                },
            )
            .optional()?;
        match record {
            Some((mut publish, status_raw)) => {
                publish.status = PublishStatus::from_db(&status_raw)?;
                Ok(Some(publish))
            }
            None => Ok(None),
        }
    }

    pub fn publish_files(&self, publish_id: &str) -> Result<Vec<ManifestFile>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT path, sha256, size FROM publish_files WHERE publish_id = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![publish_id], |row| {
            Ok(ManifestFile {
                path: row.get(0)?,
                sha256: row.get(1)?,
                size: row.get::<_, i64>(2)? as u64,
            })
        })?;
        let mut out = Vec::new();
        // Bounded by MAX_MANIFEST_FILES, enforced at publish creation.
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Size of the first publish-file entry with this hash, if the publish
    /// references it. Content-addressed: all entries with one hash share a
    /// size, so "first" is unambiguous.
    pub fn publish_file_by_hash(
        &self,
        publish_id: &str,
        sha256: &str,
    ) -> Result<Option<u64>, StoreError> {
        let size: Option<i64> = self
            .conn
            .query_row(
                "SELECT size FROM publish_files WHERE publish_id = ?1 AND sha256 = ?2 LIMIT 1",
                params![publish_id, sha256],
                |row| row.get(0),
            )
            .optional()?;
        Ok(size.map(|s| s as u64))
    }

    pub fn version_by_id(&self, version_id: &str) -> Result<Option<FinalizedVersion>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT version_number, path_count, total_bytes FROM versions WHERE id = ?1",
                params![version_id],
                |row| {
                    Ok(FinalizedVersion {
                        version_id: String::new(),
                        version_number: row.get::<_, i64>(0)? as u32,
                        path_count: row.get::<_, i64>(1)? as u32,
                        total_bytes: row.get::<_, i64>(2)? as u64,
                    })
                },
            )
            .optional()?;
        let Some(mut version) = row else {
            return Ok(None);
        };
        version.version_id = version_id.to_string();
        Ok(Some(version))
    }

    pub fn version_by_git_ref_event_id(
        &self,
        event_id: i64,
    ) -> Result<Option<FinalizedVersion>, StoreError> {
        let version_id = self
            .conn
            .query_row(
                "SELECT id FROM versions WHERE git_ref_event_id = ?1",
                params![event_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match version_id {
            Some(version_id) => self.version_by_id(&version_id),
            None => Ok(None),
        }
    }

    /// Which of these hashes have no verified blob yet. Input order is
    /// preserved; duplicates collapse to one entry.
    pub fn missing_blobs(&self, hashes: &[&str]) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT 1 FROM blobs WHERE sha256 = ?1")?;
        let mut missing: Vec<String> = Vec::new();
        // Bounded by MAX_MANIFEST_FILES, validated before this call.
        for hash in hashes {
            let exists: Option<i64> = stmt.query_row(params![hash], |row| row.get(0)).optional()?;
            let already_listed = missing.iter().any(|m| m == hash);
            if exists.is_none() && !already_listed {
                missing.push((*hash).to_string());
            }
        }
        Ok(missing)
    }

    pub fn record_blob(&mut self, sha256: &str, size: u64, now: u64) -> Result<(), StoreError> {
        assert!(sha256.len() == 64);
        self.conn.execute(
            "INSERT OR IGNORE INTO blobs (sha256, size, created_at) VALUES (?1, ?2, ?3)",
            params![sha256, size, now],
        )?;
        Ok(())
    }

    /// Finalize a pending publish into an immutable version and flip the
    /// site's active-version pointer. One transaction; verifies every
    /// manifest blob is present inside that transaction.
    pub fn finalize_publish(
        &mut self,
        publish_id: &str,
        version_id: &str,
        manifest_sha256: &str,
        now: u64,
    ) -> Result<FinalizedVersion, StoreError> {
        self.finalize_publish_for_git_event(publish_id, version_id, manifest_sha256, None, now)
    }

    /// Finalize with an optional git ref event id. The unique index on
    /// `versions.git_ref_event_id` makes deploy replay deterministic after a
    /// crash between Version creation and event acknowledgement.
    pub fn finalize_publish_for_git_event(
        &mut self,
        publish_id: &str,
        version_id: &str,
        manifest_sha256: &str,
        git_ref_event_id: Option<i64>,
        now: u64,
    ) -> Result<FinalizedVersion, StoreError> {
        assert!(manifest_sha256.len() == 64);
        let tx = self.conn.transaction()?;

        let (site_id, status_raw): (String, String) = tx
            .query_row(
                "SELECT site_id, status FROM publishes WHERE id = ?1",
                params![publish_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(StoreError::NotFound("publish"))?;
        if PublishStatus::from_db(&status_raw)? != PublishStatus::Pending {
            return Err(StoreError::Conflict("publish is not pending"));
        }

        let missing_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM publish_files pf
             LEFT JOIN blobs b ON b.sha256 = pf.sha256
             WHERE pf.publish_id = ?1 AND b.sha256 IS NULL",
            params![publish_id],
            |row| row.get(0),
        )?;
        if missing_count > 0 {
            return Err(StoreError::Conflict("publish has missing blobs"));
        }
        let (path_count, total_bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM publish_files WHERE publish_id = ?1",
            params![publish_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if path_count == 0 {
            return Err(StoreError::CorruptState("pending publish has no files"));
        }

        let version_number: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version_number), 0) + 1 FROM versions WHERE site_id = ?1",
            params![site_id],
            |row| row.get(0),
        )?;

        tx.execute(
            "INSERT INTO versions (id, site_id, version_number, manifest_sha256, path_count, total_bytes, spa_fallback, start_command, git_ref_event_id, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, p.spa_fallback, p.start_command, ?7, ?8 FROM publishes p WHERE p.id = ?9",
            params![
                version_id,
                site_id,
                version_number,
                manifest_sha256,
                path_count,
                total_bytes,
                git_ref_event_id,
                now,
                publish_id
            ],
        )?;
        // The first publish fixes the site kind; app sites get a loopback
        // port allocated once and keep it across versions.
        let start_command: Option<String> = tx.query_row(
            "SELECT start_command FROM publishes WHERE id = ?1",
            params![publish_id],
            |row| row.get(0),
        )?;
        let kind = if start_command.is_some() {
            "app"
        } else {
            "static"
        };
        tx.execute(
            "UPDATE sites SET kind = ?1 WHERE id = ?2",
            params![kind, site_id],
        )?;
        if start_command.is_some() {
            let has_port: Option<i64> = tx.query_row(
                "SELECT app_port FROM sites WHERE id = ?1",
                params![site_id],
                |row| row.get(0),
            )?;
            if has_port.is_none() {
                let next_port: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(app_port), 20999) + 1 FROM sites",
                    params![],
                    |row| row.get(0),
                )?;
                if next_port > 29999 {
                    return Err(StoreError::Conflict("app port range exhausted"));
                }
                tx.execute(
                    "UPDATE sites SET app_port = ?1 WHERE id = ?2",
                    params![next_port, site_id],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO version_files (version_id, path, sha256, size)
             SELECT ?1, path, sha256, size FROM publish_files WHERE publish_id = ?2",
            params![version_id, publish_id],
        )?;
        tx.execute(
            "UPDATE publishes SET status = 'finalized', version_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![version_id, now, publish_id],
        )?;
        tx.execute(
            "UPDATE sites SET active_version_id = ?1, status = 'published', updated_at = ?2 WHERE id = ?3",
            params![version_id, now, site_id],
        )?;
        let metadata = publish_event_metadata(version_number as u32);
        tx.execute(
            "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
             VALUES (?1, 'publish_succeeded', ?2, ?3, ?4)",
            params![site_id, Option::<String>::None, metadata, now],
        )?;

        // Paired assertion: re-read the committed file rows before trusting
        // the version we just wrote.
        let copied_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM version_files WHERE version_id = ?1",
            params![version_id],
            |row| row.get(0),
        )?;
        if copied_count != path_count {
            return Err(StoreError::CorruptState(
                "version file rows do not match publish file rows",
            ));
        }

        tx.commit()?;
        Ok(FinalizedVersion {
            version_id: version_id.to_string(),
            version_number: version_number as u32,
            path_count: path_count as u32,
            total_bytes: total_bytes as u64,
        })
    }

    pub fn version_file(
        &self,
        version_id: &str,
        path: &str,
    ) -> Result<Option<(String, u64)>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT sha256, size FROM version_files WHERE version_id = ?1 AND path = ?2",
                params![version_id, path],
                |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)),
            )
            .optional()?;
        Ok(row)
    }

    // ---- sharing -----------------------------------------------------------

    pub fn set_visibility(
        &mut self,
        site_id: &str,
        visibility: Visibility,
        now: u64,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE sites SET visibility = ?1, updated_at = ?2 WHERE id = ?3",
            params![visibility.as_str(), now, site_id],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound("site"));
        }
        Ok(())
    }

    pub fn add_share(&mut self, site_id: &str, email: &str, now: u64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO shares (site_id, email, created_at) VALUES (?1, ?2, ?3)",
            params![site_id, email, now],
        )?;
        Ok(())
    }

    pub fn remove_share(&mut self, site_id: &str, email: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM shares WHERE site_id = ?1 AND email = ?2",
            params![site_id, email],
        )?;
        Ok(())
    }

    pub fn shares(&self, site_id: &str) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT email FROM shares WHERE site_id = ?1 ORDER BY email")?;
        let rows = stmt.query_map(params![site_id], |row| row.get(0))?;
        let mut out = Vec::new();
        // Bounded by MAX_SHARES_PER_SITE, enforced at share time.
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn count_shares(&self, site_id: &str) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM shares WHERE site_id = ?1",
            params![site_id],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn is_email_shared(&self, site_id: &str, email: &str) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM shares WHERE site_id = ?1 AND email = ?2",
                params![site_id, email],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    // ---- email keys -------------------------------------------------------

    pub fn create_email_login_token(
        &mut self,
        token_hash: &str,
        email: &str,
        expires_at: u64,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(token_hash.len() == 64);
        assert!(expires_at > now);
        self.conn.execute(
            "INSERT INTO email_login_tokens (token_hash, email, expires_at, used_at, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4)",
            params![token_hash, email, expires_at, now],
        )?;
        Ok(())
    }

    pub fn redeem_email_login_token(
        &mut self,
        token_hash: &str,
        now: u64,
    ) -> Result<String, StoreError> {
        let tx = self.conn.transaction()?;
        let row: Option<(String, u64, Option<u64>)> = tx
            .query_row(
                "SELECT email, expires_at, used_at
                 FROM email_login_tokens
                 WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, Option<i64>>(2)?.map(|v| v as u64),
                    ))
                },
            )
            .optional()?;
        let (email, expires_at, used_at) = row.ok_or(StoreError::NotFound("email login token"))?;
        if used_at.is_some() {
            return Err(StoreError::Conflict("email login token already used"));
        }
        if now > expires_at {
            return Err(StoreError::Conflict("email login token expired"));
        }
        tx.execute(
            "UPDATE email_login_tokens SET used_at = ?1 WHERE token_hash = ?2",
            params![now, token_hash],
        )?;
        tx.commit()?;
        Ok(email)
    }

    pub fn add_email_key(&mut self, email: &str, pubkey: &str, now: u64) -> Result<(), StoreError> {
        assert!(pubkey.len() == 64);
        self.conn.execute(
            "INSERT INTO email_keys (email, pubkey, verified_at, revoked_at)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(email, pubkey) DO UPDATE SET
                verified_at = ?3,
                revoked_at = NULL",
            params![email, pubkey, now],
        )?;
        Ok(())
    }

    pub fn count_email_keys(&self, email: &str) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM email_keys WHERE email = ?1 AND revoked_at IS NULL",
            params![email],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn has_email_key(&self, email: &str, pubkey: &str) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM email_keys
                 WHERE email = ?1 AND pubkey = ?2 AND revoked_at IS NULL",
                params![email, pubkey],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    // ---- magic-link tokens -------------------------------------------------

    pub fn create_login_token(
        &mut self,
        token_hash: &str,
        site_id: &str,
        email: &str,
        expires_at: u64,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(token_hash.len() == 64);
        assert!(expires_at > now);
        self.conn.execute(
            "INSERT INTO login_tokens (token_hash, site_id, email, expires_at, used_at, created_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![token_hash, site_id, email, expires_at, now],
        )?;
        Ok(())
    }

    /// Redeem a token exactly once. Marks it used in the same transaction
    /// that validates it, so a replayed link cannot win a race.
    pub fn redeem_login_token(
        &mut self,
        token_hash: &str,
        now: u64,
    ) -> Result<(String, String), StoreError> {
        let tx = self.conn.transaction()?;
        let row: Option<(String, String, u64, Option<u64>)> = tx
            .query_row(
                "SELECT site_id, email, expires_at, used_at FROM login_tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    ))
                },
            )
            .optional()?;
        let (site_id, email, expires_at, used_at) =
            row.ok_or(StoreError::NotFound("login token"))?;
        if used_at.is_some() {
            return Err(StoreError::Conflict("login token already used"));
        }
        if now > expires_at {
            return Err(StoreError::Conflict("login token expired"));
        }
        tx.execute(
            "UPDATE login_tokens SET used_at = ?1 WHERE token_hash = ?2",
            params![now, token_hash],
        )?;
        tx.commit()?;
        Ok((site_id, email))
    }

    // ---- audit -------------------------------------------------------------

    pub fn record_event(
        &mut self,
        site_id: Option<&str>,
        action: &str,
        actor_pubkey: Option<&str>,
        now: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
             VALUES (?1, ?2, ?3, '{}', ?4)",
            params![site_id, action, actor_pubkey, now],
        )?;
        Ok(())
    }
}

fn map_unique_violation(error: rusqlite::Error, conflict: &'static str) -> StoreError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &error
        && failure.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return StoreError::Conflict(conflict);
    }
    StoreError::Sqlite(error)
}

fn ensure_native_principal(
    tx: &rusqlite::Transaction<'_>,
    pubkey: &str,
    principal_id: String,
    now: u64,
) -> Result<String, StoreError> {
    assert!(pubkey.len() == 64);
    tx.execute(
        "INSERT OR IGNORE INTO principals (id, kind, email, pubkey, created_at, updated_at)
         VALUES (?1, 'native', NULL, ?2, ?3, ?3)",
        params![principal_id, pubkey, now],
    )?;
    let id: String = tx.query_row(
        "SELECT id FROM principals WHERE pubkey = ?1",
        params![pubkey],
        |row| row.get(0),
    )?;
    Ok(id)
}

fn ensure_external_principal(
    tx: &rusqlite::Transaction<'_>,
    email: &str,
    principal_id: String,
    now: u64,
) -> Result<String, StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO principals (id, kind, email, pubkey, created_at, updated_at)
         VALUES (?1, 'external', ?2, NULL, ?3, ?3)",
        params![principal_id, email, now],
    )?;
    let id: String = tx.query_row(
        "SELECT id FROM principals WHERE email = ?1",
        params![email],
        |row| row.get(0),
    )?;
    Ok(id)
}

impl Store {
    fn row_to_project(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<ProjectRecord, StoreError>> {
        let visibility_raw: String = row.get(3)?;
        Ok(Ok(ProjectRecord {
            id: row.get(0)?,
            slug: row.get(1)?,
            owner_principal_id: row.get(2)?,
            visibility: Visibility::from_db(&visibility_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        }))
    }

    fn row_to_project_access(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<ProjectAccessRecord, StoreError>> {
        let visibility_raw: String = row.get(3)?;
        let role_raw: String = row.get(4)?;
        Ok(Ok(ProjectAccessRecord {
            project: ProjectRecord {
                id: row.get(0)?,
                slug: row.get(1)?,
                owner_principal_id: row.get(2)?,
                visibility: Visibility::from_db(&visibility_raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            },
            role: ProjectCollaboratorRole::from_db(&role_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
        }))
    }

    fn row_to_project_output(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<ProjectOutputRecord, StoreError>> {
        let spa_raw: i64 = row.get(8)?;
        Ok(Ok(ProjectOutputRecord {
            id: row.get(0)?,
            project_id: row.get(1)?,
            output_id: row.get(2)?,
            kind: row.get(3)?,
            site_id: row.get(4)?,
            site_name: row.get(5)?,
            branch: row.get(6)?,
            path: row.get(7)?,
            spa: spa_raw != 0,
        }))
    }

    fn row_to_project_collaborator(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<ProjectCollaboratorRecord, StoreError>> {
        let role_raw: String = row.get(2)?;
        Ok(Ok(ProjectCollaboratorRecord {
            project_id: row.get(0)?,
            principal_id: row.get(1)?,
            role: ProjectCollaboratorRole::from_db(&role_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            email: row.get(3)?,
            pubkey: row.get(4)?,
        }))
    }

    fn row_to_git_ref_event(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Result<GitRefEventRecord, StoreError>> {
        let status_raw: String = row.get(9)?;
        Ok(Ok(GitRefEventRecord {
            id: row.get(0)?,
            project_id: row.get(1)?,
            ref_name: row.get(2)?,
            old_sha: row.get(3)?,
            new_sha: row.get(4)?,
            actor_principal_id: row.get(5)?,
            actor_agent_key_id: row.get(6)?,
            git_credential_id: row.get(7)?,
            project_output_id: row.get(8)?,
            status: GitRefEventStatus::from_db(&status_raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?,
            version_id: row.get(10)?,
            error: row.get(11)?,
        }))
    }
}

fn publish_event_metadata(version_number: u32) -> String {
    format!("{{\"version\":{version_number}}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER_KEY: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NOW: u64 = 1_750_000_000;

    fn file(path: &str, sha: &str, size: u64) -> ManifestFile {
        ManifestFile {
            path: path.into(),
            sha256: sha.into(),
            size,
        }
    }

    fn store_with_site(name: &str) -> Store {
        let mut store = Store::open_in_memory().unwrap();
        store
            .create_site_with_claim("site_1", "claim_1", name, OWNER, NOW)
            .unwrap();
        store
    }

    fn project_output(site_name: &str) -> ProjectOutputApply {
        ProjectOutputApply {
            output_id: "mockup".to_string(),
            kind: "site".to_string(),
            site_name: site_name.to_string(),
            branch: "main".to_string(),
            path: ".".to_string(),
            spa: false,
        }
    }

    #[test]
    fn project_init_creates_site_output_and_replays() {
        let mut store = Store::open_in_memory().unwrap();
        let first = store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW,
            )
            .unwrap();
        assert!(first.created);
        assert_eq!(first.project.slug, "finitechat-native");
        assert_eq!(first.outputs.len(), 1);
        assert!(first.outputs[0].created);
        assert_eq!(
            first.outputs[0].record.site_name,
            "finitechat-native-mockup"
        );
        assert!(
            store
                .site_by_name("finitechat-native-mockup")
                .unwrap()
                .is_some()
        );

        let replay = store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW + 1,
            )
            .unwrap();
        assert!(!replay.created);
        assert!(!replay.outputs[0].created);
        assert_eq!(replay.project.id, first.project.id);
        assert_eq!(store.project_outputs(&first.project.id).unwrap().len(), 1);
    }

    #[test]
    fn project_collaborator_grant_replays() {
        let mut store = Store::open_in_memory().unwrap();
        let applied = store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW,
            )
            .unwrap();
        let owner_principal = store.principal_by_pubkey(OWNER).unwrap().unwrap();
        let input = ProjectCollaboratorApply {
            email: "skyler@example.com".to_string(),
            role: ProjectCollaboratorRole::Editor,
        };

        let granted = store
            .add_project_collaborator(&applied.project.id, &owner_principal.id, &input, NOW + 1)
            .unwrap();
        assert!(granted.created);
        assert_eq!(granted.record.email.as_deref(), Some("skyler@example.com"));

        let replay = store
            .add_project_collaborator(&applied.project.id, &owner_principal.id, &input, NOW + 2)
            .unwrap();
        assert!(!replay.created);
        assert_eq!(
            store
                .active_project_collaborators(&applied.project.id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn project_collaborator_removal_revokes_git_credentials_and_replays() {
        let mut store = Store::open_in_memory().unwrap();
        let applied = store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW,
            )
            .unwrap();
        let owner_principal = store.principal_by_pubkey(OWNER).unwrap().unwrap();
        store
            .add_project_collaborator(
                &applied.project.id,
                &owner_principal.id,
                &ProjectCollaboratorApply {
                    email: "skyler@example.com".to_string(),
                    role: ProjectCollaboratorRole::Editor,
                },
                NOW + 1,
            )
            .unwrap();
        let collaborator = store
            .active_project_collaborator_by_email(&applied.project.id, "skyler@example.com")
            .unwrap()
            .unwrap();
        store
            .create_git_credential(
                "gcred_1",
                &applied.project.id,
                &collaborator.principal_id,
                SHA_A,
                None,
                NOW + 1,
            )
            .unwrap();
        store
            .create_git_credential(
                "gcred_2",
                &applied.project.id,
                &collaborator.principal_id,
                SHA_B,
                None,
                NOW + 2,
            )
            .unwrap();

        let removed = store
            .remove_project_collaborator(
                &applied.project.id,
                &owner_principal.id,
                "skyler@example.com",
                NOW + 3,
            )
            .unwrap();
        assert!(removed.removed);
        assert_eq!(removed.revoked_git_credentials, 2);
        assert!(
            store
                .active_project_collaborator_by_email(&applied.project.id, "skyler@example.com")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .git_credential_by_id("gcred_1")
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );
        assert!(
            store
                .git_credential_by_id("gcred_2")
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );

        let replay = store
            .remove_project_collaborator(
                &applied.project.id,
                &owner_principal.id,
                "skyler@example.com",
                NOW + 4,
            )
            .unwrap();
        assert!(!replay.removed);
        assert_eq!(replay.revoked_git_credentials, 0);

        let unknown = store
            .remove_project_collaborator(
                &applied.project.id,
                &owner_principal.id,
                "unknown@example.com",
                NOW + 5,
            )
            .unwrap();
        assert!(!unknown.removed);
        assert_eq!(unknown.revoked_git_credentials, 0);
    }

    #[test]
    fn project_init_rejects_conflicting_slug_and_site_name() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW,
            )
            .unwrap();

        let slug_conflict = store.init_project(
            OTHER_KEY,
            "finitechat-native",
            &[project_output("other-mockup")],
            NOW + 1,
        );
        assert!(matches!(
            slug_conflict,
            Err(StoreError::Conflict("project slug already exists"))
        ));

        let site_conflict = store.init_project(
            OWNER,
            "another-project",
            &[project_output("finitechat-native-mockup")],
            NOW + 2,
        );
        assert!(matches!(
            site_conflict,
            Err(StoreError::Conflict("site name already claimed"))
        ));
    }

    #[test]
    fn git_ref_events_are_idempotent_per_ref_transition() {
        let mut store = Store::open_in_memory().unwrap();
        let applied = store
            .init_project(
                OWNER,
                "finitechat-native",
                &[project_output("finitechat-native-mockup")],
                NOW,
            )
            .unwrap();
        let credential_id = "gcred_11111111111111111111111111111111";
        let token_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        store
            .create_git_credential(
                credential_id,
                &applied.project.id,
                &applied.project.owner_principal_id,
                token_hash,
                None,
                NOW + 1,
            )
            .unwrap();

        let old_sha = "0000000000000000000000000000000000000000";
        let new_sha = "1111111111111111111111111111111111111111";
        let (event, inserted) = store
            .record_git_ref_event(
                &applied.project.id,
                "refs/heads/main",
                old_sha,
                new_sha,
                &applied.project.owner_principal_id,
                None,
                credential_id,
                NOW + 2,
            )
            .unwrap();
        assert!(inserted);
        assert_eq!(event.status, GitRefEventStatus::Pending);

        let (replay, replay_inserted) = store
            .record_git_ref_event(
                &applied.project.id,
                "refs/heads/main",
                old_sha,
                new_sha,
                &applied.project.owner_principal_id,
                None,
                credential_id,
                NOW + 3,
            )
            .unwrap();
        assert!(!replay_inserted);
        assert_eq!(replay.id, event.id);

        let (next_transition, next_inserted) = store
            .record_git_ref_event(
                &applied.project.id,
                "refs/heads/main",
                "2222222222222222222222222222222222222222",
                new_sha,
                &applied.project.owner_principal_id,
                None,
                credential_id,
                NOW + 4,
            )
            .unwrap();
        assert!(next_inserted);
        assert_ne!(next_transition.id, event.id);
    }

    #[test]
    fn publish_grants_roundtrip_with_source_scoping() {
        let mut store = Store::open_in_memory().unwrap();
        assert!(!store.has_publish_access(OWNER, NOW).unwrap());
        store
            .grant_publish_access(OWNER, PublishGrantSource::Operator, "vip", None, NOW)
            .unwrap();
        assert!(store.has_publish_access(OWNER, NOW).unwrap());
        store
            .grant_publish_access(
                OWNER,
                PublishGrantSource::Operator,
                "vip replay",
                None,
                NOW + 1,
            )
            .unwrap();
        store
            .grant_publish_access(
                OWNER,
                PublishGrantSource::Core,
                "paid",
                Some(NOW + 100),
                NOW + 2,
            )
            .unwrap();
        let grants = store.list_publish_grants(NOW + 3).unwrap();
        assert_eq!(grants.len(), 2);
        assert_eq!(grants[0].note, "vip replay");
        assert_eq!(grants[1].source, PublishGrantSource::Core);
        assert_eq!(grants[1].expires_at, Some(NOW + 100));
        assert!(store.disallow_pubkey(OWNER).unwrap());
        assert!(store.has_publish_access(OWNER, NOW + 4).unwrap());
        assert!(
            !store
                .revoke_publish_access(OWNER, PublishGrantSource::Operator, NOW + 5)
                .unwrap()
        );
        assert!(
            store
                .revoke_publish_access(OWNER, PublishGrantSource::Core, NOW + 6)
                .unwrap()
        );
        assert!(!store.has_publish_access(OWNER, NOW + 7).unwrap());
    }

    #[test]
    fn expired_publish_grant_fails_closed() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .grant_publish_access(
                OWNER,
                PublishGrantSource::Core,
                "expired",
                Some(NOW + 10),
                NOW,
            )
            .unwrap();
        assert!(store.has_publish_access(OWNER, NOW + 9).unwrap());
        assert!(!store.has_publish_access(OWNER, NOW + 10).unwrap());
        assert!(store.list_publish_grants(NOW + 10).unwrap().is_empty());
    }

    #[test]
    fn claim_then_lookup() {
        let store = store_with_site("hello");
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.id, "site_1");
        assert_eq!(site.owner_pubkey, OWNER);
        assert_eq!(site.status, SiteStatus::ClaimedUnpublished);
        assert_eq!(site.visibility, Visibility::Private);
        assert!(site.active_version_id.is_none());
        assert!(store.site_by_name("missing").unwrap().is_none());
        assert_eq!(store.count_sites_by_owner(OWNER).unwrap(), 1);
    }

    #[test]
    fn duplicate_name_claim_conflicts() {
        let mut store = store_with_site("hello");
        let result = store.create_site_with_claim("site_2", "claim_2", "hello", OWNER, NOW);
        assert!(matches!(
            result,
            Err(StoreError::Conflict("name already claimed"))
        ));
    }

    #[test]
    fn publish_lifecycle_finalizes_and_flips_pointer() {
        let mut store = store_with_site("hello");
        let files = vec![file("/index.html", SHA_A, 10), file("/a.css", SHA_B, 5)];
        store
            .create_publish("pub_1", "site_1", &files, false, None, NOW)
            .unwrap();

        let missing = store.missing_blobs(&[SHA_A, SHA_B, SHA_A]).unwrap();
        assert_eq!(missing, vec![SHA_A.to_string(), SHA_B.to_string()]);

        store.record_blob(SHA_A, 10, NOW).unwrap();
        let early = store.finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW);
        assert!(matches!(
            early,
            Err(StoreError::Conflict("publish has missing blobs"))
        ));

        store.record_blob(SHA_B, 5, NOW).unwrap();
        let finalized = store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        assert_eq!(finalized.version_number, 1);
        assert_eq!(finalized.path_count, 2);
        assert_eq!(finalized.total_bytes, 15);

        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.status, SiteStatus::Published);
        assert_eq!(site.active_version_id.as_deref(), Some("ver_1"));
        assert_eq!(site.active_version_number, Some(1));
        assert_eq!(
            store.version_file("ver_1", "/index.html").unwrap(),
            Some((SHA_A.to_string(), 10))
        );
        assert_eq!(store.version_file("ver_1", "/missing").unwrap(), None);
    }

    #[test]
    fn finalize_replay_is_rejected() {
        let mut store = store_with_site("hello");
        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                false,
                None,
                NOW,
            )
            .unwrap();
        store.record_blob(SHA_A, 10, NOW).unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        let replay = store.finalize_publish("pub_1", "ver_2", &"c".repeat(64), NOW);
        assert!(matches!(
            replay,
            Err(StoreError::Conflict("publish is not pending"))
        ));
    }

    #[test]
    fn second_publish_bumps_version_number() {
        let mut store = store_with_site("hello");
        store.record_blob(SHA_A, 10, NOW).unwrap();
        store.record_blob(SHA_B, 5, NOW).unwrap();

        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                false,
                None,
                NOW,
            )
            .unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        store
            .create_publish(
                "pub_2",
                "site_1",
                &[file("/index.html", SHA_B, 5)],
                false,
                None,
                NOW + 1,
            )
            .unwrap();
        let second = store
            .finalize_publish("pub_2", "ver_2", &"d".repeat(64), NOW + 1)
            .unwrap();
        assert_eq!(second.version_number, 2);

        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.active_version_id.as_deref(), Some("ver_2"));
    }

    #[test]
    fn sharing_roundtrip() {
        let mut store = store_with_site("hello");
        store
            .set_visibility("site_1", Visibility::Shared, NOW)
            .unwrap();
        store.add_share("site_1", "a@example.com", NOW).unwrap();
        store.add_share("site_1", "a@example.com", NOW).unwrap();
        store.add_share("site_1", "b@example.com", NOW).unwrap();
        assert_eq!(store.count_shares("site_1").unwrap(), 2);
        assert!(store.is_email_shared("site_1", "a@example.com").unwrap());
        store.remove_share("site_1", "a@example.com").unwrap();
        assert!(!store.is_email_shared("site_1", "a@example.com").unwrap());
        assert_eq!(store.shares("site_1").unwrap(), vec!["b@example.com"]);

        let missing = store.set_visibility("site_unknown", Visibility::Public, NOW);
        assert!(matches!(missing, Err(StoreError::NotFound("site"))));
    }

    #[test]
    fn email_login_tokens_and_keys_roundtrip() {
        let mut store = store_with_site("hello");
        let token_hash = "a".repeat(64);
        store
            .create_email_login_token(&token_hash, "paul@finite.vip", NOW + 900, NOW)
            .unwrap();
        let email = store
            .redeem_email_login_token(&token_hash, NOW + 1)
            .unwrap();
        assert_eq!(email, "paul@finite.vip");
        assert!(matches!(
            store.redeem_email_login_token(&token_hash, NOW + 2),
            Err(StoreError::Conflict("email login token already used"))
        ));

        store
            .add_email_key("paul@finite.vip", OWNER, NOW + 3)
            .unwrap();
        assert!(store.has_email_key("paul@finite.vip", OWNER).unwrap());
        assert_eq!(store.count_email_keys("paul@finite.vip").unwrap(), 1);

        let expired_hash = "b".repeat(64);
        store
            .create_email_login_token(&expired_hash, "paul@finite.vip", NOW + 10, NOW)
            .unwrap();
        assert!(matches!(
            store.redeem_email_login_token(&expired_hash, NOW + 11),
            Err(StoreError::Conflict("email login token expired"))
        ));
    }

    #[test]
    fn login_token_single_use_and_expiry() {
        let mut store = store_with_site("hello");
        let hash_a = "e".repeat(64);
        store
            .create_login_token(&hash_a, "site_1", "a@example.com", NOW + 900, NOW)
            .unwrap();
        let (site_id, email) = store.redeem_login_token(&hash_a, NOW + 10).unwrap();
        assert_eq!(
            (site_id.as_str(), email.as_str()),
            ("site_1", "a@example.com")
        );
        assert!(matches!(
            store.redeem_login_token(&hash_a, NOW + 11),
            Err(StoreError::Conflict("login token already used"))
        ));

        let hash_b = "f".repeat(64);
        store
            .create_login_token(&hash_b, "site_1", "a@example.com", NOW + 900, NOW)
            .unwrap();
        assert!(matches!(
            store.redeem_login_token(&hash_b, NOW + 901),
            Err(StoreError::Conflict("login token expired"))
        ));
        assert!(matches!(
            store.redeem_login_token(&"9".repeat(64), NOW),
            Err(StoreError::NotFound("login token"))
        ));
    }

    #[test]
    fn spa_flag_copies_from_publish_to_version_and_site_record() {
        let mut store = store_with_site("hello");
        store.record_blob(SHA_A, 10, NOW).unwrap();

        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                true,
                None,
                NOW,
            )
            .unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert!(site.active_version_spa);

        // A later non-SPA publish clears the flag with the pointer flip.
        store
            .create_publish(
                "pub_2",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                false,
                None,
                NOW + 1,
            )
            .unwrap();
        store
            .finalize_publish("pub_2", "ver_2", &"d".repeat(64), NOW + 1)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert!(!site.active_version_spa);
    }

    #[test]
    fn migration_adds_spa_column_to_old_databases() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        {
            let mut store = Store::open(&db_path).unwrap();
            store
                .create_site_with_claim("site_1", "claim_1", "hello", OWNER, NOW)
                .unwrap();
        }
        // Simulate a database created before the column existed.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "ALTER TABLE versions DROP COLUMN spa_fallback;
                 ALTER TABLE publishes DROP COLUMN spa_fallback;",
            )
            .unwrap();
        }
        // Reopening migrates, and the full publish flow works afterwards.
        let mut store = Store::open(&db_path).unwrap();
        store.record_blob(SHA_A, 10, NOW).unwrap();
        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                true,
                None,
                NOW,
            )
            .unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert!(site.active_version_spa);
    }

    #[test]
    fn migration_copies_legacy_allowlist_to_publish_grants() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE allowed_pubkeys (
                   pubkey TEXT PRIMARY KEY CHECK (length(pubkey) = 64),
                   note TEXT NOT NULL DEFAULT '',
                   created_at INTEGER NOT NULL
                 );
                 INSERT INTO allowed_pubkeys (pubkey, note, created_at)
                 VALUES ('1111111111111111111111111111111111111111111111111111111111111111',
                         'legacy vip',
                         1750000000);",
            )
            .unwrap();
        }

        {
            let mut store = Store::open(&db_path).unwrap();
            assert!(store.has_publish_access(OWNER, NOW).unwrap());
            let grants = store.list_publish_grants(NOW).unwrap();
            assert_eq!(grants.len(), 1);
            assert_eq!(grants[0].source, PublishGrantSource::Operator);
            assert_eq!(grants[0].note, "legacy vip");
            assert!(store.disallow_pubkey(OWNER).unwrap());
        }

        let store = Store::open(&db_path).unwrap();
        assert!(!store.has_publish_access(OWNER, NOW + 1).unwrap());
    }

    #[test]
    fn migration_rebuilds_legacy_sites_shape_for_project_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sites (
                   id TEXT PRIMARY KEY,
                   owner_pubkey TEXT NOT NULL CHECK (length(owner_pubkey) = 64),
                   owner_email TEXT,
                   site_pubkey TEXT NOT NULL CHECK (length(site_pubkey) = 64),
                   status TEXT NOT NULL CHECK (status IN ('claimed_unpublished', 'published', 'disabled', 'deleted')),
                   visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared', 'public')),
                   kind TEXT NOT NULL DEFAULT 'static' CHECK (kind IN ('static', 'app')),
                   app_port INTEGER UNIQUE CHECK (app_port IS NULL OR (app_port >= 21000 AND app_port <= 29999)),
                   active_version_id TEXT,
                   created_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL
                 );
                 INSERT INTO sites
                   (id, owner_pubkey, owner_email, site_pubkey, status, visibility, kind,
                    app_port, active_version_id, created_at, updated_at)
                 VALUES
                   ('site_legacy',
                    '1111111111111111111111111111111111111111111111111111111111111111',
                    NULL,
                    '2222222222222222222222222222222222222222222222222222222222222222',
                    'claimed_unpublished',
                    'private',
                    'static',
                    NULL,
                    NULL,
                    1750000000,
                    1750000000);",
            )
            .unwrap();
        }

        {
            let mut store = Store::open(&db_path).unwrap();
            let legacy_site: (String, String) = store
                .conn
                .query_row(
                    "SELECT owner_pubkey, status FROM sites WHERE id = 'site_legacy'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(legacy_site.0, OWNER);
            assert_eq!(legacy_site.1, "claimed_unpublished");

            let first = store
                .init_project(
                    OWNER,
                    "finite-curriculum",
                    &[project_output("finite-curriculum")],
                    NOW,
                )
                .unwrap();
            assert!(first.created);
            assert_eq!(first.outputs.len(), 1);
        }

        let mut store = Store::open(&db_path).unwrap();
        let columns = Store::table_column_names(&store.conn, "sites").unwrap();
        assert!(!columns.iter().any(|column| column == "owner_email"));
        assert!(!columns.iter().any(|column| column == "site_pubkey"));

        let replay = store
            .init_project(
                OWNER,
                "finite-curriculum",
                &[project_output("finite-curriculum")],
                NOW + 1,
            )
            .unwrap();
        assert!(!replay.created);
        assert!(!replay.outputs[0].created);
    }

    #[test]
    fn app_publish_sets_kind_and_allocates_stable_port() {
        let mut store = store_with_site("hello");
        store.record_blob(SHA_A, 10, NOW).unwrap();

        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/app.tar.gz", SHA_A, 10)],
                false,
                Some("node server.js"),
                NOW,
            )
            .unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.kind, SiteKind::App);
        assert_eq!(site.app_port, Some(21000));
        assert_eq!(site.active_version_start.as_deref(), Some("node server.js"));

        // A second app version keeps the same port.
        store.record_blob(SHA_B, 5, NOW).unwrap();
        store
            .create_publish(
                "pub_2",
                "site_1",
                &[file("/app.tar.gz", SHA_B, 5)],
                false,
                Some("bun run start.ts"),
                NOW + 1,
            )
            .unwrap();
        store
            .finalize_publish("pub_2", "ver_2", &"d".repeat(64), NOW + 1)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.app_port, Some(21000));
        assert_eq!(
            site.active_version_start.as_deref(),
            Some("bun run start.ts")
        );

        // A second app site gets the next port.
        store
            .create_site_with_claim("site_2", "claim_2", "world", OWNER, NOW)
            .unwrap();
        store
            .create_publish(
                "pub_3",
                "site_2",
                &[file("/app.tar.gz", SHA_A, 10)],
                false,
                Some("uv run app.py"),
                NOW + 2,
            )
            .unwrap();
        store
            .finalize_publish("pub_3", "ver_3", &"e".repeat(64), NOW + 2)
            .unwrap();
        let other = store.site_by_name("world").unwrap().unwrap();
        assert_eq!(other.app_port, Some(21001));
    }

    #[test]
    fn static_publish_keeps_kind_static_and_no_port() {
        let mut store = store_with_site("hello");
        store.record_blob(SHA_A, 10, NOW).unwrap();
        store
            .create_publish(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                false,
                None,
                NOW,
            )
            .unwrap();
        store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.kind, SiteKind::Static);
        assert_eq!(site.app_port, None);
        assert_eq!(site.active_version_start, None);
    }

    #[test]
    fn registry_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        {
            let mut store = Store::open(&db_path).unwrap();
            store
                .create_site_with_claim("site_1", "claim_1", "hello", OWNER, NOW)
                .unwrap();
            store
                .create_publish(
                    "pub_1",
                    "site_1",
                    &[file("/index.html", SHA_A, 10)],
                    false,
                    None,
                    NOW,
                )
                .unwrap();
            store.record_blob(SHA_A, 10, NOW).unwrap();
            store
                .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
                .unwrap();
            store.allow_pubkey(OWNER, "paul", NOW).unwrap();
        }
        let store = Store::open(&db_path).unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.status, SiteStatus::Published);
        assert_eq!(site.active_version_number, Some(1));
        assert_eq!(
            store.version_file("ver_1", "/index.html").unwrap(),
            Some((SHA_A.to_string(), 10))
        );
        assert!(store.is_pubkey_allowed(OWNER).unwrap());
    }
}
