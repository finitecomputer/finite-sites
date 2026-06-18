//! SQLite-backed registry store for Finite Sites.
//!
//! The store exposes typed reads plus transactional composites for every
//! mutation that must be atomic (claiming a name, finalizing a publish).
//! Database and corruption errors are surfaced as typed errors, never hidden
//! behind `Option`.

mod schema;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use finitesites_proto::ManifestFile;

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
    pub owner_email: Option<String>,
    pub site_pubkey: String,
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
    pub actor_pubkey: Option<String>,
    pub actor_email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FinalizedVersion {
    pub version_id: String,
    pub version_number: u32,
    pub path_count: u32,
    pub total_bytes: u64,
    pub source: Option<SourceSnapshotRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshotRecord {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRecord {
    pub email: String,
    pub added_at: u64,
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

const SITE_SELECT: &str = "
    SELECT s.id, c.name, s.owner_pubkey, s.owner_email, s.site_pubkey, s.status, s.visibility,
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
        Self::ensure_column(&conn, "sites", "owner_email", "owner_email TEXT")?;
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
        Self::ensure_column(&conn, "publishes", "start_command", "start_command TEXT")?;
        Self::ensure_column(
            &conn,
            "publishes",
            "actor_pubkey",
            "actor_pubkey TEXT CHECK (actor_pubkey IS NULL OR length(actor_pubkey) = 64)",
        )?;
        Self::ensure_column(&conn, "publishes", "actor_email", "actor_email TEXT")?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS sites_owner_email
             ON sites(owner_email) WHERE owner_email IS NOT NULL",
            [],
        )?;
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

    pub fn site_by_site_pubkey(&self, site_pubkey: &str) -> Result<Option<SiteRecord>, StoreError> {
        self.site_query("WHERE s.site_pubkey = ?1", site_pubkey)
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
        let status_raw: String = row.get(5)?;
        let visibility_raw: String = row.get(6)?;
        let version_number: Option<i64> = row.get(8)?;
        let spa_raw: i64 = row.get(9)?;
        let kind_raw: String = row.get(10)?;
        let app_port: Option<i64> = row.get(11)?;
        Ok((|| {
            Ok(SiteRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                owner_pubkey: row.get(2)?,
                owner_email: row.get(3)?,
                site_pubkey: row.get(4)?,
                status: SiteStatus::from_db(&status_raw)?,
                visibility: Visibility::from_db(&visibility_raw)?,
                active_version_id: row.get(7)?,
                active_version_number: version_number.map(|n| n as u32),
                active_version_spa: spa_raw != 0,
                kind: SiteKind::from_db(&kind_raw)?,
                app_port: app_port.map(|p| p as u16),
                active_version_start: row.get(12)?,
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
        site_pubkey: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        self.create_site_with_claim_and_owner_email(
            site_id,
            claim_id,
            name,
            owner_pubkey,
            site_pubkey,
            None,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_site_with_claim_and_owner_email(
        &mut self,
        site_id: &str,
        claim_id: &str,
        name: &str,
        owner_pubkey: &str,
        site_pubkey: &str,
        owner_email: Option<&str>,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(owner_pubkey.len() == 64 && site_pubkey.len() == 64);
        let tx = self.conn.transaction()?;
        let site_insert = tx.execute(
            "INSERT INTO sites (id, owner_pubkey, owner_email, site_pubkey, status, visibility, active_version_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'claimed_unpublished', 'private', NULL, ?5, ?5)",
            params![site_id, owner_pubkey, owner_email, site_pubkey, now],
        );
        if let Err(error) = site_insert {
            return Err(map_unique_violation(error, "site key already registered"));
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

    pub fn set_owner_email(
        &mut self,
        site_id: &str,
        owner_email: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE sites SET owner_email = ?1, updated_at = ?2 WHERE id = ?3",
            params![owner_email, now, site_id],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound("site"));
        }
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
        self.create_publish_with_actor(
            publish_id,
            site_id,
            files,
            spa_fallback,
            start_command,
            None,
            None,
            None,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_publish_with_actor(
        &mut self,
        publish_id: &str,
        site_id: &str,
        files: &[ManifestFile],
        spa_fallback: bool,
        start_command: Option<&str>,
        actor_pubkey: Option<&str>,
        actor_email: Option<&str>,
        source: Option<&SourceSnapshotRecord>,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(!files.is_empty());
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO publishes (id, site_id, status, version_id, spa_fallback, start_command, actor_pubkey, actor_email, created_at, updated_at)
             VALUES (?1, ?2, 'pending', NULL, ?4, ?5, ?6, ?7, ?3, ?3)",
            params![
                publish_id,
                site_id,
                now,
                spa_fallback,
                start_command,
                actor_pubkey,
                actor_email
            ],
        )?;
        if let Some(source) = source {
            tx.execute(
                "INSERT INTO publish_sources (publish_id, sha256, size)
                 VALUES (?1, ?2, ?3)",
                params![publish_id, source.sha256, source.size],
            )?;
        }
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
                "SELECT id, site_id, status, version_id, actor_pubkey, actor_email
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
                            actor_pubkey: row.get(4)?,
                            actor_email: row.get(5)?,
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
                        source: None,
                    })
                },
            )
            .optional()?;
        let Some(mut version) = row else {
            return Ok(None);
        };
        version.version_id = version_id.to_string();
        version.source = self.version_source(version_id)?;
        Ok(Some(version))
    }

    pub fn publish_source_by_hash(
        &self,
        publish_id: &str,
        sha256: &str,
    ) -> Result<Option<u64>, StoreError> {
        let size: Option<i64> = self
            .conn
            .query_row(
                "SELECT size FROM publish_sources WHERE publish_id = ?1 AND sha256 = ?2",
                params![publish_id, sha256],
                |row| row.get(0),
            )
            .optional()?;
        Ok(size.map(|s| s as u64))
    }

    pub fn publish_source(
        &self,
        publish_id: &str,
    ) -> Result<Option<SourceSnapshotRecord>, StoreError> {
        let source = self
            .conn
            .query_row(
                "SELECT sha256, size FROM publish_sources WHERE publish_id = ?1",
                params![publish_id],
                |row| {
                    Ok(SourceSnapshotRecord {
                        sha256: row.get(0)?,
                        size: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()?;
        Ok(source)
    }

    pub fn version_source(
        &self,
        version_id: &str,
    ) -> Result<Option<SourceSnapshotRecord>, StoreError> {
        let source = self
            .conn
            .query_row(
                "SELECT sha256, size FROM version_sources WHERE version_id = ?1",
                params![version_id],
                |row| {
                    Ok(SourceSnapshotRecord {
                        sha256: row.get(0)?,
                        size: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()?;
        Ok(source)
    }

    pub fn active_version_source(
        &self,
        site_id: &str,
    ) -> Result<Option<(u32, SourceSnapshotRecord)>, StoreError> {
        let source = self
            .conn
            .query_row(
                "SELECT v.version_number, vs.sha256, vs.size
                 FROM sites s
                 JOIN versions v ON v.id = s.active_version_id
                 JOIN version_sources vs ON vs.version_id = v.id
                 WHERE s.id = ?1",
                params![site_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u32,
                        SourceSnapshotRecord {
                            sha256: row.get(1)?,
                            size: row.get::<_, i64>(2)? as u64,
                        },
                    ))
                },
            )
            .optional()?;
        Ok(source)
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
        assert!(manifest_sha256.len() == 64);
        let tx = self.conn.transaction()?;

        let (site_id, status_raw, actor_pubkey, actor_email): (
            String,
            String,
            Option<String>,
            Option<String>,
        ) = tx
            .query_row(
                "SELECT site_id, status, actor_pubkey, actor_email FROM publishes WHERE id = ?1",
                params![publish_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
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
        let missing_source_count: i64 = tx.query_row(
            "SELECT COUNT(*)
             FROM publish_sources ps
             LEFT JOIN blobs b ON b.sha256 = ps.sha256
             WHERE ps.publish_id = ?1 AND b.sha256 IS NULL",
            params![publish_id],
            |row| row.get(0),
        )?;
        if missing_source_count > 0 {
            return Err(StoreError::Conflict("publish has missing source blob"));
        }
        let source: Option<SourceSnapshotRecord> = tx
            .query_row(
                "SELECT sha256, size FROM publish_sources WHERE publish_id = ?1",
                params![publish_id],
                |row| {
                    Ok(SourceSnapshotRecord {
                        sha256: row.get(0)?,
                        size: row.get::<_, i64>(1)? as u64,
                    })
                },
            )
            .optional()?;

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
            "INSERT INTO versions (id, site_id, version_number, manifest_sha256, path_count, total_bytes, spa_fallback, start_command, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, p.spa_fallback, p.start_command, ?7 FROM publishes p WHERE p.id = ?8",
            params![version_id, site_id, version_number, manifest_sha256, path_count, total_bytes, now, publish_id],
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
            "INSERT INTO version_sources (version_id, sha256, size, created_at)
             SELECT ?1, sha256, size, ?3 FROM publish_sources WHERE publish_id = ?2",
            params![version_id, publish_id, now],
        )?;
        tx.execute(
            "UPDATE publishes SET status = 'finalized', version_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![version_id, now, publish_id],
        )?;
        tx.execute(
            "UPDATE sites SET active_version_id = ?1, status = 'published', updated_at = ?2 WHERE id = ?3",
            params![version_id, now, site_id],
        )?;
        let metadata = publish_event_metadata(
            actor_pubkey.as_deref(),
            actor_email.as_deref(),
            version_number as u32,
            source.is_some(),
        );
        tx.execute(
            "INSERT INTO site_events (site_id, action, actor_pubkey, metadata, created_at)
             VALUES (?1, 'publish_succeeded', ?2, ?3, ?4)",
            params![site_id, actor_pubkey, metadata, now],
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
            source,
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

    // ---- editors and email keys ------------------------------------------

    pub fn add_editor(
        &mut self,
        site_id: &str,
        email: &str,
        added_by_pubkey: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        assert!(added_by_pubkey.len() == 64);
        self.conn.execute(
            "INSERT INTO site_editors (site_id, email, added_by_pubkey, added_at, removed_at)
             VALUES (?1, ?2, ?3, ?4, NULL)
             ON CONFLICT(site_id, email) DO UPDATE SET
                added_by_pubkey = ?3,
                added_at = ?4,
                removed_at = NULL",
            params![site_id, email, added_by_pubkey, now],
        )?;
        Ok(())
    }

    pub fn remove_editor(
        &mut self,
        site_id: &str,
        email: &str,
        now: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE site_editors
             SET removed_at = CASE WHEN ?3 >= added_at THEN ?3 ELSE added_at END
             WHERE site_id = ?1 AND email = ?2 AND removed_at IS NULL",
            params![site_id, email, now],
        )?;
        Ok(())
    }

    pub fn editors(&self, site_id: &str) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT email FROM site_editors
             WHERE site_id = ?1 AND removed_at IS NULL
             ORDER BY email",
        )?;
        let rows = stmt.query_map(params![site_id], |row| row.get(0))?;
        let mut out = Vec::new();
        // Bounded by MAX_EDITORS_PER_SITE, enforced by the engine.
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn count_editors(&self, site_id: &str) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM site_editors WHERE site_id = ?1 AND removed_at IS NULL",
            params![site_id],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn is_email_editor(&self, site_id: &str, email: &str) -> Result<bool, StoreError> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM site_editors
                 WHERE site_id = ?1 AND email = ?2 AND removed_at IS NULL",
                params![site_id, email],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

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

fn publish_event_metadata(
    actor_pubkey: Option<&str>,
    actor_email: Option<&str>,
    version_number: u32,
    has_source: bool,
) -> String {
    // Inputs are normalized before storage: pubkeys are hex and emails reject
    // quotes/backslashes/commas, so this bounded audit JSON is safe to build
    // without a JSON dependency in the store crate.
    let mut metadata = format!(
        "{{\"version\":{},\"has_source\":{}",
        version_number, has_source
    );
    if let Some(pubkey) = actor_pubkey {
        metadata.push_str(",\"actor_pubkey\":\"");
        metadata.push_str(pubkey);
        metadata.push('"');
    }
    if let Some(email) = actor_email {
        metadata.push_str(",\"actor_email\":\"");
        metadata.push_str(email);
        metadata.push('"');
    }
    metadata.push('}');
    metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const SITE_KEY: &str = "2222222222222222222222222222222222222222222222222222222222222222";
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
            .create_site_with_claim("site_1", "claim_1", name, OWNER, SITE_KEY, NOW)
            .unwrap();
        store
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
        let result =
            store.create_site_with_claim("site_2", "claim_2", "hello", OWNER, OTHER_KEY, NOW);
        assert!(matches!(
            result,
            Err(StoreError::Conflict("name already claimed"))
        ));
    }

    #[test]
    fn duplicate_site_key_conflicts() {
        let mut store = store_with_site("hello");
        let result =
            store.create_site_with_claim("site_2", "claim_2", "world", OWNER, SITE_KEY, NOW);
        assert!(matches!(
            result,
            Err(StoreError::Conflict("site key already registered"))
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
    fn owner_email_and_editors_roundtrip() {
        let mut store = store_with_site("hello");
        store
            .set_owner_email("site_1", "paul@finite.vip", NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.owner_email.as_deref(), Some("paul@finite.vip"));

        store
            .add_editor("site_1", "skyler_bot@finite.vip", SITE_KEY, NOW)
            .unwrap();
        store
            .add_editor("site_1", "skyler_bot@finite.vip", SITE_KEY, NOW + 1)
            .unwrap();
        assert!(
            store
                .is_email_editor("site_1", "skyler_bot@finite.vip")
                .unwrap()
        );
        assert_eq!(
            store.editors("site_1").unwrap(),
            vec!["skyler_bot@finite.vip"]
        );
        store
            .remove_editor("site_1", "skyler_bot@finite.vip", NOW + 2)
            .unwrap();
        assert!(
            !store
                .is_email_editor("site_1", "skyler_bot@finite.vip")
                .unwrap()
        );
        assert!(store.editors("site_1").unwrap().is_empty());
        store
            .add_editor("site_1", "skyler_bot@finite.vip", SITE_KEY, NOW + 3)
            .unwrap();
        assert_eq!(store.count_editors("site_1").unwrap(), 1);
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
    fn source_snapshot_copies_to_finalized_version() {
        let mut store = store_with_site("hello");
        let source = SourceSnapshotRecord {
            sha256: SHA_B.into(),
            size: 20,
        };
        store
            .create_publish_with_actor(
                "pub_1",
                "site_1",
                &[file("/index.html", SHA_A, 10)],
                false,
                None,
                Some(SITE_KEY),
                None,
                Some(&source),
                NOW,
            )
            .unwrap();
        store.record_blob(SHA_A, 10, NOW).unwrap();
        let missing_source = store.finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW);
        assert!(matches!(
            missing_source,
            Err(StoreError::Conflict("publish has missing source blob"))
        ));

        store.record_blob(SHA_B, 20, NOW).unwrap();
        let finalized = store
            .finalize_publish("pub_1", "ver_1", &"c".repeat(64), NOW)
            .unwrap();
        assert_eq!(finalized.source, Some(source.clone()));
        assert_eq!(store.version_source("ver_1").unwrap(), Some(source.clone()));
        assert_eq!(
            store.active_version_source("site_1").unwrap(),
            Some((1, source))
        );
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
                .create_site_with_claim("site_1", "claim_1", "hello", OWNER, SITE_KEY, NOW)
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
    fn migration_adds_owner_email_column_before_index() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("registry.db");
        {
            let mut store = Store::open(&db_path).unwrap();
            store
                .create_site_with_claim("site_1", "claim_1", "hello", OWNER, SITE_KEY, NOW)
                .unwrap();
        }
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "DROP INDEX IF EXISTS sites_owner_email;
                 ALTER TABLE sites DROP COLUMN owner_email;",
            )
            .unwrap();
        }

        let mut store = Store::open(&db_path).unwrap();
        store
            .set_owner_email("site_1", "paul@finite.vip", NOW)
            .unwrap();
        let site = store.site_by_name("hello").unwrap().unwrap();
        assert_eq!(site.owner_email.as_deref(), Some("paul@finite.vip"));
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
            .create_site_with_claim("site_2", "claim_2", "world", OWNER, OTHER_KEY, NOW)
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
                .create_site_with_claim("site_1", "claim_1", "hello", OWNER, SITE_KEY, NOW)
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
