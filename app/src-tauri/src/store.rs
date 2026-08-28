use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{
    params, Connection, DatabaseName, OptionalExtension, Transaction, TransactionBehavior,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 38;
const HISTORY_RESULT_LIMIT: i64 = 500;

pub struct Store {
    connection: Mutex<Connection>,
    artifacts_root: PathBuf,
    voices_root: PathBuf,
    transcription_sources_root: PathBuf,
}

#[derive(Clone, Debug)]
struct PreparedVideoOutput {
    id: String,
    explicit_id: bool,
    project_id: String,
    version_id: Option<String>,
    job_id: Option<String>,
    kind: String,
    label: String,
    artifact_path: PathBuf,
    mime_type: String,
    size_bytes: i64,
    sha256: String,
    duration_us: Option<i64>,
    width: Option<i64>,
    height: Option<i64>,
    is_primary: bool,
    provenance_json: String,
}

static NEVER_CANCEL_VIDEO_PUBLICATION: AtomicBool = AtomicBool::new(false);

impl Store {
    pub fn open(data_root: PathBuf, artifacts_root: PathBuf) -> Result<Self, String> {
        ensure_private_directory(&data_root, "soundAr data")?;
        ensure_private_directory(&artifacts_root, "artifact")?;
        ensure_private_directory(&artifacts_root.join("video"), "video artifact")?;
        let voices_root = data_root.join("voices");
        ensure_private_directory(&voices_root, "voice library")?;
        let transcription_sources_root = data_root.join("transcription-sources");
        ensure_private_directory(&transcription_sources_root, "transcription storage")?;

        let database_path = data_root.join("soundar.sqlite3");
        let mut connection = Connection::open(&database_path)
            .map_err(|error| format!("Could not open the soundAr database: {error}"))?;
        secure_private_file(&database_path, "soundAr database")?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("Could not configure the soundAr database: {error}"))?;
        verify_database_integrity(&connection)
            .map_err(|error| database_recovery_error(&database_path, &data_root, &error))?;
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|error| format!("Could not configure the soundAr database: {error}"))?;

        let current_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("Could not inspect the soundAr database: {error}"))?;
        if current_version > SCHEMA_VERSION {
            return Err(format!(
                "This soundAr database uses schema {current_version}, but this app supports schema {SCHEMA_VERSION}."
            ));
        }
        if current_version < SCHEMA_VERSION {
            if current_version > 0 && database_path.is_file() {
                create_migration_backup(&connection, &database_path)?;
            }
            migrate(&mut connection, current_version)?;
        }
        if transcription_schema_requires_repair(&connection)? {
            if current_version == SCHEMA_VERSION && database_path.is_file() {
                create_migration_backup(&connection, &database_path)?;
            }
            repair_transcription_schema(&mut connection)?;
        }
        verify_database_integrity(&connection)
            .map_err(|error| database_recovery_error(&database_path, &data_root, &error))?;
        backfill_voice_provenance(&connection)?;
        recover_incomplete_publications(&mut connection, &artifacts_root)?;
        cleanup_partial_artifacts(&artifacts_root)?;
        cleanup_preview_artifacts(&artifacts_root)?;

        let recovered_at = now();
        connection.execute(
            "INSERT INTO job_events (id, job_id, status, progress, error, created_at)
             SELECT lower(hex(randomblob(16))), id, 'failed', progress, 'Interrupted when soundAr last closed', ?1 FROM jobs WHERE status IN ('preparing', 'running')",
            [&recovered_at],
        ).map_err(|error| format!("Could not record interrupted job recovery: {error}"))?;
        connection
            .execute(
                "UPDATE jobs SET status = 'failed', error = 'Interrupted when soundAr last closed',
                preview_audio_path = NULL, preview_duration_seconds = NULL, updated_at = ?1
             WHERE status IN ('preparing', 'running')",
                [&recovered_at],
            )
            .map_err(|error| format!("Could not recover interrupted jobs: {error}"))?;
        connection
            .execute(
                "UPDATE batch_items SET status = 'failed', error = 'Interrupted when soundAr last closed', updated_at = ?1 WHERE status = 'running'",
                [now()],
            )
            .map_err(|error| format!("Could not recover interrupted batch items: {error}"))?;
        connection
            .execute(
                "UPDATE batch_runs SET status = 'failed', error = 'Interrupted when soundAr last closed', updated_at = ?1 WHERE status = 'running'",
                [now()],
            )
            .map_err(|error| format!("Could not recover interrupted batches: {error}"))?;
        connection
            .execute(
                "UPDATE comparison_takes SET status = 'failed', error = 'Interrupted when soundAr last closed', updated_at = ?1 WHERE status IN ('preparing', 'running')",
                [now()],
            )
            .map_err(|error| format!("Could not recover interrupted comparison takes: {error}"))?;
        connection
            .execute(
                "UPDATE comparison_runs SET status = CASE
                    WHEN EXISTS(SELECT 1 FROM comparison_takes WHERE comparison_id = comparison_runs.id AND status = 'completed') THEN 'partial'
                    ELSE 'failed' END,
                    updated_at = ?1
                 WHERE status IN ('queued', 'running')",
                [now()],
            )
            .map_err(|error| format!("Could not recover interrupted comparisons: {error}"))?;
        connection
            .execute(
                "UPDATE video_workflow_stages
                 SET status = 'interrupted', error_json = json_object('code', 'video.interrupted', 'message', 'Interrupted when soundAr last closed'), updated_at = ?1
                 WHERE status = 'running'",
                [&recovered_at],
            )
            .map_err(|error| format!("Could not checkpoint interrupted video work: {error}"))?;
        connection
            .execute(
                "DELETE FROM video_project_locks WHERE lease_expires_at <= ?1",
                [&recovered_at],
            )
            .map_err(|error| format!("Could not expire stale video project leases: {error}"))?;

        Ok(Self {
            connection: Mutex::new(connection),
            artifacts_root,
            voices_root,
            transcription_sources_root,
        })
    }

    pub fn import_transcription_source(&self, raw_path: &str) -> Result<PathBuf, String> {
        let source = PathBuf::from(raw_path);
        if !source.is_file() {
            return Err("The selected audio file no longer exists".to_string());
        }
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or("The selected audio file has no extension")?;
        if !matches!(extension.as_str(), "wav" | "flac" | "mp3" | "m4a" | "ogg") {
            return Err("Transcription input must be WAV, FLAC, MP3, M4A, or OGG".to_string());
        }
        let destination = self.transcription_sources_root.join(format!(
            "{}.{}",
            Uuid::new_v4().simple(),
            extension
        ));
        let temporary = destination.with_extension(format!("{extension}.partial"));
        fs::copy(&source, &temporary)
            .map_err(|error| format!("Could not import transcription audio: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("Could not finalize transcription audio: {error}"))?;
        Ok(destination)
    }

    pub fn record_engine_event(
        &self,
        engine: &str,
        event: &str,
        detail: &str,
    ) -> Result<(), String> {
        if engine.is_empty()
            || engine.len() > 80
            || !engine
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        {
            return Err("Engine event identifier is invalid".to_string());
        }
        if !matches!(event, "started" | "recovered" | "failed" | "stopped") {
            return Err("Engine event type is invalid".to_string());
        }
        if detail.len() > 80
            || !detail
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
        {
            return Err("Engine event detail must be a short diagnostic code".to_string());
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not record engine lifecycle event: {error}"))?;
        transaction.execute(
            "INSERT INTO engine_events (id, engine, event, detail, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().simple().to_string(), engine, event, detail, now()],
        ).map_err(|error| format!("Could not save engine lifecycle event: {error}"))?;
        transaction.execute(
            "DELETE FROM engine_events WHERE engine = ?1 AND id NOT IN (SELECT id FROM engine_events WHERE engine = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 200)",
            [engine],
        ).map_err(|error| format!("Could not bound engine lifecycle history: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit engine lifecycle event: {error}"))
    }

    pub fn engine_event_summary(&self, engine: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let (starts, recoveries, failures): (i64, i64, i64) = connection.query_row(
            "SELECT SUM(CASE WHEN event = 'started' THEN 1 ELSE 0 END), SUM(CASE WHEN event = 'recovered' THEN 1 ELSE 0 END), SUM(CASE WHEN event = 'failed' THEN 1 ELSE 0 END) FROM engine_events WHERE engine = ?1",
            [engine],
            |row| Ok((row.get::<_, Option<i64>>(0)?.unwrap_or(0), row.get::<_, Option<i64>>(1)?.unwrap_or(0), row.get::<_, Option<i64>>(2)?.unwrap_or(0))),
        ).map_err(|error| format!("Could not summarize engine lifecycle: {error}"))?;
        let last_start: Option<String> = connection.query_row(
            "SELECT created_at FROM engine_events WHERE engine = ?1 AND event = 'started' ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [engine], |row| row.get(0),
        ).optional().map_err(|error| format!("Could not read the latest engine start: {error}"))?;
        let last_failure: Option<(String, String)> = connection.query_row(
            "SELECT created_at, detail FROM engine_events WHERE engine = ?1 AND event = 'failed' ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [engine], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|error| format!("Could not read the latest engine failure: {error}"))?;
        Ok(json!({
            "worker_starts": starts,
            "worker_restarts": recoveries,
            "worker_failures": failures,
            "last_started_at": last_start,
            "last_failure_at": last_failure.as_ref().map(|item| item.0.clone()),
            "last_error": last_failure.map(|item| item.1),
        }))
    }

    pub fn engine_needs_recovery(&self, engine: &str) -> Result<bool, String> {
        let connection = self.lock()?;
        let event: Option<String> = connection.query_row(
            "SELECT event FROM engine_events WHERE engine = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [engine], |row| row.get(0),
        ).optional().map_err(|error| format!("Could not inspect engine recovery state: {error}"))?;
        Ok(event.as_deref() == Some("failed"))
    }

    pub fn application_settings(&self) -> Result<Value, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT key, value_json FROM settings WHERE key IN ('theme', 'dense_tables', 'reduced_motion')")
            .map_err(|error| format!("Could not prepare application settings: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("Could not read application settings: {error}"))?;
        let mut settings = json!({"theme": "light", "dense_tables": true, "reduced_motion": false});
        for row in rows {
            let (key, raw) =
                row.map_err(|error| format!("Could not read an application setting: {error}"))?;
            if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                settings[&key] = value;
            }
        }
        Ok(settings)
    }

    pub fn save_application_setting(&self, key: &str, value: &Value) -> Result<Value, String> {
        let valid = match key {
            "theme" => value
                .as_str()
                .is_some_and(|theme| matches!(theme, "dark" | "light")),
            "dense_tables" | "reduced_motion" => value.is_boolean(),
            _ => false,
        };
        if !valid {
            return Err("Unsupported application setting or value".to_string());
        }
        let connection = self.lock()?;
        connection.execute(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![key, value.to_string(), now()],
        ).map_err(|error| format!("Could not save application setting: {error}"))?;
        drop(connection);
        self.application_settings()
    }

    pub fn capture_path(&self) -> Result<PathBuf, String> {
        fs::create_dir_all(&self.transcription_sources_root)
            .map_err(|error| format!("Could not create capture storage: {error}"))?;
        Ok(self
            .transcription_sources_root
            .join(format!("capture-{}.wav", Uuid::new_v4().simple())))
    }

    pub fn transcription_audio_bytes(&self, raw_path: &str) -> Result<Vec<u8>, String> {
        let path = self.transcription_audio_path(raw_path)?;
        fs::read(path).map_err(|error| format!("Could not read transcription audio: {error}"))
    }

    pub fn transcription_audio_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        let root = self
            .transcription_sources_root
            .canonicalize()
            .map_err(|error| format!("Could not resolve transcription storage: {error}"))?;
        let path = PathBuf::from(raw_path)
            .canonicalize()
            .map_err(|error| format!("Transcription audio was not found: {error}"))?;
        if !path.starts_with(root) {
            return Err("Transcription audio is outside managed storage".to_string());
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("Could not inspect transcription audio: {error}"))?;
        if metadata.len() > 250 * 1024 * 1024 {
            return Err("Transcription audio is too large for in-app playback".to_string());
        }
        Ok(path)
    }

    pub fn create_job(&self, kind: &str, request: &Value) -> Result<String, String> {
        let priority = priority_value(request.get("priority"))?;
        let request_json = durable_request(request).to_string();
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the queued job: {error}"))?;
        transaction.execute(
                "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at) VALUES (?1, ?2, 'queued', ?3, 0, 1, ?4, ?5, ?5)",
                params![id, kind, request_json, priority, timestamp],
            )
            .map_err(|error| format!("Could not queue the job: {error}"))?;
        insert_job_event(&transaction, &id, "queued", 0.0, None, &timestamp)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the queued job: {error}"))?;
        Ok(id)
    }

    pub fn create_idempotent_job(
        &self,
        kind: &str,
        idempotency_key: &str,
        request: &Value,
    ) -> Result<Option<(String, bool)>, String> {
        let priority = priority_value(request.get("priority"))?;
        let request_json = durable_request(request).to_string();
        let request_sha256 = sha256_bytes(request_json.as_bytes());
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the idempotent job: {error}"))?;
        let existing = transaction
            .query_row(
                "SELECT request_sha256, job_id FROM api_job_submissions WHERE operation = ?1 AND idempotency_key = ?2",
                params![kind, idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the idempotency key: {error}"))?;
        if let Some((existing_sha256, job_id)) = existing {
            return if existing_sha256 == request_sha256 {
                Ok(Some((job_id, false)))
            } else {
                Ok(None)
            };
        }
        transaction
            .execute(
                "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at) VALUES (?1, ?2, 'preparing', ?3, 0.05, 1, ?4, ?5, ?5)",
                params![id, kind, request_json, priority, timestamp],
            )
            .map_err(|error| format!("Could not queue the idempotent job: {error}"))?;
        insert_job_event(&transaction, &id, "preparing", 0.05, None, &timestamp)?;
        transaction
            .execute(
                "INSERT INTO api_job_submissions (operation, idempotency_key, request_sha256, job_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![kind, idempotency_key, request_sha256, id, timestamp],
            )
            .map_err(|error| format!("Could not record the idempotent submission: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the idempotent job: {error}"))?;
        Ok(Some((id, true)))
    }

    pub fn update_job(&self, id: &str, status: &str, progress: f64) -> Result<(), String> {
        validate_job_status(status)?;
        let progress = progress.clamp(0.0, 1.0);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the job update: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE jobs SET status = ?2, progress = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, status, progress, timestamp],
            )
            .map_err(|error| format!("Could not update the job: {error}"))?;
        if changed == 0 {
            return Err("The job was not found".to_string());
        }
        insert_job_event(&transaction, id, status, progress, None, &timestamp)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the job update: {error}"))?;
        Ok(())
    }

    pub fn update_job_preview(
        &self,
        id: &str,
        raw_path: &str,
        duration_seconds: f64,
        first_audio_seconds: f64,
        progress: f64,
    ) -> Result<(), String> {
        let path = self.validate_artifact_path(raw_path)?;
        let expected_name = format!(".preview-{id}.wav");
        if path.file_name().and_then(|value| value.to_str()) != Some(&expected_name) {
            return Err("The runtime returned an invalid job preview path".to_string());
        }
        validate_audio_file(&path)?;
        let timestamp = now();
        let progress = progress.clamp(0.2, 0.94);
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE jobs SET preview_audio_path = ?2, preview_duration_seconds = ?3,
                    first_audio_seconds = CASE WHEN first_audio_seconds IS NULL THEN ?4 ELSE first_audio_seconds END,
                    progress = MAX(progress, ?5), updated_at = ?6
                 WHERE id = ?1 AND status = 'running'",
                params![id, path.to_string_lossy(), duration_seconds.max(0.0), first_audio_seconds.max(0.0), progress, timestamp],
            )
            .map_err(|error| format!("Could not update the job audio preview: {error}"))?;
        if changed == 0 {
            return Err("The active generation job was not found".to_string());
        }
        Ok(())
    }

    pub fn job_preview_audio(&self, id: &str) -> Result<Vec<u8>, String> {
        let raw_path: String = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT preview_audio_path FROM jobs WHERE id = ?1 AND preview_audio_path IS NOT NULL AND status IN ('preparing', 'running')",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("Could not locate the job audio preview: {error}"))?
                .ok_or("This job has no active audio preview")?
        };
        let path = self.validate_artifact_path(&raw_path)?;
        let expected_name = format!(".preview-{id}.wav");
        if path.file_name().and_then(|value| value.to_str()) != Some(&expected_name) {
            return Err("The stored job preview path is invalid".to_string());
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("Could not inspect the job audio preview: {error}"))?;
        if metadata.len() > 128 * 1024 * 1024 {
            return Err("The job audio preview is too large to play safely".to_string());
        }
        fs::read(path).map_err(|error| format!("Could not read the job audio preview: {error}"))
    }

    fn clear_job_preview(&self, id: &str) -> Result<(), String> {
        let raw_path: Option<String> = {
            let connection = self.lock()?;
            let path = connection
                .query_row(
                    "SELECT preview_audio_path FROM jobs WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("Could not locate the job audio preview: {error}"))?
                .flatten();
            connection
                .execute(
                    "UPDATE jobs SET preview_audio_path = NULL, preview_duration_seconds = NULL WHERE id = ?1",
                    [id],
                )
                .map_err(|error| format!("Could not clear the job audio preview: {error}"))?;
            path
        };
        if let Some(raw_path) = raw_path {
            let path = self.validate_artifact_path_allow_missing(&raw_path)?;
            if path.file_name().and_then(|value| value.to_str())
                == Some(&format!(".preview-{id}.wav"))
            {
                fs::remove_file(path).ok();
            }
        }
        Ok(())
    }

    pub fn start_job(&self, id: &str) -> Result<String, String> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the job update: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE jobs SET status = 'running', progress = MAX(progress, 0.2), updated_at = ?2
                 WHERE id = ?1 AND status IN ('queued', 'preparing')",
                params![id, timestamp],
            )
            .map_err(|error| format!("Could not start the job: {error}"))?;
        let (status, progress) = transaction
            .query_row(
                "SELECT status, progress FROM jobs WHERE id = ?1",
                [id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the job: {error}"))?
            .ok_or_else(|| "The job was not found".to_string())?;
        if changed > 0 {
            insert_job_event(&transaction, id, "running", progress, None, &timestamp)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the job update: {error}"))?;
        Ok(status)
    }

    pub fn fail_job(&self, id: &str, message: &str) -> Result<(), String> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the failed job update: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE jobs SET status = 'failed', error = ?2, updated_at = ?3
                 WHERE id = ?1 AND status IN ('queued', 'preparing', 'running')",
                params![id, message, timestamp],
            )
            .map_err(|error| format!("Could not record the failed job: {error}"))?;
        if changed == 0 {
            let exists = transaction
                .query_row("SELECT 1 FROM jobs WHERE id = ?1", [id], |_| Ok(()))
                .optional()
                .map_err(|error| format!("Could not inspect the failed job: {error}"))?
                .is_some();
            if !exists {
                return Err("The job was not found".to_string());
            }
            transaction
                .commit()
                .map_err(|error| format!("Could not commit the unchanged job: {error}"))?;
            return Ok(());
        }
        let progress: f64 = transaction
            .query_row("SELECT progress FROM jobs WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .map_err(|error| format!("Could not read failed job progress: {error}"))?;
        insert_job_event(
            &transaction,
            id,
            "failed",
            progress,
            Some(message),
            &timestamp,
        )?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the failed job: {error}"))?;
        drop(connection);
        self.clear_job_preview(id)?;
        Ok(())
    }

    pub fn complete_job(&self, id: &str) -> Result<bool, String> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start job completion: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE jobs SET status = 'completed', progress = 1, error = NULL, updated_at = ?2
                 WHERE id = ?1 AND status IN ('queued', 'preparing', 'running')",
                params![id, timestamp],
            )
            .map_err(|error| format!("Could not complete the job: {error}"))?;
        if changed > 0 {
            insert_job_event(&transaction, id, "completed", 1.0, None, &timestamp)?;
        } else {
            let exists = transaction
                .query_row("SELECT 1 FROM jobs WHERE id = ?1", [id], |_| Ok(()))
                .optional()
                .map_err(|error| format!("Could not inspect the completed job: {error}"))?
                .is_some();
            if !exists {
                return Err("The job was not found".to_string());
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit job completion: {error}"))?;
        drop(connection);
        self.clear_job_preview(id)?;
        Ok(changed > 0)
    }

    pub fn cancel_job(&self, id: &str) -> Result<bool, String> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start cancellation: {error}"))?;
        let changed = transaction.execute(
                "UPDATE jobs SET status = 'cancelled', error = NULL, updated_at = ?2 WHERE id = ?1 AND status IN ('queued', 'preparing', 'running')",
                params![id, timestamp],
            )
            .map_err(|error| format!("Could not cancel the job: {error}"))?;
        if changed > 0 {
            let progress: f64 = transaction
                .query_row("SELECT progress FROM jobs WHERE id = ?1", [id], |row| {
                    row.get(0)
                })
                .map_err(|error| format!("Could not read cancelled job progress: {error}"))?;
            insert_job_event(&transaction, id, "cancelled", progress, None, &timestamp)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit cancellation: {error}"))?;
        drop(connection);
        self.clear_job_preview(id)?;
        Ok(changed > 0)
    }

    pub fn job_status(&self, id: &str) -> Result<Option<String>, String> {
        let connection = self.lock()?;
        connection
            .query_row("SELECT status FROM jobs WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| format!("Could not read the job state: {error}"))
    }

    /// Test-only crash injection. A worker can publish its transactional result immediately
    /// before the generic worker wrapper persists `completed`; reopening the Store must then
    /// recover this synthetic in-flight state exactly like an operating-system termination.
    #[cfg(test)]
    pub(crate) fn simulate_worker_crash_after_commit(&self, id: &str) -> Result<(), String> {
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE jobs SET status = 'running', progress = 0.99, error = NULL, updated_at = ?2
                 WHERE id = ?1 AND status = 'completed'",
                params![id, now()],
            )
            .map_err(|error| format!("Could not inject the post-commit worker crash: {error}"))?;
        if changed != 1 {
            return Err(
                "The post-commit crash injection requires one completed durable job".into(),
            );
        }
        Ok(())
    }

    pub fn complete_synthesis(
        &self,
        job_id: &str,
        request: &Value,
        result: &Value,
    ) -> Result<Value, String> {
        let staging_raw = result.get("staging_path").and_then(Value::as_str);
        if self.job_status(job_id)?.as_deref() == Some("cancelled") {
            if let Some(path) =
                staging_raw.or_else(|| result.get("audio_path").and_then(Value::as_str))
            {
                let _ = self
                    .validate_artifact_path(path)
                    .and_then(|path| fs::remove_file(path).map_err(|error| error.to_string()));
            }
            return Err("Generation was cancelled before the artifact was published".to_string());
        }
        let raw_path = result
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or("The inference engine returned no audio path")?;
        let (audio_path, source_path) = if let Some(staging_raw) = staging_raw {
            let final_path = self.validate_artifact_path_allow_missing(raw_path)?;
            let staging_path = self.validate_artifact_path(staging_raw)?;
            let expected_staging =
                PathBuf::from(format!("{}.partial", final_path.to_string_lossy()));
            if staging_path != expected_staging {
                return Err("The inference worker returned an invalid staging path".to_string());
            }
            (final_path, staging_path)
        } else {
            let path = self.validate_artifact_path(raw_path)?;
            (path.clone(), path)
        };
        validate_audio_file(&source_path)?;
        let metadata = fs::metadata(&source_path)
            .map_err(|error| format!("Could not inspect generated audio: {error}"))?;
        let checksum = sha256_file(&source_path)?;
        let artifact_id = Uuid::new_v4().simple().to_string();
        let history_id = result
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(job_id)
            .to_string();
        let generation_kind = request
            .get("generation_kind")
            .and_then(Value::as_str)
            .unwrap_or("speech");
        if !matches!(generation_kind, "speech" | "music") {
            return Err("The generation request has an unsupported kind".to_string());
        }
        let result_generation_kind = result
            .get("generation_kind")
            .and_then(Value::as_str)
            .unwrap_or(generation_kind);
        if result_generation_kind != generation_kind {
            return Err("The inference engine returned the wrong generation kind".to_string());
        }
        let text = request
            .get("text")
            .or_else(|| request.get("prompt"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let title = request
            .get("title")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| title_from_text(text));
        let voice = if generation_kind == "music" {
            "Not applicable"
        } else {
            request
                .get("voice_name")
                .and_then(Value::as_str)
                .or_else(|| request.get("speaker").and_then(Value::as_str))
                .unwrap_or("Default voice")
        };
        let created_at = result
            .get("created_at")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(now);
        let format = audio_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("wav")
            .to_ascii_lowercase();
        let model_id = result.get("model_id").and_then(Value::as_str).unwrap_or("");
        let engine = result.get("engine").and_then(Value::as_str).unwrap_or("");
        let sample_rate = result
            .get("sample_rate")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let duration = result
            .get("duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let inference = result
            .get("inference_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let rtf = result.get("rtf").and_then(Value::as_f64).unwrap_or(0.0);
        let vram = result
            .get("vram_peak_mb")
            .and_then(numeric_value)
            .map(|value| value.round() as i64)
            .unwrap_or(0);
        let runtime_worker_state = result
            .get("runtime_worker_state")
            .and_then(Value::as_str)
            .filter(|value| matches!(*value, "cold" | "warm"))
            .unwrap_or("unknown");
        let end_to_end_seconds = result
            .get("end_to_end_seconds")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(inference);
        let runtime_overhead_seconds = result
            .get("runtime_overhead_seconds")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or_else(|| (end_to_end_seconds - inference).max(0.0));
        let waveform = result
            .get("waveform")
            .cloned()
            .unwrap_or_else(|| json!([]))
            .to_string();

        let publication_id = staging_raw.map(|_| Uuid::new_v4().simple().to_string());
        if let Some(publication_id) = publication_id.as_deref() {
            let connection = self.lock()?;
            let status: Option<String> = connection
                .query_row("SELECT status FROM jobs WHERE id = ?1", [job_id], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(|error| format!("Could not verify the generation state: {error}"))?;
            if status.as_deref() == Some("cancelled") {
                fs::remove_file(&source_path).ok();
                return Err(
                    "Generation was cancelled before the artifact was published".to_string()
                );
            }
            connection.execute(
                "INSERT INTO artifact_publications (id, job_id, staging_path, final_path, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![publication_id, job_id, source_path.to_string_lossy(), audio_path.to_string_lossy(), now()],
            ).map_err(|error| format!("Could not prepare artifact publication: {error}"))?;
            drop(connection);
            if let Err(error) = fs::rename(&source_path, &audio_path) {
                let connection = self.lock()?;
                connection
                    .execute(
                        "DELETE FROM artifact_publications WHERE id = ?1",
                        [publication_id],
                    )
                    .ok();
                return Err(format!("Could not publish generated audio: {error}"));
            }
        }

        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize the generation: {error}"))?;
        let status: Option<String> = transaction
            .query_row("SELECT status FROM jobs WHERE id = ?1", [job_id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| format!("Could not verify the generation state: {error}"))?;
        if status.as_deref() == Some("cancelled") {
            drop(transaction);
            if publication_id.is_some() {
                fs::remove_file(&audio_path).ok();
                let connection = self.lock()?;
                connection
                    .execute(
                        "DELETE FROM artifact_publications WHERE job_id = ?1",
                        [job_id],
                    )
                    .ok();
            }
            return Err("Generation was cancelled before the artifact was published".to_string());
        }
        transaction
            .execute(
                "INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![artifact_id, job_id, audio_path.to_string_lossy(), format, metadata.len() as i64, checksum, created_at],
            )
            .map_err(|error| format!("Could not store the audio artifact: {error}"))?;
        transaction
            .execute(
                "INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, generation_kind, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at, runtime_worker_state, end_to_end_seconds, runtime_overhead_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
                params![history_id, job_id, artifact_id, title, voice, text, model_id, engine, generation_kind, audio_path.to_string_lossy(), sample_rate, duration, inference, rtf, vram, waveform, created_at, runtime_worker_state, end_to_end_seconds, runtime_overhead_seconds],
            )
            .map_err(|error| format!("Could not store generation history: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'completed', progress = 1, output_artifact_id = ?2, updated_at = ?3 WHERE id = ?1 AND status != 'cancelled'",
                params![job_id, artifact_id, now()],
            )
            .map_err(|error| format!("Could not complete the generation job: {error}"))?;
        insert_job_event(&transaction, job_id, "completed", 1.0, None, &now())?;
        if let Some(publication_id) = publication_id.as_deref() {
            transaction
                .execute(
                    "DELETE FROM artifact_publications WHERE id = ?1",
                    [publication_id],
                )
                .map_err(|error| format!("Could not finish artifact publication: {error}"))?;
        }
        transaction.commit().map_err(|error| {
            if publication_id.is_some() {
                fs::remove_file(&audio_path).ok();
            }
            format!("Could not commit the generation: {error}")
        })?;
        drop(connection);
        self.clear_job_preview(job_id)?;
        Ok(history_value(
            &history_id,
            job_id,
            &title,
            voice,
            text,
            model_id,
            engine,
            generation_kind,
            &audio_path,
            sample_rate,
            duration,
            inference,
            rtf,
            vram,
            serde_json::from_str(&waveform).unwrap_or_else(|_| json!([])),
            &created_at,
            "verified",
            false,
            "",
            runtime_worker_state,
            end_to_end_seconds,
            runtime_overhead_seconds,
        ))
    }

    pub fn complete_transcription(
        &self,
        job_id: &str,
        audio_path: &str,
        original_audio_path: &str,
        processing: &Value,
        result: &Value,
    ) -> Result<Value, String> {
        let id = Uuid::new_v4().simple().to_string();
        let model_id = result.get("model_id").and_then(Value::as_str).unwrap_or("");
        let engine = result.get("engine").and_then(Value::as_str).unwrap_or("");
        let text = result.get("text").and_then(Value::as_str).unwrap_or("");
        let segments = result.get("segments").cloned().unwrap_or_else(|| json!([]));
        let words = result.get("words").cloned().unwrap_or_else(|| json!([]));
        let detected_language = result.get("detected_language").and_then(Value::as_str);
        let language_confidence = result
            .get("language_confidence")
            .and_then(Value::as_f64)
            .filter(|value| (0.0..=1.0).contains(value));
        let evidence = result.get("evidence").cloned().unwrap_or_else(|| {
            json!({
                "schema_version": 0,
                "timing_source": "unavailable",
                "language_source": "unavailable",
                "word_confidence_source": "unavailable"
            })
        });
        let duration = result
            .get("audio_duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let inference = result
            .get("inference_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let rtf = result.get("rtf").and_then(Value::as_f64).unwrap_or(0.0);
        let vram = result
            .get("vram_peak_mb")
            .and_then(numeric_value)
            .map(|value| value.round() as i64)
            .unwrap_or(0);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize transcription: {error}"))?;
        transaction.execute(
            "INSERT INTO transcriptions (id, job_id, source_path, model_id, engine, text, segments_json, audio_duration_seconds, inference_seconds, rtf, vram_peak_mb, created_at, original_source_path, processing_json, words_json, detected_language, language_confidence, evidence_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![id, job_id, audio_path, model_id, engine, text, segments.to_string(), duration, inference, rtf, vram, timestamp, original_audio_path, processing.to_string(), words.to_string(), detected_language, language_confidence, evidence.to_string()],
        ).map_err(|error| format!("Could not store transcription: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'completed', progress = 1, updated_at = ?2 WHERE id = ?1",
                params![job_id, now()],
            )
            .map_err(|error| format!("Could not complete transcription job: {error}"))?;
        insert_job_event(&transaction, job_id, "completed", 1.0, None, &now())?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit transcription: {error}"))?;
        Ok(json!({
            "id": id, "job_id": job_id, "source_path": audio_path, "model_id": model_id,
            "engine": engine, "text": text, "segments": segments,
            "audio_duration_seconds": duration, "inference_seconds": inference,
            "rtf": rtf, "vram_peak_mb": vram, "created_at": timestamp,
            "original_source_path": original_audio_path, "processing": processing,
            "words": words, "detected_language": detected_language,
            "language_confidence": language_confidence, "evidence": evidence
        }))
    }

    pub fn list_transcriptions(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT t.id, t.job_id, t.source_path, t.model_id, t.engine,
                    COALESCE(r.text, t.text), COALESCE(r.segments_json, t.segments_json),
                    t.audio_duration_seconds, t.inference_seconds, t.rtf, t.vram_peak_mb,
                    t.created_at, t.original_source_path, t.processing_json, t.words_json,
                    t.detected_language, t.language_confidence, t.evidence_json, t.text,
                    (SELECT COUNT(*) FROM transcription_revisions count_r WHERE count_r.transcription_id = t.id),
                    COALESCE(r.created_at, t.created_at)
             FROM transcriptions t
             LEFT JOIN transcription_revisions r ON r.id = (
                 SELECT latest.id FROM transcription_revisions latest
                 WHERE latest.transcription_id = t.id
                 ORDER BY latest.created_at DESC, latest.rowid DESC LIMIT 1
             )
             ORDER BY t.created_at DESC LIMIT 1000",
        ).map_err(|error| format!("Could not prepare transcriptions: {error}"))?;
        let rows = statement.query_map([], |row| {
            let segments: String = row.get(6)?;
            let words: String = row.get(14)?;
            let evidence: String = row.get(17)?;
            Ok(json!({
                "id": row.get::<_, String>(0)?, "job_id": row.get::<_, String>(1)?,
                "source_path": row.get::<_, String>(2)?, "model_id": row.get::<_, String>(3)?,
                "engine": row.get::<_, String>(4)?, "text": row.get::<_, String>(5)?,
                "segments": serde_json::from_str::<Value>(&segments).unwrap_or_else(|_| json!([])),
                "audio_duration_seconds": row.get::<_, f64>(7)?, "inference_seconds": row.get::<_, f64>(8)?,
                "rtf": row.get::<_, f64>(9)?, "vram_peak_mb": row.get::<_, i64>(10)?,
                "created_at": row.get::<_, String>(11)?,
                "original_source_path": row.get::<_, String>(12)?,
                "processing": serde_json::from_str::<Value>(&row.get::<_, String>(13)?).unwrap_or_else(|_| json!({})),
                "words": serde_json::from_str::<Value>(&words).unwrap_or_else(|_| json!([])),
                "detected_language": row.get::<_, Option<String>>(15)?,
                "language_confidence": row.get::<_, Option<f64>>(16)?,
                "evidence": serde_json::from_str::<Value>(&evidence).unwrap_or_else(|_| json!({
                    "schema_version": 0,
                    "timing_source": "unavailable",
                    "language_source": "unavailable",
                    "word_confidence_source": "unavailable"
                })),
                "original_text": row.get::<_, String>(18)?,
                "revision_count": row.get::<_, i64>(19)?,
                "updated_at": row.get::<_, String>(20)?,
            }))
        }).map_err(|error| format!("Could not list transcriptions: {error}"))?;
        let mut records = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read transcriptions: {error}"))?;
        drop(statement);
        for record in &mut records {
            let transcription_id = record["id"].as_str().unwrap_or_default().to_string();
            record["diarization"] =
                latest_transcription_diarization(&connection, &transcription_id)?
                    .unwrap_or(Value::Null);
            let revision = record["revision_count"].as_i64().unwrap_or(0);
            let text = record["text"].as_str().unwrap_or_default();
            record["alignment"] = latest_transcription_alignment(
                &connection,
                &transcription_id,
                revision,
                &sha256_bytes(text.as_bytes()),
            )?
            .unwrap_or(Value::Null);
        }
        Ok(records)
    }

    pub fn update_transcription(
        &self,
        transcription_id: &str,
        text: &str,
        segments: &Value,
    ) -> Result<Value, String> {
        let corrected_text = text.trim();
        if corrected_text.is_empty() {
            return Err("A corrected transcript cannot be empty".to_string());
        }
        if corrected_text.chars().count() > 500_000 {
            return Err("A corrected transcript is limited to 500,000 characters".to_string());
        }
        let corrected_segments = segments
            .as_array()
            .ok_or("Corrected transcript segments must be an array")?;

        let connection = self.lock()?;
        let original: Option<(String, String)> = connection
            .query_row(
                "SELECT text, segments_json FROM transcriptions WHERE id = ?1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the transcript: {error}"))?;
        let (original_text, original_segments_json) =
            original.ok_or("The transcript was not found")?;
        let original_segments: Value =
            serde_json::from_str(&original_segments_json).unwrap_or_else(|_| json!([]));
        let measured_segments = original_segments
            .as_array()
            .ok_or("The stored transcript timing evidence is invalid")?;
        if corrected_segments.len() != measured_segments.len() {
            return Err("Corrections must preserve every measured transcript segment".to_string());
        }
        for (index, (corrected, measured)) in corrected_segments
            .iter()
            .zip(measured_segments.iter())
            .enumerate()
        {
            let corrected_text = corrected
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Corrected segment {} has no text", index + 1))?;
            if corrected_text.chars().count() > 50_000 {
                return Err(format!("Corrected segment {} is too long", index + 1));
            }
            for key in ["start_seconds", "end_seconds"] {
                let corrected_time = corrected
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("Corrected segment {} has invalid timing", index + 1))?;
                let measured_time = measured
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite())
                    .ok_or("The stored transcript timing evidence is invalid")?;
                if (corrected_time - measured_time).abs() > 0.000_001 {
                    return Err(
                        "Transcript corrections cannot change measured timestamps".to_string()
                    );
                }
            }
        }

        let latest: Option<(String, String, String)> = connection
            .query_row(
                "SELECT text, segments_json, created_at FROM transcription_revisions WHERE transcription_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect transcript revisions: {error}"))?;
        let (current_text, current_segments, current_updated_at) =
            latest.unwrap_or((original_text, original_segments_json, now()));
        let corrected_segments_json = segments.to_string();
        if current_text == corrected_text && current_segments == corrected_segments_json {
            let revision_count: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM transcription_revisions WHERE transcription_id = ?1",
                    [transcription_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("Could not count transcript revisions: {error}"))?;
            return Ok(json!({
                "id": transcription_id,
                "text": corrected_text,
                "segments": segments,
                "revision_count": revision_count,
                "updated_at": current_updated_at
            }));
        }

        let timestamp = now();
        connection
            .execute(
                "INSERT INTO transcription_revisions (id, transcription_id, text, segments_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![Uuid::new_v4().simple().to_string(), transcription_id, corrected_text, corrected_segments_json, timestamp],
            )
            .map_err(|error| format!("Could not save transcript correction: {error}"))?;
        let revision_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM transcription_revisions WHERE transcription_id = ?1",
                [transcription_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not count transcript revisions: {error}"))?;
        Ok(json!({
            "id": transcription_id,
            "text": corrected_text,
            "segments": segments,
            "revision_count": revision_count,
            "updated_at": timestamp
        }))
    }

    pub fn transcription_diarization_request(
        &self,
        transcription_id: &str,
    ) -> Result<Value, String> {
        let connection = self.lock()?;
        let record: Option<(String, String, f64)> = connection
            .query_row(
                "SELECT source_path, words_json, audio_duration_seconds FROM transcriptions WHERE id = ?1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the transcript for speaker separation: {error}"))?;
        let (source_path, words_json, duration) = record.ok_or("The transcript was not found")?;
        drop(connection);
        let managed_path = self.transcription_audio_path(&source_path)?;
        let words = serde_json::from_str::<Value>(&words_json)
            .map_err(|_| "The transcript has invalid word evidence")?;
        if words.as_array().is_none_or(Vec::is_empty) {
            return Err("Speaker separation requires measured word timestamps".to_string());
        }
        Ok(json!({
            "transcription_id": transcription_id,
            "audio_path": managed_path,
            "audio_duration_seconds": duration,
            "words": words,
        }))
    }

    pub fn transcription_alignment_request(&self, transcription_id: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let record: Option<(String, String, String, i64, f64)> = connection
            .query_row(
                "SELECT t.source_path, COALESCE(r.text, t.text), COALESCE(r.segments_json, t.segments_json),
                        (SELECT COUNT(*) FROM transcription_revisions c WHERE c.transcription_id = t.id),
                        t.audio_duration_seconds
                 FROM transcriptions t
                 LEFT JOIN transcription_revisions r ON r.id = (
                    SELECT latest.id FROM transcription_revisions latest
                    WHERE latest.transcription_id = t.id
                    ORDER BY latest.created_at DESC, latest.rowid DESC LIMIT 1
                 ) WHERE t.id = ?1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the transcript for alignment: {error}"))?;
        let (source_path, text, segments_json, revision, duration) =
            record.ok_or("The transcript was not found")?;
        drop(connection);
        let managed_path = self.transcription_audio_path(&source_path)?;
        let segments = serde_json::from_str::<Value>(&segments_json)
            .map_err(|_| "The transcript has invalid segment evidence")?;
        if segments.as_array().is_none_or(Vec::is_empty) {
            return Err("Forced alignment requires measured transcript segments".to_string());
        }
        Ok(json!({
            "transcription_id": transcription_id,
            "audio_path": managed_path,
            "audio_duration_seconds": duration,
            "text": text,
            "segments": segments,
            "source_revision": revision,
            "source_text_sha256": sha256_bytes(text.as_bytes()),
        }))
    }

    pub fn complete_transcription_alignment(
        &self,
        job_id: &str,
        transcription_id: &str,
        result: &Value,
    ) -> Result<Value, String> {
        let model_id =
            required_trimmed(result, "model_id", "Alignment model evidence is required")?;
        let engine = required_trimmed(result, "engine", "Alignment engine evidence is required")?;
        let source_revision = result
            .get("source_revision")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .ok_or("Alignment source revision is invalid")?;
        let source_hash = required_trimmed(
            result,
            "source_text_sha256",
            "Alignment source hash is required",
        )?;
        if source_hash.len() != 64 || !source_hash.chars().all(|value| value.is_ascii_hexdigit()) {
            return Err("Alignment source hash is invalid".to_string());
        }
        let words = result
            .get("words")
            .and_then(Value::as_array)
            .ok_or("Aligned words must be an array")?;
        if words.is_empty() || words.len() > 500_000 {
            return Err("Forced alignment must report a bounded non-empty word list".to_string());
        }
        let evidence = result.get("evidence").cloned().unwrap_or_else(|| json!({}));
        if evidence
            .get("source_revision_linked")
            .and_then(Value::as_bool)
            != Some(true)
            || evidence.get("score_calibrated").and_then(Value::as_bool) != Some(false)
            || evidence
                .get("original_timestamps_preserved")
                .and_then(Value::as_bool)
                != Some(true)
            || evidence.get("provisional").and_then(Value::as_bool) != Some(true)
        {
            return Err("Alignment evidence must preserve source timing and disclose provisional uncalibrated scores".to_string());
        }

        let current = self.transcription_alignment_request(transcription_id)?;
        if current["source_revision"].as_i64() != Some(source_revision)
            || current["source_text_sha256"].as_str() != Some(source_hash)
        {
            return Err(
                "The transcript changed while alignment was running; run alignment again"
                    .to_string(),
            );
        }
        let duration = current["audio_duration_seconds"].as_f64().unwrap_or(0.0);
        let segments = current["segments"]
            .as_array()
            .ok_or("Current transcript segments are invalid")?;
        let expected_words = segments
            .iter()
            .enumerate()
            .flat_map(|(segment_index, segment)| {
                alignment_words(segment["text"].as_str().unwrap_or_default())
                    .into_iter()
                    .map(move |text| (segment_index, text))
            })
            .collect::<Vec<_>>();
        if words.len() != expected_words.len() {
            return Err(
                "Aligned words must exactly match the current transcript revision".to_string(),
            );
        }
        let mut previous_end = 0.0;
        for (word_index, word) in words.iter().enumerate() {
            let text = required_trimmed(word, "text", "Every aligned word requires text")?;
            if text.chars().count() > 500 {
                return Err("An aligned word is too long".to_string());
            }
            let start = word
                .get("start_seconds")
                .and_then(numeric_value)
                .filter(|value| value.is_finite())
                .ok_or("Aligned word start is invalid")?;
            let end = word
                .get("end_seconds")
                .and_then(numeric_value)
                .filter(|value| value.is_finite())
                .ok_or("Aligned word end is invalid")?;
            let score = word
                .get("alignment_score")
                .and_then(numeric_value)
                .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
                .ok_or("Aligned word score is invalid")?;
            let segment_index = word
                .get("segment_index")
                .and_then(Value::as_u64)
                .ok_or("Aligned word segment is invalid")? as usize;
            let segment = segments
                .get(segment_index)
                .ok_or("Aligned word segment is invalid")?;
            let segment_start = segment["start_seconds"]
                .as_f64()
                .ok_or("Transcript segment start is invalid")?;
            let segment_end = segment["end_seconds"]
                .as_f64()
                .ok_or("Transcript segment end is invalid")?;
            let (expected_segment, expected_text) = &expected_words[word_index];
            if start < previous_end - 0.000_001
                || end <= start
                || end > duration + 0.001
                || start < segment_start - 0.001
                || end > segment_end + 0.001
                || segment_index != *expected_segment
                || !text.eq_ignore_ascii_case(expected_text)
                || !score.is_finite()
            {
                return Err("Aligned words must be ordered inside measured segments".to_string());
            }
            previous_end = end;
        }
        let mean_score = result
            .get("mean_alignment_score")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or("Mean alignment score is invalid")?;
        let inference_seconds = result
            .get("inference_seconds")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0);
        let vram_peak_mb = result
            .get("vram_peak_mb")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0);
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize alignment: {error}"))?;
        transaction.execute(
            "INSERT INTO transcription_alignments (id, transcription_id, job_id, model_id, engine, source_revision, source_text_sha256, words_json, evidence_json, mean_alignment_score, inference_seconds, vram_peak_mb, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![id, transcription_id, job_id, model_id, engine, source_revision, source_hash, Value::Array(words.clone()).to_string(), evidence.to_string(), mean_score, inference_seconds, vram_peak_mb, timestamp],
        ).map_err(|error| format!("Could not store alignment evidence: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'completed', progress = 1, updated_at = ?2 WHERE id = ?1",
                params![job_id, timestamp],
            )
            .map_err(|error| format!("Could not complete the alignment job: {error}"))?;
        insert_job_event(&transaction, job_id, "completed", 1.0, None, &timestamp)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit alignment evidence: {error}"))?;
        drop(connection);
        let connection = self.lock()?;
        latest_transcription_alignment(&connection, transcription_id, source_revision, source_hash)?
            .ok_or("Stored alignment could not be reloaded".to_string())
    }

    pub fn complete_transcription_diarization(
        &self,
        job_id: &str,
        transcription_id: &str,
        result: &Value,
    ) -> Result<Value, String> {
        let model_id =
            required_trimmed(result, "model_id", "Diarization model evidence is required")?;
        let engine = required_trimmed(result, "engine", "Diarization engine evidence is required")?;
        let speakers = result
            .get("speakers")
            .and_then(Value::as_array)
            .ok_or("Diarization speakers must be an array")?;
        if speakers.is_empty() || speakers.len() > 8 {
            return Err("Diarization must report between 1 and 8 speakers".to_string());
        }
        let mut speaker_ids = Vec::with_capacity(speakers.len());
        for speaker in speakers {
            let id = required_trimmed(
                speaker,
                "id",
                "Every diarization speaker requires an identifier",
            )?;
            if !valid_speaker_id(id) || speaker_ids.iter().any(|known| known == id) {
                return Err(
                    "Diarization speaker identifiers must be unique speaker-N values".to_string(),
                );
            }
            speaker_ids.push(id.to_string());
        }
        let turns = result
            .get("turns")
            .and_then(Value::as_array)
            .ok_or("Diarization turns must be an array")?;
        if turns.is_empty() || turns.len() > 100_000 {
            return Err("Diarization must report a bounded non-empty turn list".to_string());
        }
        let evidence = result.get("evidence").cloned().unwrap_or_else(|| json!({}));
        if evidence.get("provisional").and_then(Value::as_bool) != Some(true)
            || evidence.get("overlap_detection").and_then(Value::as_bool) != Some(false)
        {
            return Err("Diarization evidence must disclose provisional clustering and unavailable overlap detection".to_string());
        }

        let connection = self.lock()?;
        let transcript: Option<(f64, String)> = connection
            .query_row(
                "SELECT audio_duration_seconds, words_json FROM transcriptions WHERE id = ?1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| {
                format!("Could not inspect diarization transcript evidence: {error}")
            })?;
        let (duration, words_json) = transcript.ok_or("The transcript was not found")?;
        let word_count = serde_json::from_str::<Value>(&words_json)
            .ok()
            .and_then(|value| value.as_array().map(Vec::len))
            .ok_or("The transcript has invalid word evidence")?;
        let mut previous_end = 0.0;
        let mut previous_word_end: Option<u64> = None;
        for turn in turns {
            let speaker_id = required_trimmed(
                turn,
                "speaker_id",
                "Every diarization turn requires a speaker",
            )?;
            if !speaker_ids.iter().any(|known| known == speaker_id) {
                return Err("A diarization turn references an unknown speaker".to_string());
            }
            let start = turn
                .get("start_seconds")
                .and_then(numeric_value)
                .filter(|value| value.is_finite())
                .ok_or("Diarization turn start is invalid")?;
            let end = turn
                .get("end_seconds")
                .and_then(numeric_value)
                .filter(|value| value.is_finite())
                .ok_or("Diarization turn end is invalid")?;
            if start < previous_end - 0.000_001 || end <= start || end > duration + 0.001 {
                return Err(
                    "Diarization turns must be ordered inside the source duration".to_string(),
                );
            }
            let word_start = turn
                .get("word_start_index")
                .and_then(Value::as_u64)
                .ok_or("Diarization word bounds are invalid")?;
            let word_end = turn
                .get("word_end_index")
                .and_then(Value::as_u64)
                .ok_or("Diarization word bounds are invalid")?;
            if word_start > word_end
                || word_end as usize >= word_count
                || previous_word_end.is_some_and(|value| word_start <= value)
            {
                return Err(
                    "Diarization word bounds must be ordered inside measured words".to_string(),
                );
            }
            if turn.get("confidence").is_some_and(|value| !value.is_null()) {
                return Err("This diarization adapter cannot report turn confidence".to_string());
            }
            if turn
                .get("text")
                .and_then(Value::as_str)
                .is_none_or(|text| text.chars().count() > 50_000)
            {
                return Err("Diarization turn text is invalid".to_string());
            }
            previous_end = end;
            previous_word_end = Some(word_end);
        }

        let inference_seconds = result
            .get("inference_seconds")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0);
        let vram_peak_mb = result
            .get("vram_peak_mb")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0);
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut connection = connection;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize diarization: {error}"))?;
        transaction.execute(
            "INSERT INTO transcription_diarizations (id, transcription_id, job_id, model_id, engine, speakers_json, turns_json, evidence_json, inference_seconds, vram_peak_mb, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![id, transcription_id, job_id, model_id, engine, Value::Array(speakers.clone()).to_string(), Value::Array(turns.clone()).to_string(), evidence.to_string(), inference_seconds, vram_peak_mb, timestamp],
        ).map_err(|error| format!("Could not store diarization evidence: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'completed', progress = 1, updated_at = ?2 WHERE id = ?1",
                params![job_id, timestamp],
            )
            .map_err(|error| format!("Could not complete the diarization job: {error}"))?;
        insert_job_event(&transaction, job_id, "completed", 1.0, None, &timestamp)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit diarization evidence: {error}"))?;
        drop(connection);
        let connection = self.lock()?;
        latest_transcription_diarization(&connection, transcription_id)?
            .ok_or("Stored diarization could not be reloaded".to_string())
    }

    pub fn update_transcription_speaker_labels(
        &self,
        transcription_id: &str,
        labels: &Value,
    ) -> Result<Value, String> {
        let labels = labels
            .as_object()
            .ok_or("Speaker labels must be an object")?;
        let connection = self.lock()?;
        let latest = latest_transcription_diarization(&connection, transcription_id)?
            .ok_or("Run speaker separation before editing labels")?;
        let speakers = latest["speakers"]
            .as_array()
            .ok_or("Stored diarization speakers are invalid")?;
        let known = speakers
            .iter()
            .filter_map(|speaker| speaker["id"].as_str())
            .collect::<Vec<_>>();
        if labels.len() != known.len() || labels.keys().any(|id| !known.contains(&id.as_str())) {
            return Err(
                "Speaker labels must name every speaker in the latest analysis".to_string(),
            );
        }
        let mut normalized = serde_json::Map::new();
        for id in known {
            let name = labels
                .get(id)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .ok_or("Speaker names cannot be empty")?;
            if name.chars().count() > 80 || name.chars().any(char::is_control) {
                return Err("Speaker names are limited to 80 printable characters".to_string());
            }
            normalized.insert(id.to_string(), json!(name));
        }
        let normalized = Value::Object(normalized);
        if latest.get("labels") == Some(&normalized) {
            return Ok(json!({
                "transcription_id": transcription_id,
                "labels": normalized,
                "label_revision_count": latest["label_revision_count"],
                "labels_updated_at": latest["labels_updated_at"]
            }));
        }
        let timestamp = now();
        connection.execute(
            "INSERT INTO transcription_speaker_label_revisions (id, transcription_id, diarization_id, labels_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().simple().to_string(), transcription_id, latest["id"].as_str().unwrap_or_default(), normalized.to_string(), timestamp],
        ).map_err(|error| format!("Could not save speaker labels: {error}"))?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM transcription_speaker_label_revisions WHERE transcription_id = ?1 AND diarization_id = ?2",
            params![transcription_id, latest["id"].as_str().unwrap_or_default()],
            |row| row.get(0),
        ).map_err(|error| format!("Could not count speaker-label revisions: {error}"))?;
        Ok(json!({
            "transcription_id": transcription_id,
            "labels": normalized,
            "label_revision_count": count,
            "labels_updated_at": timestamp
        }))
    }

    pub fn save_benchmark(&self, result: &Value) -> Result<Value, String> {
        let history_id = required_trimmed(
            result,
            "history_id",
            "Benchmark generation evidence is required",
        )?;
        let transcription_id = required_trimmed(
            result,
            "transcription_id",
            "Benchmark transcription evidence is required",
        )?;
        let connection = self.lock()?;
        let generation: Option<(String, String, String, String, f64, f64, f64, i64, String, String, f64, f64)> = connection
            .query_row(
                "SELECT h.audio_path, h.model_id, h.engine, h.text, h.duration_seconds, h.inference_seconds, h.rtf, h.vram_peak_mb, a.sha256, h.runtime_worker_state, h.end_to_end_seconds, h.runtime_overhead_seconds
                 FROM history h JOIN artifacts a ON a.id = h.artifact_id WHERE h.id = ?1",
                [history_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect benchmark generation evidence: {error}"))?;
        let transcription: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT source_path, model_id, engine, text FROM transcriptions WHERE id = ?1",
                [transcription_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| {
                format!("Could not inspect benchmark transcription evidence: {error}")
            })?;
        drop(connection);
        let (
            audio_path,
            model_id,
            engine,
            source_text,
            duration,
            inference,
            rtf,
            vram,
            source_sha256,
            runtime_worker_state,
            end_to_end_seconds,
            runtime_overhead_seconds,
        ) = generation.ok_or("The benchmark generation was not found")?;
        let (transcription_path, verifier_model_id, verifier_engine, transcript) =
            transcription.ok_or("The benchmark transcription was not found")?;
        let generated_bytes = self.generated_audio_bytes(&audio_path)?;
        let transcription_bytes = self.transcription_audio_bytes(&transcription_path)?;
        if sha256_bytes(&transcription_bytes) != source_sha256
            || transcription_bytes != generated_bytes
        {
            return Err(
                "Benchmark transcription must reference the exact generated artifact".to_string(),
            );
        }
        let source_words = normalize_metric_words(&source_text);
        let transcript_words = normalize_metric_words(&transcript);
        let word_errors = edit_distance(&source_words, &transcript_words);
        let source_characters = source_words.join("").chars().collect::<Vec<_>>();
        let transcript_characters = transcript_words.join("").chars().collect::<Vec<_>>();
        let character_errors = edit_distance(&source_characters, &transcript_characters);
        let word_error_rate =
            metric_error_rate(word_errors, source_words.len(), transcript_words.len());
        let character_error_rate = metric_error_rate(
            character_errors,
            source_characters.len(),
            transcript_characters.len(),
        );
        if !matches!(runtime_worker_state.as_str(), "cold" | "warm") {
            return Err("Benchmark generation has no native cold/warm evidence".to_string());
        }
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let payload = json!({
            "id": id, "history_id": history_id, "transcription_id": transcription_id,
            "model_id": model_id, "engine": engine, "inference_seconds": inference,
            "duration_seconds": duration, "rtf": rtf, "vram_mb": vram,
            "source_text": source_text, "transcript": transcript,
            "word_errors": word_errors, "reference_words": source_words.len(),
            "word_error_rate": word_error_rate,
            "character_errors": character_errors, "reference_characters": source_characters.len(),
            "character_error_rate": character_error_rate,
            "scoring_version": "soundar-unicode-v1", "source_sha256": source_sha256,
            "verifier_model_id": verifier_model_id, "verifier_engine": verifier_engine,
            "warm_state": runtime_worker_state,
            "end_to_end_seconds": end_to_end_seconds,
            "runtime_overhead_seconds": runtime_overhead_seconds,
            "model_revision": bounded_optional_text(result, "model_revision", 160)?,
            "gpu_name": bounded_optional_text(result, "gpu_name", 160)?,
            "driver_version": bounded_optional_text(result, "driver_version", 80)?,
            "app_version": bounded_optional_text(result, "app_version", 40)?,
            "created_at": timestamp,
        });
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO benchmark_runs (id, model_id, result_json, created_at, history_id, transcription_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, model_id, payload.to_string(), timestamp, history_id, transcription_id],
            )
            .map_err(|error| format!("Could not save benchmark run: {error}"))?;
        Ok(payload)
    }

    pub fn list_benchmarks(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT result_json FROM benchmark_runs ORDER BY created_at DESC LIMIT 1000")
            .map_err(|error| format!("Could not prepare benchmark runs: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                let payload: String = row.get(0)?;
                Ok(serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({})))
            })
            .map_err(|error| format!("Could not list benchmark runs: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read benchmark runs: {error}"))
    }

    pub fn list_history(&self, query: Option<&str>) -> Result<Vec<Value>, String> {
        self.list_history_filtered(query, None)
    }

    pub fn list_history_filtered(
        &self,
        query: Option<&str>,
        filters: Option<&Value>,
    ) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let search = format!("%{}%", query.unwrap_or("").trim().to_ascii_lowercase());
        let model_id = optional_history_filter(filters, "model_id", 256)?;
        let voice = optional_history_filter(filters, "voice", 256)?;
        let favorite = filters
            .and_then(|value| value.get("favorite"))
            .and_then(Value::as_bool)
            .map(i64::from);
        let artifact_filter = optional_history_filter(filters, "artifact_state", 32)?;
        if artifact_filter.as_deref().is_some_and(|value| {
            !matches!(value, "available" | "unavailable" | "missing" | "modified")
        }) {
            return Err("History artifact filter is invalid".to_string());
        }
        let mut statement = connection
            .prepare(
                "SELECT h.id, h.job_id, h.title, h.voice, h.text, h.model_id, h.engine, h.generation_kind, h.audio_path, h.sample_rate, h.duration_seconds, h.inference_seconds, h.rtf, h.vram_peak_mb, h.waveform_json, h.created_at, h.favorite, h.notes, h.runtime_worker_state, h.end_to_end_seconds, h.runtime_overhead_seconds, a.size_bytes
                 FROM history h JOIN artifacts a ON a.id = h.artifact_id
                 WHERE lower(title || ' ' || voice || ' ' || model_id || ' ' || generation_kind || ' ' || text || ' ' || notes) LIKE ?1
                 AND (?2 IS NULL OR h.model_id = ?2)
                 AND (?3 IS NULL OR h.voice = ?3)
                 AND (?4 IS NULL OR h.favorite = ?4)
                 ORDER BY h.created_at DESC",
            )
            .map_err(|error| format!("Could not prepare history search: {error}"))?;
        let rows = statement
            .query_map(params![search, model_id, voice, favorite], |row| {
                let path: String = row.get(8)?;
                let waveform: String = row.get(14)?;
                let artifact_state = artifact_file_state(Path::new(&path), row.get(21)?);
                Ok(history_value(
                    &row.get::<_, String>(0)?,
                    &row.get::<_, String>(1)?,
                    &row.get::<_, String>(2)?,
                    &row.get::<_, String>(3)?,
                    &row.get::<_, String>(4)?,
                    &row.get::<_, String>(5)?,
                    &row.get::<_, String>(6)?,
                    &row.get::<_, String>(7)?,
                    Path::new(&path),
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    serde_json::from_str(&waveform).unwrap_or_else(|_| json!([])),
                    &row.get::<_, String>(15)?,
                    &artifact_state,
                    row.get::<_, i64>(16)? != 0,
                    &row.get::<_, String>(17)?,
                    &row.get::<_, String>(18)?,
                    row.get(19)?,
                    row.get(20)?,
                ))
            })
            .map_err(|error| format!("Could not search history: {error}"))?;
        let mut history = Vec::new();
        for row in rows {
            let item = row.map_err(|error| format!("Could not read history: {error}"))?;
            let state = item
                .get("artifact_state")
                .and_then(Value::as_str)
                .unwrap_or("missing");
            let matches_artifact = match artifact_filter.as_deref() {
                None => true,
                Some("unavailable") => matches!(state, "missing" | "modified"),
                Some(expected) => state == expected,
            };
            if matches_artifact {
                history.push(item);
                if history.len() >= HISTORY_RESULT_LIMIT as usize {
                    break;
                }
            }
        }
        Ok(history)
    }

    pub fn get_history(&self, id: &str) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT h.id, h.job_id, h.title, h.voice, h.text, h.model_id, h.engine, h.generation_kind, h.audio_path, h.sample_rate, h.duration_seconds, h.inference_seconds, h.rtf, h.vram_peak_mb, h.waveform_json, h.created_at, h.favorite, h.notes, h.runtime_worker_state, h.end_to_end_seconds, h.runtime_overhead_seconds, a.size_bytes
                 FROM history h JOIN artifacts a ON a.id = h.artifact_id WHERE h.id = ?1",
                [id],
                |row| {
                    let path: String = row.get(8)?;
                    let waveform: String = row.get(14)?;
                    let artifact_state = artifact_file_state(Path::new(&path), row.get(21)?);
                    Ok(history_value(
                        &row.get::<_, String>(0)?, &row.get::<_, String>(1)?,
                        &row.get::<_, String>(2)?, &row.get::<_, String>(3)?,
                        &row.get::<_, String>(4)?, &row.get::<_, String>(5)?,
                        &row.get::<_, String>(6)?, &row.get::<_, String>(7)?,
                        Path::new(&path), row.get(9)?, row.get(10)?, row.get(11)?,
                        row.get(12)?, row.get(13)?,
                        serde_json::from_str(&waveform).unwrap_or_else(|_| json!([])),
                        &row.get::<_, String>(15)?, &artifact_state,
                        row.get::<_, i64>(16)? != 0, &row.get::<_, String>(17)?,
                        &row.get::<_, String>(18)?, row.get(19)?, row.get(20)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("Could not read history: {error}"))
    }

    /// Resolve the immutable History artifact produced by one exact synthesis job.
    /// Durable composite workflows use this to adopt a completed child after restart instead of
    /// generating duplicate speech in the create-child/checkpoint crash window.
    pub fn get_history_by_job_id(&self, job_id: &str) -> Result<Option<Value>, String> {
        let history_id = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT id FROM history WHERE job_id = ?1 LIMIT 1",
                    [job_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("Could not resolve synthesis History: {error}"))?
        };
        history_id
            .as_deref()
            .map(|history_id| self.get_history(history_id))
            .transpose()
            .map(Option::flatten)
    }

    /// Resolve an audio path only when it is an intact, registered soundAr history artifact.
    ///
    /// Video Studio agent tools accept references to existing soundAr speech/music, but must not
    /// turn an assistant-supplied filesystem path into an arbitrary local-file read. Canonicalizing
    /// into the managed artifact root, binding the path to both `history` and `artifacts`, and
    /// checking the recorded digest keeps that boundary identical to normal History playback.
    pub fn get_registered_history_by_audio_path(
        &self,
        raw_path: &str,
    ) -> Result<Option<Value>, String> {
        let audio_path = self.validate_artifact_path(raw_path)?;
        let canonical_path = audio_path.to_string_lossy().to_string();
        let record: Option<(String, i64, String)> = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT h.id, a.size_bytes, a.sha256
                     FROM history h JOIN artifacts a ON a.id = h.artifact_id
                     WHERE h.audio_path IN (?1, ?2) AND a.path IN (?1, ?2)
                     LIMIT 1",
                    params![raw_path, canonical_path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|error| format!("Could not verify the soundAr audio artifact: {error}"))?
        };
        let Some((history_id, expected_size, expected_sha256)) = record else {
            return Ok(None);
        };
        let metadata = fs::metadata(&audio_path)
            .map_err(|error| format!("Could not inspect the soundAr audio artifact: {error}"))?;
        if metadata.len() as i64 != expected_size || sha256_file(&audio_path)? != expected_sha256 {
            return Err(
                "The registered soundAr audio changed on disk and cannot be used by Video Studio"
                    .to_string(),
            );
        }
        self.get_history(&history_id)
    }

    /// Resolve a managed Video Studio path only when it is a ready, integrity-bound audio
    /// asset or output. Directory membership alone is not registration: assistant tools must
    /// never gain arbitrary local-file access merely because a file was placed below `video/`.
    pub fn get_registered_video_audio_by_path(
        &self,
        raw_path: &str,
    ) -> Result<Option<Value>, String> {
        let audio_path = self.validate_artifact_path(raw_path)?;
        let canonical_path = audio_path.to_string_lossy().to_string();
        let record: Option<(String, String, String, String, Option<i64>, Option<String>)> = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT project_id, id, kind, mime_type, size_bytes, content_sha256
                     FROM video_media_assets
                     WHERE local_path IN (?1, ?2) AND status = 'ready'
                       AND mime_type LIKE 'audio/%'
                     UNION ALL
                     SELECT project_id, id, kind, mime_type, size_bytes, sha256
                     FROM video_output_records
                     WHERE artifact_path IN (?1, ?2) AND status = 'ready'
                       AND mime_type LIKE 'audio/%'
                     LIMIT 1",
                    params![raw_path, canonical_path],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| {
                    format!("Could not verify the Video Studio audio artifact: {error}")
                })?
        };
        let Some((project_id, id, kind, mime_type, expected_size, expected_sha256)) = record else {
            return Ok(None);
        };
        let expected_size = expected_size
            .filter(|value| *value >= 0)
            .ok_or("The registered Video Studio audio has no trusted size and cannot be reused")?
            as u64;
        let expected_sha256 = expected_sha256.filter(|value| value.len() == 64).ok_or(
            "The registered Video Studio audio has no trusted checksum and cannot be reused",
        )?;
        let metadata = fs::metadata(&audio_path)
            .map_err(|error| format!("Could not inspect the Video Studio audio: {error}"))?;
        if !metadata.is_file()
            || metadata.len() != expected_size
            || sha256_file(&audio_path)? != expected_sha256
        {
            return Err(
                "The registered Video Studio audio changed on disk and cannot be reused"
                    .to_string(),
            );
        }
        Ok(Some(json!({
            "id": id,
            "project_id": project_id,
            "kind": kind,
            "mime_type": mime_type,
            "local_path": canonical_path,
            "size_bytes": expected_size,
            "sha256": expected_sha256,
        })))
    }

    pub fn delete_history(&self, id: &str, delete_audio: bool) -> Result<bool, String> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not delete history: {error}"))?;
        let record: Option<(String, String)> = transaction
            .query_row(
                "SELECT artifact_id, audio_path FROM history WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not find the history record: {error}"))?;
        let Some((artifact_id, raw_path)) = record else {
            return Ok(false);
        };
        transaction
            .execute("DELETE FROM history WHERE id = ?1", [id])
            .map_err(|error| format!("Could not delete the history record: {error}"))?;
        if delete_audio {
            let path = self.validate_artifact_path_allow_missing(&raw_path)?;
            if path.is_file() {
                fs::remove_file(&path)
                    .map_err(|error| format!("Could not delete generated audio: {error}"))?;
            }
            transaction
                .execute("DELETE FROM artifacts WHERE id = ?1", [artifact_id])
                .map_err(|error| format!("Could not delete the artifact record: {error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit history deletion: {error}"))?;
        Ok(true)
    }

    pub fn generated_audio_bytes(&self, raw_path: &str) -> Result<Vec<u8>, String> {
        let audio_path = self.validate_artifact_path(raw_path)?;
        let stored_path = audio_path.to_string_lossy().to_string();
        let connection = self.lock()?;
        let artifact: Option<(i64, String)> = connection
            .query_row(
                "SELECT size_bytes, sha256 FROM artifacts WHERE path IN (?1, ?2)",
                params![raw_path, stored_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not verify generated audio: {error}"))?;
        drop(connection);
        let Some((expected_size, expected_checksum)) = artifact else {
            return Err("Playback is restricted to recorded soundAr artifacts".to_string());
        };
        let bytes = fs::read(&audio_path)
            .map_err(|error| format!("Could not read generated audio: {error}"))?;
        if bytes.len() as i64 != expected_size {
            return Err(
                "Generated audio changed on disk and cannot be trusted for playback".to_string(),
            );
        }
        if sha256_bytes(&bytes) != expected_checksum {
            return Err(
                "Generated audio failed its checksum and cannot be trusted for playback"
                    .to_string(),
            );
        }
        Ok(bytes)
    }

    pub fn duplicate_history(&self, id: &str) -> Result<Value, String> {
        let original = self
            .get_history(id)?
            .ok_or_else(|| "The history record was not found".to_string())?;
        let request = self.history_request(id)?;
        let source = original
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or("The history record has no audio artifact")?;
        let bytes = self.generated_audio_bytes(source)?;
        let extension = Path::new(source)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| matches!(*value, "wav" | "flac"))
            .ok_or("Only WAV and FLAC history artifacts can be duplicated")?;
        let history_id = Uuid::new_v4().simple().to_string();
        let job_id = Uuid::new_v4().simple().to_string();
        let artifact_id = Uuid::new_v4().simple().to_string();
        let destination = self
            .artifacts_root
            .join(format!("duplicate-{history_id}.{extension}"));
        let temporary = destination.with_extension(format!("{extension}.partial"));
        fs::write(&temporary, &bytes)
            .map_err(|error| format!("Could not stage the duplicate artifact: {error}"))?;
        fs::rename(&temporary, &destination).map_err(|error| {
            fs::remove_file(&temporary).ok();
            format!("Could not publish the duplicate artifact: {error}")
        })?;
        let timestamp = now();
        let title = format!(
            "{} copy",
            original
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Generation")
        );
        let mut duplicate_request = request;
        duplicate_request["title"] = json!(title);
        duplicate_request["duplicated_from_history_id"] = json!(id);
        let result = (|| -> Result<Value, String> {
            let mut connection = self.lock()?;
            let transaction = connection
                .transaction()
                .map_err(|error| format!("Could not duplicate history: {error}"))?;
            transaction.execute(
                "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at)
                 VALUES (?1, 'artifact-copy', 'completed', ?2, 1, 1, 1, ?3, ?3)",
                params![job_id, duplicate_request.to_string(), timestamp],
            ).map_err(|error| format!("Could not store the duplicate job: {error}"))?;
            insert_job_event(&transaction, &job_id, "completed", 1.0, None, &timestamp)?;
            transaction.execute(
                "INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![artifact_id, job_id, destination.to_string_lossy(), extension, bytes.len() as i64, sha256_bytes(&bytes), timestamp],
            ).map_err(|error| format!("Could not store the duplicate artifact: {error}"))?;
            transaction.execute(
                "INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, generation_kind, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at, favorite, notes)
                 SELECT ?1, ?2, ?3, ?4, voice, text, model_id, engine, generation_kind, ?5, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, ?6, 0, notes FROM history WHERE id = ?7",
                params![history_id, job_id, artifact_id, title, destination.to_string_lossy(), timestamp, id],
            ).map_err(|error| format!("Could not store the duplicate history record: {error}"))?;
            transaction
                .execute(
                    "UPDATE jobs SET output_artifact_id = ?2 WHERE id = ?1",
                    params![job_id, artifact_id],
                )
                .map_err(|error| format!("Could not link the duplicate artifact: {error}"))?;
            transaction
                .commit()
                .map_err(|error| format!("Could not commit the duplicate: {error}"))?;
            drop(connection);
            self.get_history(&history_id)?
                .ok_or_else(|| "The duplicate history record was not found".to_string())
        })();
        if result.is_err() {
            fs::remove_file(destination).ok();
        }
        result
    }

    pub fn export_history(&self, id: &str, raw_destination: &str) -> Result<Value, String> {
        let original = self
            .get_history(id)?
            .ok_or_else(|| "The history record was not found".to_string())?;
        let source = original
            .get("audio_path")
            .and_then(Value::as_str)
            .ok_or("The history record has no audio artifact")?;
        let bytes = self.generated_audio_bytes(source)?;
        let destination = PathBuf::from(raw_destination);
        if !destination.is_absolute() {
            return Err("Choose an absolute export path".to_string());
        }
        if destination.exists() {
            return Err(
                "The export destination already exists; choose another filename".to_string(),
            );
        }
        let source_extension = Path::new(source)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let destination_extension = destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if destination_extension != source_extension
            || !matches!(source_extension.as_str(), "wav" | "flac")
        {
            return Err(format!(
                "Export this artifact as .{source_extension} to preserve its original encoding"
            ));
        }
        let parent = destination
            .parent()
            .ok_or("The export destination has no parent directory")?
            .canonicalize()
            .map_err(|error| format!("Could not access the export directory: {error}"))?;
        if !parent.is_dir() {
            return Err("The export destination parent is not a directory".to_string());
        }
        let filename = destination
            .file_name()
            .ok_or("The export destination has no filename")?;
        let destination = parent.join(filename);
        let temporary = parent.join(format!(
            ".soundar-export-{}.partial",
            Uuid::new_v4().simple()
        ));
        fs::write(&temporary, &bytes)
            .map_err(|error| format!("Could not stage the exported audio: {error}"))?;
        fs::hard_link(&temporary, &destination).map_err(|error| {
            fs::remove_file(&temporary).ok();
            format!(
                "Could not publish the exported audio without overwriting another file: {error}"
            )
        })?;
        fs::remove_file(&temporary).ok();
        let receipt = json!({
            "id": Uuid::new_v4().simple().to_string(), "history_id": id,
            "path": destination.to_string_lossy(), "format": source_extension,
            "size_bytes": bytes.len(), "sha256": sha256_bytes(&bytes), "created_at": now(),
        });
        let connection = self.lock()?;
        if let Err(error) = connection.execute(
            "INSERT INTO history_exports (id, history_id, destination_path, format, size_bytes, sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![receipt["id"].as_str(), id, receipt["path"].as_str(), receipt["format"].as_str(), bytes.len() as i64, receipt["sha256"].as_str(), receipt["created_at"].as_str()],
        ) {
            fs::remove_file(&destination).ok();
            return Err(format!("Could not record the export receipt: {error}"));
        }
        Ok(receipt)
    }

    pub fn list_jobs(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, kind, status, progress, attempt, error, created_at, updated_at, request_json, priority,
                        preview_audio_path, preview_duration_seconds, first_audio_seconds
                 FROM jobs WHERE dismissed = 0 ORDER BY created_at DESC LIMIT 100",
            )
            .map_err(|error| format!("Could not prepare the job list: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                let request_json: String = row.get(8)?;
                let request = serde_json::from_str::<Value>(&request_json).unwrap_or_else(|_| json!({}));
                let status = row.get::<_, String>(2)?;
                let progress = row.get::<_, f64>(3)?;
                let stage = if status == "queued" {
                    "queued"
                } else if status == "completed" {
                    "completed"
                } else if progress < 0.12 {
                    "preparing"
                } else if progress < 0.30 {
                    "planning"
                } else if progress < 0.78 {
                    "rendering"
                } else if progress < 0.94 {
                    "decoding"
                } else {
                    "finalizing"
                };
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "kind": row.get::<_, String>(1)?,
                    "status": status,
                    "progress": progress,
                    "stage": stage,
                    "attempt": row.get::<_, i64>(4)?,
                    "error": row.get::<_, Option<String>>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                    "updated_at": row.get::<_, String>(7)?,
                    "title": request.get("title").and_then(Value::as_str).or_else(|| request.get("text").and_then(Value::as_str)).or_else(|| request.get("prompt").and_then(Value::as_str)).unwrap_or("Untitled task"),
                    "model_id": request.get("model_id").and_then(Value::as_str),
                    "priority": priority_name(row.get::<_, i64>(9)?),
                    "preview_audio_path": row.get::<_, Option<String>>(10)?,
                    "preview_duration_seconds": row.get::<_, Option<f64>>(11)?,
                    "first_audio_seconds": row.get::<_, Option<f64>>(12)?,
                }))
            })
            .map_err(|error| format!("Could not list jobs: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read jobs: {error}"))
    }

    pub fn get_job(&self, id: &str) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT j.id, j.kind, j.status, j.progress, j.attempt, j.error, j.created_at, j.updated_at, j.request_json,
                        a.format, a.size_bytes, a.sha256, h.id, j.priority,
                        j.preview_audio_path, j.preview_duration_seconds, j.first_audio_seconds
                 FROM jobs j
                 LEFT JOIN artifacts a ON a.id = j.output_artifact_id
                 LEFT JOIN history h ON h.job_id = j.id
                 WHERE j.id = ?1",
                [id],
                |row| {
                    let request_json: String = row.get(8)?;
                    let request = serde_json::from_str::<Value>(&request_json).unwrap_or_else(|_| json!({}));
                    let format = row.get::<_, Option<String>>(9)?;
                    let size_bytes = row.get::<_, Option<i64>>(10)?;
                    let sha256 = row.get::<_, Option<String>>(11)?;
                    let history_id = row.get::<_, Option<String>>(12)?;
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "object": "job",
                        "kind": row.get::<_, String>(1)?,
                        "status": row.get::<_, String>(2)?,
                        "progress": row.get::<_, f64>(3)?,
                        "attempt": row.get::<_, i64>(4)?,
                        "error": row.get::<_, Option<String>>(5)?,
                        "created_at": row.get::<_, String>(6)?,
                        "updated_at": row.get::<_, String>(7)?,
                        "title": request.get("title").and_then(Value::as_str).or_else(|| request.get("text").and_then(Value::as_str)).or_else(|| request.get("prompt").and_then(Value::as_str)).unwrap_or("Untitled task"),
                        "model_id": request.get("model_id").and_then(Value::as_str),
                        "priority": priority_name(row.get::<_, i64>(13)?),
                        "preview_audio_path": row.get::<_, Option<String>>(14)?,
                        "preview_duration_seconds": row.get::<_, Option<f64>>(15)?,
                        "first_audio_seconds": row.get::<_, Option<f64>>(16)?,
                        "result": format.map(|format| json!({
                            "format": format,
                            "size_bytes": size_bytes.unwrap_or_default(),
                            "sha256": sha256,
                            "history_id": history_id,
                            "audio_url": format!("/v1/jobs/{id}/audio"),
                        })),
                    }))
                },
            )
            .optional()
            .map_err(|error| format!("Could not read the job: {error}"))
    }

    /// Lists active synthesis/service children that are durably bound to one composite Video
    /// Studio parent. This is deliberately narrow: cancellation must never fan out based on an
    /// arbitrary user-supplied job field or cancel unrelated audio generation.
    pub fn active_video_child_jobs(
        &self,
        parent_job_id: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, kind, request_json FROM jobs
                 WHERE kind IN ('synthesis', 'video_replace_narration', 'video_import_local')
                   AND status IN ('queued', 'preparing', 'running')
                 ORDER BY created_at, id",
            )
            .map_err(|error| format!("Could not prepare child task lookup: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("Could not inspect child tasks: {error}"))?;
        let mut children = Vec::new();
        for row in rows {
            let (id, kind, request_json) =
                row.map_err(|error| format!("Could not read child task: {error}"))?;
            let request = serde_json::from_str::<Value>(&request_json)
                .map_err(|error| format!("Stored child task request is invalid: {error}"))?;
            let bound_parent = request
                .get("parent_job_id")
                .or_else(|| request.get("video_parent_job_id"))
                .and_then(Value::as_str);
            if bound_parent == Some(parent_job_id) {
                children.push((id, kind));
            }
        }
        Ok(children)
    }

    /// Finds the newest durable child of a composite Video Studio job, including a child that
    /// already completed. Composite runners use this before rechecking their original project
    /// expectation so a crash after the child committed cannot turn a successful edit into an
    /// unrecoverable stale-parent failure.
    pub fn video_child_job(
        &self,
        parent_job_id: &str,
        child_kind: &str,
    ) -> Result<Option<(String, String)>, String> {
        if !matches!(
            child_kind,
            "synthesis" | "video_replace_narration" | "video_import_local"
        ) {
            return Err("video.invalid_child_kind: Unsupported composite video child kind".into());
        }
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, status, request_json FROM jobs
                 WHERE kind = ?1
                 ORDER BY created_at DESC, id DESC",
            )
            .map_err(|error| format!("Could not prepare durable child lookup: {error}"))?;
        let rows = statement
            .query_map([child_kind], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("Could not inspect durable child tasks: {error}"))?;
        for row in rows {
            let (id, status, request_json) =
                row.map_err(|error| format!("Could not read a durable child task: {error}"))?;
            let request = serde_json::from_str::<Value>(&request_json)
                .map_err(|error| format!("Stored child task request is invalid: {error}"))?;
            let bound_parent = request
                .get("parent_job_id")
                .or_else(|| request.get("video_parent_job_id"))
                .and_then(Value::as_str);
            if bound_parent == Some(parent_job_id) {
                return Ok(Some((id, status)));
            }
        }
        Ok(None)
    }

    /// Returns the newest unfinished/recoverable durable Video Studio job bound to a project.
    /// Requests use either a top-level project_id or the canonical timeline batch `base` object;
    /// no filename/path heuristic is accepted as ownership evidence.
    pub fn latest_video_project_job(&self, project_id: &str) -> Result<Option<Value>, String> {
        let candidate_id = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT id FROM jobs
                     WHERE kind IN (
                        'video_analyze', 'video_plan', 'video_regenerate_narration',
                        'video_create_from_prompt',
                        'video_import_local', 'video_import_link',
                        'video_render_preview', 'video_render_final',
                        'video_render_timeline_preview', 'video_render_timeline_final',
                        'video_render_timeline_batch_preview', 'video_render_timeline_batch_final',
                        'video_replace_narration', 'video_publish_package'
                     )
                       AND status IN ('queued', 'preparing', 'running', 'failed', 'cancelled')
                       AND CASE WHEN json_valid(request_json)
                           THEN COALESCE(
                               json_extract(request_json, '$.project_id'),
                               json_extract(request_json, '$.base.project_id')
                           )
                           ELSE NULL
                       END = ?1
                       AND NOT (
                           kind IN ('video_import_local', 'video_replace_narration')
                           AND CASE WHEN json_valid(request_json)
                               THEN json_extract(request_json, '$.parent_job_id')
                               ELSE NULL
                           END IS NOT NULL
                           AND EXISTS (
                               SELECT 1 FROM jobs AS parent
                               WHERE parent.id = CASE WHEN json_valid(jobs.request_json)
                                   THEN json_extract(jobs.request_json, '$.parent_job_id')
                                   ELSE NULL
                               END
                                 AND parent.status IN ('queued', 'preparing', 'running', 'failed', 'cancelled')
                           )
                       )
                     ORDER BY updated_at DESC, created_at DESC, id DESC
                     LIMIT 1",
                    [project_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("Could not inspect project task recovery: {error}"))?
        };
        candidate_id
            .map(|id| {
                self.get_job(&id)?.ok_or_else(|| {
                    "video.job_not_found: The recoverable project task disappeared".into()
                })
            })
            .transpose()
    }

    pub fn job_events_since(&self, id: &str, after: i64) -> Result<Option<Vec<Value>>, String> {
        let connection = self.lock()?;
        let exists = connection
            .query_row("SELECT 1 FROM jobs WHERE id = ?1", [id], |_| Ok(()))
            .optional()
            .map_err(|error| format!("Could not inspect the job event stream: {error}"))?
            .is_some();
        if !exists {
            return Ok(None);
        }
        let mut statement = connection
            .prepare(
                "SELECT rowid, status, progress, error, created_at FROM job_events WHERE job_id = ?1 AND rowid > ?2 ORDER BY rowid LIMIT 100",
            )
            .map_err(|error| format!("Could not prepare the job event stream: {error}"))?;
        let events = statement
            .query_map(params![id, after], |row| {
                Ok(json!({
                    "sequence": row.get::<_, i64>(0)?,
                    "job_id": id,
                    "status": row.get::<_, String>(1)?,
                    "progress": row.get::<_, f64>(2)?,
                    "error": row.get::<_, Option<String>>(3)?,
                    "created_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|error| format!("Could not read job events: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not decode job events: {error}"))?;
        Ok(Some(events))
    }

    pub fn generated_audio_for_job(&self, id: &str) -> Result<(Vec<u8>, String), String> {
        let (path, format) = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT a.path, a.format FROM jobs j JOIN artifacts a ON a.id = j.output_artifact_id WHERE j.id = ?1 AND j.status = 'completed'",
                    [id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(|error| format!("Could not locate the job artifact: {error}"))?
                .ok_or("The job has no completed audio artifact")?
        };
        Ok((self.generated_audio_bytes(&path)?, format))
    }

    pub fn retry_synthesis_job(&self, id: &str) -> Result<(Value, Value), String> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not retry the task: {error}"))?;
        let job: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT kind, status, request_json FROM jobs WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the task: {error}"))?;
        let Some((kind, status, request_json)) = job else {
            return Err("The selected task was not found".to_string());
        };
        if !matches!(
            kind.as_str(),
            "synthesis" | "api-synthesis" | "music-generation"
        ) {
            return Err("This task must be retried from its owning workflow".to_string());
        }
        if !matches!(status.as_str(), "failed" | "cancelled") {
            return Err(
                "Only failed or cancelled audio generation tasks can be retried".to_string(),
            );
        }
        let request = serde_json::from_str::<Value>(&request_json)
            .map_err(|error| format!("The stored task request is invalid: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'preparing', progress = 0.05, attempt = attempt + 1, error = NULL, output_artifact_id = NULL, dismissed = 0, updated_at = ?2 WHERE id = ?1",
                params![id, now()],
            )
            .map_err(|error| format!("Could not prepare the task retry: {error}"))?;
        insert_job_event(&transaction, id, "preparing", 0.05, None, &now())?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the task retry: {error}"))?;
        drop(connection);
        let job = self
            .list_jobs()?
            .into_iter()
            .find(|job| job.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| "The retried task disappeared".to_string())?;
        Ok((job, request))
    }

    /// Atomically re-arms a durable Video Studio job and returns its original request.
    ///
    /// Callers provide the exact workflow kinds they know how to reconstruct. This keeps the
    /// generic job store from silently retrying a task through the wrong runner while preserving
    /// the original id, attempt history, and durable event stream.
    pub fn resume_video_job(
        &self,
        id: &str,
        allowed_kinds: &[&str],
    ) -> Result<(Value, Value), String> {
        if allowed_kinds.is_empty() {
            return Err("No resumable video workflow kinds were provided".to_string());
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not resume the video task: {error}"))?;
        let job: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT kind, status, request_json FROM jobs WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the video task: {error}"))?;
        let Some((kind, status, request_json)) = job else {
            return Err("The selected video task was not found".to_string());
        };
        if !allowed_kinds.iter().any(|allowed| *allowed == kind) {
            return Err("This task must be resumed from its owning workflow".to_string());
        }
        if !matches!(status.as_str(), "failed" | "cancelled") {
            return Err("Only failed, interrupted, or cancelled video tasks can be resumed".into());
        }
        let request = serde_json::from_str::<Value>(&request_json)
            .map_err(|error| format!("The stored video task request is invalid: {error}"))?;
        let timestamp = now();
        transaction
            .execute(
                "UPDATE jobs
                 SET status = 'preparing', progress = 0.05, attempt = attempt + 1,
                     error = NULL, output_artifact_id = NULL, dismissed = 0, updated_at = ?2
                 WHERE id = ?1 AND status IN ('failed', 'cancelled')",
                params![id, timestamp],
            )
            .map_err(|error| format!("Could not prepare the video task resume: {error}"))?;
        insert_job_event(&transaction, id, "preparing", 0.05, None, &timestamp)?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the video task resume: {error}"))?;
        drop(connection);
        let job = self
            .list_jobs()?
            .into_iter()
            .find(|job| job.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| "The resumed video task disappeared".to_string())?;
        Ok((job, request))
    }

    pub fn clear_finished_jobs(&self) -> Result<usize, String> {
        let connection = self.lock()?;
        connection
            .execute(
                "UPDATE jobs SET dismissed = 1, updated_at = ?1 WHERE dismissed = 0 AND status IN ('completed', 'failed', 'cancelled')",
                [now()],
            )
            .map_err(|error| format!("Could not clear finished tasks: {error}"))
    }

    pub fn create_batch(&self, request: &Value) -> Result<Value, String> {
        let normalized = normalize_batch_rows(request)?;
        self.validate_batch_voice_references(request, &normalized)?;
        let default_priority = priority_value(request.get("priority"))?;
        let id = Uuid::new_v4().simple().to_string();
        let name = request
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Untitled batch");
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not create the batch: {error}"))?;
        transaction
            .execute(
                "INSERT INTO batch_runs (id, name, status, total_items, completed_items, failed_items, request_json, error, priority, created_at, updated_at) VALUES (?1, ?2, 'queued', ?3, 0, 0, ?4, NULL, ?5, ?6, ?6)",
                params![id, name, normalized.len() as i64, request.to_string(), default_priority, timestamp],
            )
            .map_err(|error| format!("Could not store the batch: {error}"))?;
        for (index, row) in normalized.iter().enumerate() {
            let row_priority = priority_value(row.get("priority"))?;
            transaction
                .execute(
                    "INSERT INTO batch_items (id, batch_id, item_index, text, name, settings_json, output_name, priority, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9, ?9)",
                    params![Uuid::new_v4().simple().to_string(), id, index as i64, row["text"].as_str(), row["name"].as_str(), row["settings"].to_string(), row["output_name"].as_str(), row_priority, timestamp],
                )
                .map_err(|error| format!("Could not store batch item {}: {error}", index + 1))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the batch: {error}"))?;
        drop(connection);
        self.get_batch(&id)?
            .ok_or_else(|| "The batch disappeared after creation".to_string())
    }

    pub fn create_idempotent_batch(
        &self,
        idempotency_key: &str,
        request: &Value,
    ) -> Result<Option<(Value, bool)>, String> {
        let normalized = normalize_batch_rows(request)?;
        self.validate_batch_voice_references(request, &normalized)?;
        let default_priority = priority_value(request.get("priority"))?;
        let request_json = request.to_string();
        let request_sha256 = sha256_bytes(request_json.as_bytes());
        let id = Uuid::new_v4().simple().to_string();
        let name = request
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Untitled batch");
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not create the idempotent batch: {error}"))?;
        let existing = transaction
            .query_row(
                "SELECT request_sha256, batch_id FROM api_batch_submissions WHERE idempotency_key = ?1",
                [idempotency_key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the batch idempotency key: {error}"))?;
        if let Some((existing_sha256, batch_id)) = existing {
            drop(transaction);
            drop(connection);
            return if existing_sha256 == request_sha256 {
                Ok(self.get_batch(&batch_id)?.map(|batch| (batch, false)))
            } else {
                Ok(None)
            };
        }
        transaction
            .execute(
                "INSERT INTO batch_runs (id, name, status, total_items, completed_items, failed_items, request_json, error, priority, created_at, updated_at) VALUES (?1, ?2, 'queued', ?3, 0, 0, ?4, NULL, ?5, ?6, ?6)",
                params![id, name, normalized.len() as i64, request_json, default_priority, timestamp],
            )
            .map_err(|error| format!("Could not store the idempotent batch: {error}"))?;
        for (index, row) in normalized.iter().enumerate() {
            let row_priority = priority_value(row.get("priority"))?;
            transaction
                .execute(
                    "INSERT INTO batch_items (id, batch_id, item_index, text, name, settings_json, output_name, priority, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9, ?9)",
                    params![Uuid::new_v4().simple().to_string(), id, index as i64, row["text"].as_str(), row["name"].as_str(), row["settings"].to_string(), row["output_name"].as_str(), row_priority, timestamp],
                )
                .map_err(|error| format!("Could not store batch item {}: {error}", index + 1))?;
        }
        transaction
            .execute(
                "INSERT INTO api_batch_submissions (idempotency_key, request_sha256, batch_id, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![idempotency_key, request_sha256, id, timestamp],
            )
            .map_err(|error| format!("Could not record the idempotent batch: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the idempotent batch: {error}"))?;
        drop(connection);
        Ok(self.get_batch(&id)?.map(|batch| (batch, true)))
    }

    pub fn update_batch_item(
        &self,
        batch_id: &str,
        item_index: i64,
        status: &str,
        history_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<Value, String> {
        if !matches!(status, "running" | "completed" | "failed" | "cancelled") {
            return Err(format!("Invalid batch item status: {status}"));
        }
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|cause| format!("Could not update the batch: {cause}"))?;
        let changed = transaction
            .execute(
                "UPDATE batch_items SET status = ?3, history_id = COALESCE(?4, history_id), error = ?5, updated_at = ?6 WHERE batch_id = ?1 AND item_index = ?2",
                params![batch_id, item_index, status, history_id, error, now()],
            )
            .map_err(|cause| format!("Could not update batch item: {cause}"))?;
        if changed == 0 {
            return Err("The selected batch item was not found".to_string());
        }
        let (total, completed, failed, cancelled, active): (i64, i64, i64, i64, i64) = transaction
            .query_row(
                "SELECT COUNT(*), SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END), SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), SUM(CASE WHEN status = 'cancelled' THEN 1 ELSE 0 END), SUM(CASE WHEN status IN ('queued','running') THEN 1 ELSE 0 END) FROM batch_items WHERE batch_id = ?1",
                [batch_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .map_err(|cause| format!("Could not calculate batch progress: {cause}"))?;
        let run_status = if active > 0 {
            "running"
        } else if failed > 0 {
            "failed"
        } else if cancelled > 0 {
            "cancelled"
        } else {
            "completed"
        };
        transaction
            .execute(
                "UPDATE batch_runs SET status = CASE WHEN status = 'cancelled' THEN 'cancelled' ELSE ?2 END, total_items = ?3, completed_items = ?4, failed_items = ?5, error = CASE WHEN ?5 > 0 THEN 'One or more items failed' ELSE NULL END, updated_at = ?6 WHERE id = ?1",
                params![batch_id, run_status, total, completed, failed, now()],
            )
            .map_err(|cause| format!("Could not update batch progress: {cause}"))?;
        transaction
            .commit()
            .map_err(|cause| format!("Could not commit batch progress: {cause}"))?;
        drop(connection);
        self.get_batch(batch_id)?
            .ok_or_else(|| "The batch disappeared during update".to_string())
    }

    pub fn list_batches(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT id FROM batch_runs ORDER BY created_at DESC LIMIT 100")
            .map_err(|error| format!("Could not prepare batch runs: {error}"))?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("Could not list batch runs: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read batch runs: {error}"))?;
        let mut batches = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(batch) = batch_value(&connection, &id)
                .map_err(|error| format!("Could not read batch details: {error}"))?
            {
                batches.push(batch);
            }
        }
        Ok(batches)
    }

    pub fn get_batch(&self, id: &str) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        batch_value(&connection, id).map_err(|error| format!("Could not read the batch: {error}"))
    }

    pub fn start_batch_item(
        &self,
        batch_id: &str,
        item_index: i64,
        job_id: &str,
    ) -> Result<bool, String> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the batch item: {error}"))?;
        let changed = transaction
            .execute(
                "UPDATE batch_items SET status = 'running', job_id = ?3, attempt = attempt + 1, error = NULL, updated_at = ?4
                 WHERE batch_id = ?1 AND item_index = ?2 AND status IN ('queued','failed')
                 AND EXISTS (
                    SELECT 1 FROM batch_runs
                    WHERE id = ?1 AND status IN ('queued','running') AND pause_requested = 0
                 )",
                params![batch_id, item_index, job_id, now()],
            )
            .map_err(|error| format!("Could not start the batch item: {error}"))?;
        if changed > 0 {
            transaction
                .execute(
                    "UPDATE batch_runs SET status = 'running', error = NULL, updated_at = ?2 WHERE id = ?1 AND status IN ('queued','failed')",
                    params![batch_id, now()],
                )
                .map_err(|error| format!("Could not start the batch run: {error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit batch start: {error}"))?;
        Ok(changed > 0)
    }

    pub fn cancel_batch(&self, batch_id: &str) -> Result<Vec<String>, String> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not cancel the batch: {error}"))?;
        let active_jobs = {
            let mut statement = transaction
                .prepare("SELECT job_id FROM batch_items WHERE batch_id = ?1 AND status = 'running' AND job_id IS NOT NULL")
                .map_err(|error| format!("Could not inspect active batch jobs: {error}"))?;
            let jobs = statement
                .query_map([batch_id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("Could not read active batch jobs: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read active batch jobs: {error}"))?;
            jobs
        };
        transaction
            .execute(
                "UPDATE batch_items SET status = 'cancelled', error = NULL, updated_at = ?2 WHERE batch_id = ?1 AND status IN ('queued','failed')",
                params![batch_id, now()],
            )
            .map_err(|error| format!("Could not cancel queued batch items: {error}"))?;
        transaction
            .execute(
                "UPDATE batch_runs SET status = 'cancelled', error = NULL, updated_at = ?2 WHERE id = ?1 AND status IN ('queued','running','failed')",
                params![batch_id, now()],
            )
            .map_err(|error| format!("Could not cancel the batch run: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit batch cancellation: {error}"))?;
        Ok(active_jobs)
    }

    pub fn pause_batch(&self, batch_id: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE batch_runs SET pause_requested = 1, updated_at = ?2 WHERE id = ?1 AND status IN ('queued','running')",
            params![batch_id, now()],
        ).map_err(|error| format!("Could not pause the batch: {error}"))?;
        if changed == 0 {
            return Err("Only a queued or running batch can be paused".to_string());
        }
        drop(connection);
        self.get_batch(batch_id)?
            .ok_or_else(|| "The paused batch was not found".to_string())
    }

    pub fn resume_batch(&self, batch_id: &str, retry_failed: bool) -> Result<Value, String> {
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not resume the batch: {error}"))?;
        let running: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM batch_items WHERE batch_id = ?1 AND status = 'running'",
                [batch_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect active batch rows: {error}"))?;
        if running > 0 {
            return Err("Wait for active rows to finish before resuming this batch".to_string());
        }
        if retry_failed {
            transaction.execute(
                "UPDATE batch_items SET status = 'queued', job_id = NULL, error = NULL, updated_at = ?2 WHERE batch_id = ?1 AND status = 'failed'",
                params![batch_id, now()],
            ).map_err(|error| format!("Could not requeue failed rows: {error}"))?;
        }
        let queued: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM batch_items WHERE batch_id = ?1 AND status IN ('queued','failed')",
            [batch_id], |row| row.get(0),
        ).map_err(|error| format!("Could not inspect resumable batch rows: {error}"))?;
        if queued == 0 {
            return Err("This batch has no rows to resume".to_string());
        }
        let changed = transaction.execute(
            "UPDATE batch_runs SET pause_requested = 0, status = 'queued', failed_items = (SELECT COUNT(*) FROM batch_items WHERE batch_id = ?1 AND status = 'failed'), error = NULL, updated_at = ?2 WHERE id = ?1 AND status != 'cancelled'",
            params![batch_id, now()],
        ).map_err(|error| format!("Could not resume the batch: {error}"))?;
        if changed == 0 {
            return Err("A cancelled batch cannot be resumed".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit batch resume: {error}"))?;
        drop(connection);
        self.get_batch(batch_id)?
            .ok_or_else(|| "The resumed batch was not found".to_string())
    }

    pub fn update_history_metadata(&self, id: &str, changes: &Value) -> Result<Value, String> {
        let connection = self.lock()?;
        let current: Option<(String, i64, String)> = connection
            .query_row(
                "SELECT title, favorite, notes FROM history WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not read history metadata: {error}"))?;
        let Some((current_title, current_favorite, current_notes)) = current else {
            return Err("The history record was not found".to_string());
        };
        let title = changes
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&current_title);
        let favorite = changes
            .get("favorite")
            .and_then(Value::as_bool)
            .map(|value| if value { 1 } else { 0 })
            .unwrap_or(current_favorite);
        let notes = changes
            .get("notes")
            .and_then(Value::as_str)
            .unwrap_or(&current_notes);
        if notes.len() > 10_000 {
            return Err("History notes are limited to 10,000 characters".to_string());
        }
        connection
            .execute(
                "UPDATE history SET title = ?2, favorite = ?3, notes = ?4 WHERE id = ?1",
                params![id, title, favorite, notes],
            )
            .map_err(|error| format!("Could not update history metadata: {error}"))?;
        drop(connection);
        self.list_history(None)?
            .into_iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| "The updated history record was not found".to_string())
    }

    pub fn history_request(&self, id: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT jobs.request_json FROM history JOIN jobs ON jobs.id = history.job_id WHERE history.id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Could not read generation settings: {error}"))?;
        payload
            .ok_or_else(|| "The history record was not found".to_string())
            .and_then(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| format!("Stored generation settings are invalid: {error}"))
            })
    }

    pub fn save_comparison(&self, comparison: &Value) -> Result<Value, String> {
        let left_history_id = required_trimmed(
            comparison,
            "left_history_id",
            "Comparison output A is required",
        )?;
        let right_history_id = required_trimmed(
            comparison,
            "right_history_id",
            "Comparison output B is required",
        )?;
        let script = required_trimmed(comparison, "script", "Comparison script is required")?;
        let id = comparison
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let winner = comparison.get("winner").and_then(Value::as_str);
        if winner.is_some_and(|value| !matches!(value, "A" | "B" | "tie")) {
            return Err("Comparison winner must be A, B, or tie".to_string());
        }
        let notes = comparison
            .get("notes")
            .and_then(Value::as_str)
            .unwrap_or("");
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not save comparison: {error}"))?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM comparison_runs WHERE id = ?1)",
                [&id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect comparison: {error}"))?;
        if !exists {
            transaction.execute(
                "INSERT INTO comparison_runs (id, script, status, blind, revealed, notes, created_at, updated_at)
                 VALUES (?1, ?2, 'completed', 0, 1, ?3, ?4, ?4)",
                params![id, script, notes, timestamp],
            ).map_err(|error| format!("Could not save comparison: {error}"))?;
            for (position, (label, history_id)) in [("A", left_history_id), ("B", right_history_id)]
                .into_iter()
                .enumerate()
            {
                transaction.execute(
                    "INSERT INTO comparison_takes (id, comparison_id, position, label, request_json, history_id, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, '{}', ?5, 'completed', ?6, ?6)",
                    params![format!("{id}:{label}"), id, position as i64, label, history_id, timestamp],
                ).map_err(|error| format!("Could not save comparison take: {error}"))?;
            }
        }
        let winner_take_id = winner.and_then(|label| match label {
            "A" | "B" => Some(format!("{id}:{label}")),
            _ => None,
        });
        transaction.execute(
            "UPDATE comparison_runs SET winner_take_id = ?2, tie = ?3, notes = ?4, revealed = 1, updated_at = ?5 WHERE id = ?1",
            params![id, winner_take_id, winner == Some("tie"), notes, timestamp],
        ).map_err(|error| format!("Could not update comparison: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit comparison: {error}"))?;
        drop(connection);
        let mut saved = self
            .get_comparison(&id)?
            .ok_or_else(|| "The comparison was not found".to_string())?;
        saved["left_history_id"] = json!(left_history_id);
        saved["right_history_id"] = json!(right_history_id);
        saved["winner"] = json!(winner);
        Ok(saved)
    }

    pub fn list_comparisons(&self) -> Result<Vec<Value>, String> {
        let ids = {
            let connection = self.lock()?;
            let mut statement = connection
                .prepare("SELECT id FROM comparison_runs ORDER BY updated_at DESC LIMIT 500")
                .map_err(|error| format!("Could not prepare comparisons: {error}"))?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("Could not list comparisons: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read comparisons: {error}"))?;
            ids
        };
        ids.into_iter()
            .map(|id| {
                self.get_comparison(&id)?
                    .ok_or_else(|| "The comparison disappeared while loading".to_string())
            })
            .collect()
    }

    pub fn create_comparison(&self, request: &Value) -> Result<Value, String> {
        let script = required_trimmed(request, "script", "Comparison script is required")?;
        if script.chars().count() > 20_000 {
            return Err("Comparison scripts are limited to 20,000 characters".to_string());
        }
        let takes = request
            .get("takes")
            .and_then(Value::as_array)
            .ok_or("Comparison takes are required")?;
        if !(2..=4).contains(&takes.len()) {
            return Err("A comparison requires between 2 and 4 takes".to_string());
        }
        let default_priority = priority_value(request.get("priority"))?;
        let blind = request
            .get("blind")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not start the comparison: {error}"))?;
        transaction
            .execute(
                "INSERT INTO comparison_runs (id, script, status, blind, revealed, notes, created_at, updated_at)
                 VALUES (?1, ?2, 'running', ?3, ?4, '', ?5, ?5)",
                params![id, script, blind, !blind, timestamp],
            )
            .map_err(|error| format!("Could not create the comparison: {error}"))?;
        for (position, raw_take) in takes.iter().enumerate() {
            let object = raw_take
                .as_object()
                .ok_or_else(|| format!("Comparison take {} must be an object", position + 1))?;
            let model_id = object
                .get("model_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("Comparison take {} requires a model", position + 1))?;
            let mut synthesis = Value::Object(object.clone());
            let label = char::from(b'A' + position as u8).to_string();
            let priority = object
                .get("priority")
                .map(|value| priority_value(Some(value)))
                .transpose()?
                .unwrap_or(default_priority);
            synthesis["text"] = json!(script);
            synthesis["priority"] = json!(priority_name(priority));
            if synthesis.get("speaker").and_then(Value::as_str).is_none() {
                synthesis["speaker"] = json!("default");
            }
            if synthesis.get("language").and_then(Value::as_str).is_none() {
                synthesis["language"] = json!("en");
            }
            if synthesis.get("speed").and_then(numeric_value).is_none() {
                synthesis["speed"] = json!(1.0);
            }
            if synthesis.get("seed").and_then(Value::as_u64).is_none() {
                synthesis["seed"] = json!(42_817_u64.saturating_add(position as u64));
            }
            if synthesis
                .get("output_format")
                .and_then(Value::as_str)
                .is_none()
            {
                synthesis["output_format"] = json!("wav");
            }
            synthesis["title"] = json!(format!(
                "Compare {label}: {}",
                script.chars().take(36).collect::<String>()
            ));
            synthesis["comparison_id"] = json!(id);
            synthesis["comparison_label"] = json!(label);
            let job_id = Uuid::new_v4().simple().to_string();
            let take_id = Uuid::new_v4().simple().to_string();
            let payload = synthesis.to_string();
            transaction
                .execute(
                    "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at)
                     VALUES (?1, 'comparison-synthesis', 'preparing', ?2, 0.05, 1, ?3, ?4, ?4)",
                    params![job_id, payload, priority, timestamp],
                )
                .map_err(|error| format!("Could not create comparison job: {error}"))?;
            insert_job_event(&transaction, &job_id, "preparing", 0.05, None, &timestamp)?;
            transaction
                .execute(
                    "INSERT INTO comparison_takes (id, comparison_id, position, label, request_json, job_id, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'preparing', ?7, ?7)",
                    params![take_id, id, position as i64, label, payload, job_id, timestamp],
                )
                .map_err(|error| format!("Could not create comparison take: {error}"))?;
            let _ = model_id;
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the comparison: {error}"))?;
        drop(connection);
        self.get_comparison(&id)?
            .ok_or_else(|| "The comparison was not found after creation".to_string())
    }

    pub fn get_comparison(&self, id: &str) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        let run: Option<(String, String, String, i64, i64, i64, Option<String>, Option<String>, String, String, String)> = connection
            .query_row(
                "SELECT id, script, status, blind, revealed, tie, winner_take_id, promoted_take_id, notes, created_at, updated_at
                 FROM comparison_runs WHERE id = ?1",
                [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?)),
            )
            .optional()
            .map_err(|error| format!("Could not read the comparison: {error}"))?;
        let Some((
            id,
            script,
            status,
            blind,
            revealed,
            tie,
            winner_take_id,
            promoted_take_id,
            notes,
            created_at,
            updated_at,
        )) = run
        else {
            return Ok(None);
        };
        let mut statement = connection
            .prepare(
                "SELECT t.id, t.position, t.label, t.request_json, t.job_id, t.history_id,
                        CASE WHEN t.status IN ('completed','failed','cancelled') THEN t.status ELSE COALESCE(j.status, t.status) END,
                        t.rating, t.notes, t.favorite, t.error, t.created_at, t.updated_at
                 FROM comparison_takes t LEFT JOIN jobs j ON j.id = t.job_id
                 WHERE t.comparison_id = ?1 ORDER BY t.position",
            )
            .map_err(|error| format!("Could not prepare comparison takes: {error}"))?;
        let takes = statement
            .query_map([&id], |row| {
                let request_json: String = row.get(3)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?, "position": row.get::<_, i64>(1)?,
                    "label": row.get::<_, String>(2)?,
                    "request": serde_json::from_str::<Value>(&request_json).unwrap_or_else(|_| json!({})),
                    "job_id": row.get::<_, Option<String>>(4)?, "history_id": row.get::<_, Option<String>>(5)?,
                    "status": row.get::<_, String>(6)?, "rating": row.get::<_, Option<i64>>(7)?,
                    "notes": row.get::<_, String>(8)?, "favorite": row.get::<_, i64>(9)? != 0,
                    "error": row.get::<_, Option<String>>(10)?, "created_at": row.get::<_, String>(11)?,
                    "updated_at": row.get::<_, String>(12)?,
                }))
            })
            .map_err(|error| format!("Could not list comparison takes: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read comparison takes: {error}"))?;
        drop(statement);
        drop(connection);
        let takes = takes
            .into_iter()
            .map(|mut take| {
                let history_id = take.get("history_id").and_then(Value::as_str);
                take["result"] = history_id
                    .map(|history_id| self.get_history(history_id))
                    .transpose()?
                    .flatten()
                    .unwrap_or(Value::Null);
                Ok(take)
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Some(json!({
            "id": id, "script": script, "status": status, "blind": blind != 0,
            "revealed": revealed != 0, "tie": tie != 0, "winner_take_id": winner_take_id,
            "promoted_take_id": promoted_take_id, "notes": notes, "takes": takes,
            "created_at": created_at, "updated_at": updated_at,
        })))
    }

    pub fn comparison_execution_plan(
        &self,
        id: &str,
    ) -> Result<Vec<(String, String, Value)>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, job_id, request_json FROM comparison_takes
                 WHERE comparison_id = ?1 AND status IN ('queued','preparing') ORDER BY position",
            )
            .map_err(|error| format!("Could not prepare comparison execution: {error}"))?;
        let plan = statement
            .query_map([id], |row| {
                let payload: String = row.get(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    serde_json::from_str::<Value>(&payload).unwrap_or_else(|_| json!({})),
                ))
            })
            .map_err(|error| format!("Could not read comparison execution: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read comparison takes: {error}"))?;
        Ok(plan)
    }

    pub fn finish_comparison_take(
        &self,
        comparison_id: &str,
        take_id: &str,
        history_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        let status = if history_id.is_some() {
            "completed"
        } else if error == Some("cancelled") {
            "cancelled"
        } else {
            "failed"
        };
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finish comparison take: {error}"))?;
        let changed = transaction.execute(
            "UPDATE comparison_takes SET status = ?3, history_id = ?4, error = ?5, updated_at = ?6 WHERE id = ?1 AND comparison_id = ?2",
            params![take_id, comparison_id, status, history_id, error.filter(|value| *value != "cancelled"), timestamp],
        ).map_err(|error| format!("Could not update comparison take: {error}"))?;
        if changed == 0 {
            return Err("The comparison take was not found".to_string());
        }
        let (total, active, completed, failed, cancelled): (i64, i64, i64, i64, i64) = transaction.query_row(
            "SELECT COUNT(*), SUM(status IN ('queued','preparing','running')), SUM(status = 'completed'), SUM(status = 'failed'), SUM(status = 'cancelled') FROM comparison_takes WHERE comparison_id = ?1",
            [comparison_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).map_err(|error| format!("Could not summarize comparison: {error}"))?;
        let run_status = if active > 0 {
            "running"
        } else if completed == total {
            "completed"
        } else if completed > 0 {
            "partial"
        } else if cancelled == total {
            "cancelled"
        } else if failed > 0 {
            "failed"
        } else {
            "cancelled"
        };
        transaction
            .execute(
                "UPDATE comparison_runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
                params![comparison_id, run_status, timestamp],
            )
            .map_err(|error| format!("Could not finish comparison: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit comparison result: {error}"))
    }

    pub fn update_comparison_review(&self, id: &str, changes: &Value) -> Result<Value, String> {
        let notes = changes.get("notes").and_then(Value::as_str);
        if notes.is_some_and(|value| value.len() > 10_000) {
            return Err("Comparison notes are limited to 10,000 characters".to_string());
        }
        let take_id = changes.get("take_id").and_then(Value::as_str);
        let rating = changes.get("rating").and_then(Value::as_i64);
        if rating.is_some_and(|value| !(1..=5).contains(&value)) {
            return Err("Take ratings must be between 1 and 5".to_string());
        }
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not update comparison review: {error}"))?;
        if let Some(take_id) = take_id {
            let changed = transaction.execute(
                "UPDATE comparison_takes SET rating = COALESCE(?3, rating), notes = COALESCE(?4, notes), favorite = COALESCE(?5, favorite), updated_at = ?6 WHERE id = ?2 AND comparison_id = ?1",
                params![id, take_id, rating, notes, changes.get("favorite").and_then(Value::as_bool), timestamp],
            ).map_err(|error| format!("Could not update comparison take review: {error}"))?;
            if changed == 0 {
                return Err("The comparison take was not found".to_string());
            }
        } else {
            let winner = changes.get("winner_take_id").and_then(Value::as_str);
            let promoted = changes.get("promoted_take_id").and_then(Value::as_str);
            let tie = changes.get("tie").and_then(Value::as_bool);
            for selected in [winner, promoted].into_iter().flatten() {
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM comparison_takes WHERE comparison_id = ?1 AND id = ?2 AND status = 'completed')",
                    params![id, selected], |row| row.get(0),
                ).map_err(|error| format!("Could not validate comparison selection: {error}"))?;
                if !exists {
                    return Err(
                        "Only a completed take from this comparison can be selected".to_string()
                    );
                }
            }
            transaction.execute(
                "UPDATE comparison_runs SET revealed = COALESCE(?2, revealed), tie = COALESCE(?3, tie), winner_take_id = CASE WHEN ?3 = 1 THEN NULL ELSE COALESCE(?4, winner_take_id) END, promoted_take_id = COALESCE(?5, promoted_take_id), notes = COALESCE(?6, notes), updated_at = ?7 WHERE id = ?1",
                params![id, changes.get("revealed").and_then(Value::as_bool), tie, winner, promoted, notes, timestamp],
            ).map_err(|error| format!("Could not update comparison review: {error}"))?;
            if winner.is_some() {
                transaction
                    .execute("UPDATE comparison_runs SET tie = 0 WHERE id = ?1", [id])
                    .map_err(|error| format!("Could not update comparison verdict: {error}"))?;
            }
            if let Some(promoted) = promoted {
                transaction.execute(
                    "UPDATE history SET favorite = 1 WHERE id = (SELECT history_id FROM comparison_takes WHERE id = ?1 AND comparison_id = ?2)",
                    params![promoted, id],
                ).map_err(|error| format!("Could not promote comparison take: {error}"))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit comparison review: {error}"))?;
        drop(connection);
        self.get_comparison(id)?
            .ok_or_else(|| "The comparison was not found".to_string())
    }

    pub fn comparison_active_jobs(&self, id: &str) -> Result<Vec<(String, String)>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT t.id, t.job_id FROM comparison_takes t JOIN jobs j ON j.id = t.job_id
                 WHERE t.comparison_id = ?1 AND j.status IN ('queued','preparing','running')",
            )
            .map_err(|error| format!("Could not prepare comparison cancellation: {error}"))?;
        let jobs = statement
            .query_map([id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("Could not list comparison jobs: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read comparison jobs: {error}"))?;
        Ok(jobs)
    }

    pub fn save_preset(&self, preset: &Value) -> Result<Value, String> {
        let id = preset
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let name = preset
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or("Preset name is required")?;
        let settings = preset.get("settings").cloned().unwrap_or_else(|| json!({}));
        let timestamp = now();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO presets (id, name, schema_version, settings_json, created_at, updated_at) VALUES (?1, ?2, 1, ?3, ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, settings_json = excluded.settings_json, updated_at = excluded.updated_at",
                params![id, name, settings.to_string(), timestamp],
            )
            .map_err(|error| format!("Could not save the preset: {error}"))?;
        Ok(
            json!({ "id": id, "name": name, "schema_version": 1, "settings": settings, "created_at": timestamp }),
        )
    }

    pub fn list_presets(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT id, name, schema_version, settings_json, created_at FROM presets ORDER BY name")
            .map_err(|error| format!("Could not prepare presets: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                let settings: String = row.get(3)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "schema_version": row.get::<_, i64>(2)?,
                    "settings": serde_json::from_str::<Value>(&settings).unwrap_or_else(|_| json!({})),
                    "created_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|error| format!("Could not list presets: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read presets: {error}"))
    }

    pub fn save_project(&self, project: &Value) -> Result<Value, String> {
        let id = project
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let name = required_trimmed(project, "name", "Project name is required")?;
        let document = project.get("document").cloned().unwrap_or_else(|| {
            json!({
                "script": "",
                "chapters": [],
                "speaker_assignments": {}
            })
        });
        let document_json = document.to_string();
        let timestamp = now();
        let mut connection = self.lock()?;
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT document_json, project_kind FROM projects WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the existing project: {error}"))?;
        if existing
            .as_ref()
            .is_some_and(|(_, project_kind)| project_kind == "video")
        {
            return Err(
                "A Video Studio project can only be revised through its timeline service"
                    .to_string(),
            );
        }
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not save the project: {error}"))?;
        transaction
            .execute(
                "INSERT INTO projects (id, name, document_json, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, document_json = excluded.document_json, updated_at = excluded.updated_at",
                params![id, name, document_json, timestamp],
            )
            .map_err(|error| format!("Could not save the project: {error}"))?;
        if existing
            .as_ref()
            .map(|(existing_document, _)| existing_document.as_str())
            != Some(document_json.as_str())
        {
            transaction
                .execute(
                    "INSERT INTO project_revisions (id, project_id, document_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![Uuid::new_v4().simple().to_string(), id, document_json, timestamp],
                )
                .map_err(|error| format!("Could not save the project revision: {error}"))?;
        }
        let chapters = document
            .get("chapters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (position, chapter) in chapters.iter().enumerate() {
            let clip_id = chapter
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
            let title = chapter
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled chapter");
            let text = chapter.get("text").and_then(Value::as_str).unwrap_or("");
            let content_hash = sha256_bytes(text.as_bytes());
            let prior: Option<(String, Option<String>)> = transaction
                .query_row(
                    "SELECT content_hash, history_id FROM project_clips WHERE id = ?1 AND project_id = ?2",
                    params![clip_id, id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| format!("Could not inspect project clip: {error}"))?;
            let requested_history = chapter.get("history_id").and_then(Value::as_str);
            let history_id = if prior
                .as_ref()
                .is_some_and(|(hash, _)| hash == &content_hash)
            {
                requested_history
                    .map(str::to_string)
                    .or_else(|| prior.and_then(|(_, history)| history))
            } else {
                None
            };
            let status = if history_id.is_some() {
                "rendered"
            } else if text.trim().is_empty() {
                "empty"
            } else {
                "stale"
            };
            let settings = json!({
                "voice_id": chapter.get("voice_id").cloned().unwrap_or(Value::Null),
                "model_id": chapter.get("model_id").cloned().unwrap_or(Value::Null),
                "language": chapter.get("language").cloned().unwrap_or_else(|| json!("en")),
            });
            transaction
                .execute(
                    "INSERT INTO project_clips (id, project_id, position, title, text, status, history_id, settings_json, content_hash, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                     ON CONFLICT(id) DO UPDATE SET position = excluded.position, title = excluded.title, text = excluded.text, status = excluded.status, history_id = excluded.history_id, settings_json = excluded.settings_json, content_hash = excluded.content_hash, updated_at = excluded.updated_at",
                    params![clip_id, id, position as i64, title, text, status, history_id, settings.to_string(), content_hash, timestamp],
                )
                .map_err(|error| format!("Could not save project clip: {error}"))?;
        }
        if chapters.is_empty() {
            transaction
                .execute("DELETE FROM project_clips WHERE project_id = ?1", [&id])
                .map_err(|error| format!("Could not clear project clips: {error}"))?;
        } else {
            let ids = chapters
                .iter()
                .filter_map(|chapter| chapter.get("id").and_then(Value::as_str))
                .collect::<Vec<_>>();
            let mut statement = transaction
                .prepare("SELECT id FROM project_clips WHERE project_id = ?1")
                .map_err(|error| format!("Could not reconcile project clips: {error}"))?;
            let existing_ids = statement
                .query_map([&id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("Could not inspect project clips: {error}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read project clips: {error}"))?;
            drop(statement);
            for stale_id in existing_ids
                .iter()
                .filter(|clip_id| !ids.contains(&clip_id.as_str()))
            {
                transaction
                    .execute("DELETE FROM project_clips WHERE id = ?1", [stale_id])
                    .map_err(|error| format!("Could not remove stale project clip: {error}"))?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("Could not commit the project: {error}"))?;
        Ok(json!({
            "id": id,
            "name": name,
            "document": document,
            "created_at": timestamp,
            "updated_at": timestamp
        }))
    }

    pub fn list_projects(&self) -> Result<Vec<Value>, String> {
        self.reconcile_project_masters()?;
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT id, name, document_json, created_at, updated_at FROM projects WHERE project_kind = 'audio' ORDER BY updated_at DESC")
            .map_err(|error| format!("Could not prepare projects: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                let document: String = row.get(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "name": row.get::<_, String>(1)?,
                    "document": serde_json::from_str::<Value>(&document).unwrap_or_else(|_| json!({})),
                    "created_at": row.get::<_, String>(3)?,
                    "updated_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|error| format!("Could not list projects: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read projects: {error}"))
    }

    pub fn video_artifacts_root(&self) -> PathBuf {
        self.artifacts_root.join("video")
    }

    pub fn create_video_project(
        &self,
        name: &str,
        manifest: &Value,
        actor: &str,
    ) -> Result<Value, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 160 {
            return Err(
                "video.invalid_name: A video project name must contain 1 to 160 characters".into(),
            );
        }
        if !manifest.is_object() {
            return Err(
                "video.invalid_manifest: The timeline manifest must be a JSON object".into(),
            );
        }
        let schema_version = manifest
            .get("schema_version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_manifest: schema_version must be a positive integer")?;
        let project_id = manifest
            .get("project_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let version_id = Uuid::new_v4().simple().to_string();
        let manifest_json = manifest.to_string();
        let manifest_sha256 = sha256_bytes(manifest_json.as_bytes());
        let aspect_ratio = manifest_aspect_ratio(manifest);
        let duration_us = manifest_duration_us(manifest);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|error| {
            format!("video.store_failed: Could not create the video project: {error}")
        })?;
        transaction
            .execute(
                "INSERT INTO projects (id, name, document_json, created_at, updated_at, project_kind, current_revision)
                 VALUES (?1, ?2, ?3, ?4, ?4, 'video', 1)",
                params![project_id, name, manifest_json, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not create the project record: {error}"))?;
        transaction
            .execute(
                "INSERT INTO video_projects (project_id, status, aspect_ratio, duration_us, current_version_id, source_summary_json, created_at, updated_at)
                 VALUES (?1, 'draft', ?2, ?3, ?4, '{}', ?5, ?5)",
                params![project_id, aspect_ratio, duration_us, version_id, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not create video project state: {error}"))?;
        transaction
            .execute(
                "INSERT INTO video_project_versions (id, project_id, revision, schema_version, manifest_json, manifest_sha256, base_revision, actor, reason, created_at)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, 0, ?6, 'Project created', ?7)",
                params![version_id, project_id, schema_version, manifest_json, manifest_sha256, actor, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not save the initial timeline: {error}"))?;
        transaction
            .execute(
                "INSERT INTO video_project_events (id, project_id, version_id, event_kind, payload_json, created_at)
                 VALUES (?1, ?2, ?3, 'project.created', ?4, ?5)",
                params![Uuid::new_v4().simple().to_string(), project_id, version_id, json!({"actor": actor}).to_string(), timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not record project creation: {error}"))?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the video project: {error}")
        })?;
        drop(connection);
        self.get_video_project(&project_id)?
            .ok_or_else(|| "video.store_failed: The created project could not be reloaded".into())
    }

    /// Atomically creates the initial canonical Video Studio project/version and its owning
    /// prompt workflow. This removes both orphan directions: a restart can never leave a prompt
    /// parent without a visible project, or a project without the durable job needed to resume it.
    pub fn create_video_project_with_job(
        &self,
        name: &str,
        manifest: &Value,
        actor: &str,
        job_kind: &str,
        job_request: &Value,
        idempotency_key: &str,
    ) -> Result<(Value, String), String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 160 {
            return Err(
                "video.invalid_name: A video project name must contain 1 to 160 characters".into(),
            );
        }
        if job_kind != "video_create_from_prompt" {
            return Err(
                "video.invalid_job_kind: Atomic project creation accepts only the prompt workflow"
                    .into(),
            );
        }
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 256 {
            return Err(
                "video.invalid_idempotency_key: Prompt workflow identity is invalid".into(),
            );
        }
        if actor.trim().is_empty() || actor.chars().count() > 256 {
            return Err("video.invalid_actor: A bounded project actor is required".into());
        }
        if !manifest.is_object() {
            return Err(
                "video.invalid_manifest: The timeline manifest must be a JSON object".into(),
            );
        }
        let schema_version = manifest
            .get("schema_version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_manifest: schema_version must be a positive integer")?;
        let project_id = required_trimmed(
            manifest,
            "project_id",
            "video.invalid_project_id: A stable project id is required",
        )?;
        if project_id.len() > 256
            || job_request.get("project_id").and_then(Value::as_str) != Some(project_id)
        {
            return Err(
                "video.ownership_mismatch: Prompt parent and manifest must name one exact project"
                    .into(),
            );
        }
        if manifest.get("revision").and_then(Value::as_u64) != Some(1) {
            return Err(
                "video.invalid_initial_revision: Atomic prompt projects must begin at revision one"
                    .into(),
            );
        }
        let priority = priority_value(job_request.get("priority"))?;
        let request_json = durable_request(job_request).to_string();
        let request_sha256 = sha256_bytes(request_json.as_bytes());
        let job_id = Uuid::new_v4().simple().to_string();
        let version_id = Uuid::new_v4().simple().to_string();
        let manifest_json = manifest.to_string();
        let manifest_sha256 = sha256_bytes(manifest_json.as_bytes());
        let aspect_ratio = manifest_aspect_ratio(manifest);
        let duration_us = manifest_duration_us(manifest);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                format!(
                    "video.store_failed: Could not start atomic prompt project creation: {error}"
                )
            })?;
        let existing_submission: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM api_job_submissions
                 WHERE operation = ?1 AND idempotency_key = ?2)",
                params![job_kind, idempotency_key],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not inspect prompt workflow identity: {error}")
            })?;
        if existing_submission {
            return Err(
                "video.resume_conflict: The prompt workflow identity is already registered".into(),
            );
        }
        transaction
            .execute(
                "INSERT INTO projects
                 (id, name, document_json, created_at, updated_at, project_kind, current_revision)
                 VALUES (?1, ?2, ?3, ?4, ?4, 'video', 1)",
                params![project_id, name, manifest_json, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not create the prompt project record: {error}")
            })?;
        transaction
            .execute(
                "INSERT INTO video_projects
                 (project_id, status, aspect_ratio, duration_us, current_version_id,
                  source_summary_json, created_at, updated_at)
                 VALUES (?1, 'draft', ?2, ?3, ?4, '{}', ?5, ?5)",
                params![project_id, aspect_ratio, duration_us, version_id, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not create prompt video state: {error}")
            })?;
        transaction
            .execute(
                "INSERT INTO video_project_versions
                 (id, project_id, revision, schema_version, manifest_json, manifest_sha256,
                  base_revision, actor, reason, created_at)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, 0, ?6, 'Project created', ?7)",
                params![
                    version_id,
                    project_id,
                    schema_version,
                    manifest_json,
                    manifest_sha256,
                    actor,
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not save the initial prompt timeline: {error}")
            })?;
        transaction
            .execute(
                "INSERT INTO video_project_events
                 (id, project_id, version_id, event_kind, payload_json, created_at)
                 VALUES (?1, ?2, ?3, 'project.created', ?4, ?5)",
                params![
                    Uuid::new_v4().simple().to_string(),
                    project_id,
                    version_id,
                    json!({"actor": actor, "workflow_job_id": job_id}).to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not record prompt project creation: {error}")
            })?;
        transaction
            .execute(
                "INSERT INTO jobs
                 (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at)
                 VALUES (?1, ?2, 'preparing', ?3, 0.05, 1, ?4, ?5, ?5)",
                params![job_id, job_kind, request_json, priority, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not create the prompt parent job: {error}")
            })?;
        insert_job_event(&transaction, &job_id, "preparing", 0.05, None, &timestamp)?;
        transaction
            .execute(
                "INSERT INTO api_job_submissions
                 (operation, idempotency_key, request_sha256, job_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![job_kind, idempotency_key, request_sha256, job_id, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not bind the prompt workflow identity: {error}")
            })?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit atomic prompt project creation: {error}")
        })?;
        drop(connection);
        let project = self
            .get_video_project(project_id)?
            .ok_or("video.store_failed: The atomic prompt project could not be reloaded")?;
        Ok((project, job_id))
    }

    pub fn list_video_projects(&self) -> Result<Vec<Value>, String> {
        let ids = {
            let connection = self.lock()?;
            let mut statement = connection
                .prepare(
                    "SELECT p.id FROM projects p JOIN video_projects v ON v.project_id = p.id
                     WHERE p.project_kind = 'video' ORDER BY p.updated_at DESC",
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not prepare video projects: {error}")
                })?;
            let ids = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| {
                    format!("video.store_failed: Could not list video projects: {error}")
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    format!("video.store_failed: Could not read video projects: {error}")
                })?;
            ids
        };
        ids.iter()
            .map(|id| {
                self.get_video_project(id)?.ok_or_else(|| {
                    format!("video.store_failed: Video project {id} disappeared while listing")
                })
            })
            .collect()
    }

    pub fn get_video_project(&self, project_id: &str) -> Result<Option<Value>, String> {
        let record: Option<(
            String,
            String,
            i64,
            String,
            String,
            i64,
            String,
            String,
            String,
            i64,
            String,
            String,
            String,
        )> = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT p.id, p.name, p.current_revision, p.created_at, p.updated_at,
                            v.duration_us, v.status, v.aspect_ratio, vv.id, vv.schema_version,
                            vv.manifest_json, vv.manifest_sha256, vv.created_at
                     FROM projects p
                     JOIN video_projects v ON v.project_id = p.id
                     JOIN video_project_versions vv ON vv.id = v.current_version_id
                     WHERE p.id = ?1 AND p.project_kind = 'video'",
                    [project_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                            row.get(10)?,
                            row.get(11)?,
                            row.get(12)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| {
                    format!("video.store_failed: Could not load the video project: {error}")
                })?
        };
        let Some((
            id,
            name,
            revision,
            created_at,
            updated_at,
            duration_us,
            status,
            aspect_ratio,
            version_id,
            schema_version,
            manifest_json,
            manifest_sha256,
            version_created_at,
        )) = record
        else {
            return Ok(None);
        };
        let observed_manifest_sha256 = sha256_bytes(manifest_json.as_bytes());
        if observed_manifest_sha256 != manifest_sha256 {
            return Err(
                "video.integrity_failed: The current timeline manifest checksum is invalid".into(),
            );
        }
        let manifest = serde_json::from_str::<Value>(&manifest_json).map_err(|error| {
            format!(
                "video.integrity_failed: The current timeline manifest is invalid JSON: {error}"
            )
        })?;
        Ok(Some(json!({
            "id": id,
            "name": name,
            "project_kind": "video",
            "revision": revision,
            "status": status,
            "aspect_ratio": aspect_ratio,
            "duration_us": duration_us,
            "created_at": created_at,
            "updated_at": updated_at,
            "version": {
                "id": version_id,
                "schema_version": schema_version,
                "sha256": manifest_sha256,
                "created_at": version_created_at,
            },
            "manifest": manifest,
            "assets": self.list_video_assets(&id)?,
            "outputs": self.list_video_outputs(&id)?,
            "stages": self.list_video_stages(&id)?,
        })))
    }

    pub fn commit_video_manifest(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
    ) -> Result<Value, String> {
        self.commit_video_manifest_guarded(
            project_id,
            expected_revision,
            manifest,
            actor,
            reason,
            lock_token,
            status,
            None,
            false,
        )
    }

    /// Commits an editorial manifest mutation only while its durable parent Video Studio job is
    /// still active. The job predicate and revision CAS are checked in the same SQLite write
    /// transaction as the version insert, so cancellation cannot race a final narration publish.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_video_manifest_if_job_active(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
        required_active_job_id: &str,
    ) -> Result<Value, String> {
        if required_active_job_id.trim().is_empty() || required_active_job_id.len() > 256 {
            return Err(
                "video.parent_job_inactive: The durable parent job is missing or invalid".into(),
            );
        }
        self.commit_video_manifest_guarded(
            project_id,
            expected_revision,
            manifest,
            actor,
            reason,
            lock_token,
            status,
            Some(required_active_job_id),
            false,
        )
    }

    /// Atomically commits one editorial revision and completes its durable edit job.
    ///
    /// This closes the otherwise unavoidable crash/cancellation window between publishing a
    /// timeline mutation and recording the synchronous editor operation as completed. The same
    /// active-job, project-lock, and revision predicates are checked inside the write transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_video_manifest_and_complete_job(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
        required_active_job_id: &str,
    ) -> Result<Value, String> {
        if required_active_job_id.trim().is_empty() || required_active_job_id.len() > 256 {
            return Err(
                "video.parent_job_inactive: The durable editor job is missing or invalid".into(),
            );
        }
        self.commit_video_manifest_guarded(
            project_id,
            expected_revision,
            manifest,
            actor,
            reason,
            lock_token,
            status,
            Some(required_active_job_id),
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_video_manifest_guarded(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
        required_active_job_id: Option<&str>,
        complete_required_job: bool,
    ) -> Result<Value, String> {
        if !manifest.is_object() {
            return Err(
                "video.invalid_manifest: The timeline manifest must be a JSON object".into(),
            );
        }
        let schema_version = manifest
            .get("schema_version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_manifest: schema_version must be a positive integer")?;
        let status = status.unwrap_or("draft");
        if !matches!(
            status,
            "draft"
                | "ingesting"
                | "analyzing"
                | "review"
                | "ready"
                | "rendering"
                | "completed"
                | "failed"
                | "archived"
        ) {
            return Err("video.invalid_status: Unsupported project status".into());
        }
        let manifest_json = manifest.to_string();
        let manifest_sha256 = sha256_bytes(manifest_json.as_bytes());
        let timestamp = now();
        let duration_us = manifest_duration_us(manifest);
        let aspect_ratio = manifest_aspect_ratio(manifest);
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                format!("video.store_failed: Could not start the timeline revision: {error}")
            })?;
        let active_lock: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM video_project_locks WHERE project_id = ?1 AND token = ?2 AND lease_expires_at > ?3)",
                params![project_id, lock_token, timestamp],
                |row| row.get(0),
            )
            .map_err(|error| format!("video.store_failed: Could not validate the project lease: {error}"))?;
        if !active_lock {
            return Err(
                "video.lock_required: Acquire or renew the project lease before saving".into(),
            );
        }
        let current_revision: i64 = transaction
            .query_row(
                "SELECT current_revision FROM projects WHERE id = ?1 AND project_kind = 'video'",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the project revision: {error}")
            })?
            .ok_or("video.not_found: The video project was not found")?;
        if current_revision != expected_revision {
            return Err(format!(
                "video.revision_conflict: Expected revision {expected_revision}, but the project is at revision {current_revision}"
            ));
        }
        if let Some(required_job_id) = required_active_job_id {
            let parent = transaction
                .query_row(
                    "SELECT kind, status, request_json FROM jobs WHERE id = ?1",
                    [required_job_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(|error| {
                    format!("video.store_failed: Could not inspect the durable parent job: {error}")
                })?;
            let Some((kind, parent_status, request_json)) = parent else {
                return Err(
                    "video.parent_job_inactive: The durable parent job was not found".into(),
                );
            };
            if parent_status == "cancelled" {
                return Err(
                    "video.cancelled: The durable parent Video Studio job was cancelled".into(),
                );
            }
            if !matches!(parent_status.as_str(), "queued" | "preparing" | "running") {
                return Err(format!(
                    "video.parent_job_inactive: The durable parent job is {parent_status}"
                ));
            }
            if !kind.starts_with("video_") {
                return Err(
                    "video.ownership_mismatch: The required parent is not a Video Studio job"
                        .into(),
                );
            }
            let parent_request = serde_json::from_str::<Value>(&request_json).map_err(|error| {
                format!("video.invalid_request: The durable parent request is invalid: {error}")
            })?;
            let parent_project_id = parent_request
                .get("project_id")
                .or_else(|| parent_request.pointer("/base/project_id"))
                .and_then(Value::as_str);
            if parent_project_id != Some(project_id) {
                return Err(
                    "video.ownership_mismatch: The durable parent job belongs to another project"
                        .into(),
                );
            }
        }
        let next_revision = current_revision + 1;
        let version_id = Uuid::new_v4().simple().to_string();
        transaction
            .execute(
                "INSERT INTO video_project_versions (id, project_id, revision, schema_version, manifest_json, manifest_sha256, base_revision, actor, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![version_id, project_id, next_revision, schema_version, manifest_json, manifest_sha256, current_revision, actor, reason, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not save the timeline revision: {error}"))?;
        transaction
            .execute(
                "UPDATE projects SET document_json = ?2, current_revision = ?3, updated_at = ?4 WHERE id = ?1",
                params![project_id, manifest_json, next_revision, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not update project state: {error}"))?;
        transaction
            .execute(
                "UPDATE video_projects SET status = ?2, aspect_ratio = ?3, duration_us = ?4, current_version_id = ?5, updated_at = ?6 WHERE project_id = ?1",
                params![project_id, status, aspect_ratio, duration_us, version_id, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not advance the timeline version: {error}"))?;
        transaction
            .execute(
                "INSERT INTO video_project_events (id, project_id, version_id, event_kind, payload_json, created_at)
                 VALUES (?1, ?2, ?3, 'timeline.revised', ?4, ?5)",
                params![Uuid::new_v4().simple().to_string(), project_id, version_id, json!({"actor": actor, "reason": reason, "base_revision": current_revision, "revision": next_revision}).to_string(), timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not record the timeline revision: {error}"))?;
        if complete_required_job {
            let required_job_id = required_active_job_id.ok_or_else(|| {
                "video.parent_job_inactive: The durable editor job is missing".to_string()
            })?;
            let changed = transaction
                .execute(
                    "UPDATE jobs
                     SET status = 'completed', progress = 1, error = NULL, updated_at = ?2
                     WHERE id = ?1 AND status IN ('queued', 'preparing', 'running')",
                    params![required_job_id, timestamp],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not complete the editor job: {error}")
                })?;
            if changed != 1 {
                return Err(
                    "video.parent_job_inactive: The durable editor job is no longer active".into(),
                );
            }
            insert_job_event(
                &transaction,
                required_job_id,
                "completed",
                1.0,
                None,
                &timestamp,
            )?;
        }
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the timeline revision: {error}")
        })?;
        drop(connection);
        if let Some(required_job_id) = required_active_job_id.filter(|_| complete_required_job) {
            self.clear_job_preview(required_job_id)?;
        }
        self.get_video_project(project_id)?
            .ok_or_else(|| "video.store_failed: The revised project could not be reloaded".into())
    }

    /// Commits one new canonical manifest version and publishes its complete derived output set
    /// in the same SQLite transaction. This is the crash boundary for multi-variation renders:
    /// callers can observe either the prior version with no new outputs, or the next version with
    /// every output attached, never an artifact-only intermediate revision.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_video_manifest_with_outputs(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
        outputs: &[Value],
    ) -> Result<Value, String> {
        self.commit_video_manifest_with_outputs_cancellable(
            project_id,
            expected_revision,
            manifest,
            actor,
            reason,
            lock_token,
            status,
            outputs,
            &NEVER_CANCEL_VIDEO_PUBLICATION,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn commit_video_manifest_with_outputs_cancellable(
        &self,
        project_id: &str,
        expected_revision: i64,
        manifest: &Value,
        actor: &str,
        reason: &str,
        lock_token: &str,
        status: Option<&str>,
        outputs: &[Value],
        cancel: &AtomicBool,
    ) -> Result<Value, String> {
        if !manifest.is_object() {
            return Err(
                "video.invalid_manifest: The timeline manifest must be a JSON object".into(),
            );
        }
        let schema_version = manifest
            .get("schema_version")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_manifest: schema_version must be a positive integer")?;
        let status = status.unwrap_or("draft");
        if !matches!(
            status,
            "draft"
                | "ingesting"
                | "analyzing"
                | "review"
                | "ready"
                | "rendering"
                | "completed"
                | "failed"
                | "archived"
        ) {
            return Err("video.invalid_status: Unsupported project status".into());
        }
        if outputs.is_empty() || outputs.len() > 64 {
            return Err(
                "video.invalid_output: Commit between one and 64 derived outputs atomically".into(),
            );
        }
        if expected_revision < 0 {
            return Err("video.invalid_revision: Expected revision cannot be negative".into());
        }
        if lock_token.trim().is_empty() || lock_token.len() > 256 {
            return Err("video.lock_lost: The project lease is missing or invalid".into());
        }
        for output in outputs {
            if required_trimmed(
                output,
                "project_id",
                "video.invalid_output: project_id is required",
            )? != project_id
            {
                return Err(
                    "video.ownership_mismatch: Every derived output must belong to the committed project"
                        .into(),
                );
            }
            if output
                .get("version_id")
                .is_some_and(|value| !value.is_null())
            {
                return Err("video.invalid_output: Outputs committed with a manifest must leave version_id unset for atomic binding".into());
            }
        }

        // Artifact hashing is intentionally outside SQLite, with lease heartbeat during each
        // potentially multi-gigabyte read. The final BEGIN IMMEDIATE transaction rechecks both
        // the live token and optimistic revision immediately before any durable write.
        let prepared = outputs
            .iter()
            .map(|output| self.prepare_video_output(output, lock_token, cancel))
            .collect::<Result<Vec<_>, _>>()?;
        ensure_video_publication_active(cancel)?;
        if prepared.iter().any(|output| !output.explicit_id) {
            return Err("video.output_identity_required: Cancellable atomic publication requires a stable output id".into());
        }
        if prepared.iter().any(|output| output.version_id.is_some()) {
            return Err("video.invalid_output: Derived outputs cannot preselect a version".into());
        }
        let primary_count = prepared.iter().filter(|output| output.is_primary).count();
        if primary_count > 1 {
            return Err(
                "video.invalid_output: An atomic output batch may contain only one primary master"
                    .into(),
            );
        }
        if prepared
            .iter()
            .any(|output| output.is_primary && output.kind != "master")
        {
            return Err("video.invalid_output: Only the canonical master may be primary".into());
        }
        for (index, output) in prepared.iter().enumerate() {
            if prepared[..index].iter().any(|prior| prior.id == output.id) {
                return Err(
                    "video.integrity_failed: An atomic output batch contains a duplicate output identity"
                        .into(),
                );
            }
        }

        let manifest_json = manifest.to_string();
        let manifest_sha256 = sha256_bytes(manifest_json.as_bytes());
        let timestamp = now();
        let duration_us = manifest_duration_us(manifest);
        let aspect_ratio = manifest_aspect_ratio(manifest);
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                format!("video.store_failed: Could not start atomic render commit: {error}")
            })?;
        ensure_video_publication_active(cancel)?;
        let active_lock: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM video_project_locks
                    WHERE project_id = ?1 AND token = ?2 AND lease_expires_at > ?3
                 )",
                params![project_id, lock_token, timestamp],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not validate the project lease: {error}")
            })?;
        if !active_lock {
            return Err(
                "video.lock_lost: The project lease expired or belongs to another editor".into(),
            );
        }
        let current_revision: i64 = transaction
            .query_row(
                "SELECT current_revision FROM projects
                 WHERE id = ?1 AND project_kind = 'video'",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the project revision: {error}")
            })?
            .ok_or("video.not_found: The video project was not found")?;
        if current_revision != expected_revision {
            return Err(format!(
                "video.revision_conflict: Expected revision {expected_revision}, but the project is at revision {current_revision}"
            ));
        }
        let next_revision = current_revision
            .checked_add(1)
            .ok_or("video.invalid_revision: Project revision overflow")?;
        let version_id = Uuid::new_v4().simple().to_string();
        transaction
            .execute(
                "INSERT INTO video_project_versions
                 (id, project_id, revision, schema_version, manifest_json, manifest_sha256,
                  base_revision, actor, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    version_id,
                    project_id,
                    next_revision,
                    schema_version,
                    manifest_json,
                    manifest_sha256,
                    current_revision,
                    actor,
                    reason,
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not save the timeline revision: {error}")
            })?;
        transaction
            .execute(
                "UPDATE projects
                 SET document_json = ?2, current_revision = ?3, updated_at = ?4 WHERE id = ?1",
                params![project_id, manifest_json, next_revision, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not update project state: {error}")
            })?;
        transaction
            .execute(
                "UPDATE video_projects
                 SET status = ?2, aspect_ratio = ?3, duration_us = ?4,
                     current_version_id = ?5, updated_at = ?6
                 WHERE project_id = ?1",
                params![
                    project_id,
                    status,
                    aspect_ratio,
                    duration_us,
                    version_id,
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not advance the timeline version: {error}")
            })?;
        transaction
            .execute(
                "INSERT INTO video_project_events
                 (id, project_id, version_id, event_kind, payload_json, created_at)
                 VALUES (?1, ?2, ?3, 'timeline.revised', ?4, ?5)",
                params![
                    Uuid::new_v4().simple().to_string(),
                    project_id,
                    version_id,
                    json!({
                        "actor": actor,
                        "reason": reason,
                        "base_revision": current_revision,
                        "revision": next_revision,
                        "outputs": prepared.len(),
                    })
                    .to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not record the timeline revision: {error}")
            })?;

        if primary_count == 1 {
            transaction
                .execute(
                    "UPDATE video_output_records SET is_primary = 0, updated_at = ?2
                     WHERE project_id = ?1 AND is_primary = 1",
                    params![project_id, timestamp],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not replace the project master: {error}")
                })?;
        }
        for output in &prepared {
            ensure_video_publication_active(cancel)?;
            let existing_id: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM video_output_records WHERE id = ?1)",
                    [&output.id],
                    |row| row.get(0),
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not inspect output identity: {error}")
                })?;
            if existing_id {
                return Err("video.ownership_mismatch: An output identity cannot move to a newly committed version".into());
            }
            transaction
                .execute(
                    "INSERT INTO video_output_records
                     (id, project_id, version_id, job_id, kind, label, artifact_path, mime_type,
                      size_bytes, sha256, duration_us, width, height, status, is_primary,
                      provenance_json, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                             'ready', ?14, ?15, ?16, ?16)",
                    params![
                        output.id,
                        project_id,
                        version_id,
                        output.job_id,
                        output.kind,
                        output.label,
                        output.artifact_path.to_string_lossy(),
                        output.mime_type,
                        output.size_bytes,
                        output.sha256,
                        output.duration_us,
                        output.width,
                        output.height,
                        output.is_primary,
                        output.provenance_json,
                        timestamp,
                    ],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not record the published video: {error}")
                })?;
        }
        ensure_video_publication_active(cancel)?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the rendered timeline: {error}")
        })?;
        drop(connection);
        self.get_video_project(project_id)?
            .ok_or_else(|| "video.store_failed: The rendered project could not be reloaded".into())
    }

    pub fn acquire_video_project_lock(
        &self,
        project_id: &str,
        owner: &str,
        lease_seconds: i64,
    ) -> Result<Value, String> {
        let lease_seconds = lease_seconds.clamp(15, 300);
        let timestamp = now();
        let expires_at = (Utc::now() + ChronoDuration::seconds(lease_seconds)).to_rfc3339();
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|error| {
            format!("video.store_failed: Could not start project locking: {error}")
        })?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM video_projects WHERE project_id = ?1)",
                [project_id],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the video project: {error}")
            })?;
        if !exists {
            return Err("video.not_found: The video project was not found".into());
        }
        transaction
            .execute(
                "DELETE FROM video_project_locks WHERE project_id = ?1 AND lease_expires_at <= ?2",
                params![project_id, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not clear an expired project lease: {error}")
            })?;
        let existing: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT token, owner, lease_expires_at FROM video_project_locks WHERE project_id = ?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("video.store_failed: Could not inspect the project lease: {error}"))?;
        let token = match existing {
            Some((token, current_owner, _)) if current_owner == owner => {
                transaction
                    .execute(
                        "UPDATE video_project_locks SET heartbeat_at = ?2, lease_expires_at = ?3 WHERE token = ?1",
                        params![token, timestamp, expires_at],
                    )
                    .map_err(|error| format!("video.store_failed: Could not renew the project lease: {error}"))?;
                token
            }
            Some((_, current_owner, current_expiry)) => {
                return Err(format!(
                    "video.project_locked: {current_owner} holds this project until {current_expiry}"
                ));
            }
            None => {
                let token = Uuid::new_v4().simple().to_string();
                transaction
                    .execute(
                        "INSERT INTO video_project_locks (project_id, token, owner, acquired_at, heartbeat_at, lease_expires_at)
                         VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                        params![project_id, token, owner, timestamp, expires_at],
                    )
                    .map_err(|error| format!("video.store_failed: Could not acquire the project lease: {error}"))?;
                token
            }
        };
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the project lease: {error}")
        })?;
        Ok(json!({
            "project_id": project_id,
            "token": token,
            "owner": owner,
            "lease_seconds": lease_seconds,
            "lease_expires_at": expires_at,
        }))
    }

    pub fn heartbeat_video_project_lock(
        &self,
        project_id: &str,
        token: &str,
        lease_seconds: i64,
    ) -> Result<Value, String> {
        let lease_seconds = lease_seconds.clamp(15, 300);
        let timestamp = now();
        let expires_at = (Utc::now() + ChronoDuration::seconds(lease_seconds)).to_rfc3339();
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE video_project_locks SET heartbeat_at = ?3, lease_expires_at = ?4
                 WHERE project_id = ?1 AND token = ?2 AND lease_expires_at > ?3",
                params![project_id, token, timestamp, expires_at],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not renew the project lease: {error}")
            })?;
        if changed == 0 {
            return Err(
                "video.lock_lost: The project lease expired or belongs to another editor".into(),
            );
        }
        Ok(json!({"project_id": project_id, "token": token, "lease_expires_at": expires_at}))
    }

    pub fn release_video_project_lock(
        &self,
        project_id: &str,
        token: &str,
    ) -> Result<bool, String> {
        let connection = self.lock()?;
        connection
            .execute(
                "DELETE FROM video_project_locks WHERE project_id = ?1 AND token = ?2",
                params![project_id, token],
            )
            .map(|changed| changed > 0)
            .map_err(|error| {
                format!("video.store_failed: Could not release the project lease: {error}")
            })
    }

    pub fn record_video_rights_receipt(
        &self,
        project_id: Option<&str>,
        canonical_url: &str,
        statement: &str,
        confirmed_by: &str,
    ) -> Result<Value, String> {
        let canonical_url = canonical_url.trim();
        if canonical_url.is_empty() || statement.trim().is_empty() {
            return Err(
                "video.rights_required: The exact URL and rights statement are required".into(),
            );
        }
        let id = Uuid::new_v4().simple().to_string();
        let url_sha256 = sha256_bytes(canonical_url.as_bytes());
        let timestamp = now();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO video_rights_receipts (id, project_id, canonical_url, url_sha256, assertion_version, statement, confirmed_by, confirmed_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7)
                 ON CONFLICT DO UPDATE SET
                    canonical_url = excluded.canonical_url,
                    statement = excluded.statement,
                    confirmed_by = excluded.confirmed_by,
                    confirmed_at = excluded.confirmed_at",
                params![id, project_id, canonical_url, url_sha256, statement.trim(), confirmed_by, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not record the rights confirmation: {error}"))?;
        let (saved_id, saved_confirmed_at): (String, String) = connection
            .query_row(
                "SELECT id, confirmed_at FROM video_rights_receipts
                 WHERE COALESCE(project_id, '') = COALESCE(?1, '')
                   AND url_sha256 = ?2
                   AND assertion_version = 1
                   AND revoked_at IS NULL",
                params![project_id, url_sha256],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not reload the rights confirmation: {error}")
            })?;
        Ok(json!({
            "id": saved_id,
            "project_id": project_id,
            "canonical_url": canonical_url,
            "url_sha256": url_sha256,
            "assertion_version": 1,
            "statement": statement.trim(),
            "confirmed_by": confirmed_by,
            "confirmed_at": saved_confirmed_at,
        }))
    }

    pub fn has_video_rights_receipt(
        &self,
        project_id: Option<&str>,
        canonical_url: &str,
    ) -> Result<bool, String> {
        let hash = sha256_bytes(canonical_url.trim().as_bytes());
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM video_rights_receipts
                 WHERE project_id IS ?1 AND url_sha256 = ?2 AND canonical_url = ?3 AND revoked_at IS NULL)",
                params![project_id, hash, canonical_url.trim()],
                |row| row.get(0),
            )
            .map_err(|error| format!("video.store_failed: Could not inspect rights confirmation: {error}"))
    }

    /// Persists an exact visual source authorization produced by a trusted native picker or
    /// authenticated generation broker. Callers never reconstruct these fields from an
    /// add-visual request: that request carries only the opaque receipt id.
    pub(crate) fn create_video_visual_source_receipt(
        &self,
        receipt: &Value,
    ) -> Result<Value, String> {
        let id = required_trimmed(
            receipt,
            "id",
            "video.invalid_visual_receipt: id is required",
        )?;
        let receipt_kind = required_trimmed(
            receipt,
            "receipt_kind",
            "video.invalid_visual_receipt: receipt_kind is required",
        )?;
        if !matches!(receipt_kind, "user_selected" | "generated_locally") {
            return Err("video.invalid_visual_receipt: Unsupported visual receipt kind".into());
        }
        let project_id = required_trimmed(
            receipt,
            "project_id",
            "video.invalid_visual_receipt: project_id is required",
        )?;
        let expected_revision = receipt
            .get("expected_revision")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_visual_receipt: expected_revision must be positive")?;
        let expected_version_id = required_trimmed(
            receipt,
            "expected_version_id",
            "video.invalid_visual_receipt: expected_version_id is required",
        )?;
        let source_path = required_trimmed(
            receipt,
            "source_path",
            "video.invalid_visual_receipt: source_path is required",
        )?;
        let source_device = required_trimmed(
            receipt,
            "source_device",
            "video.invalid_visual_receipt: source_device is required",
        )?;
        let source_inode = required_trimmed(
            receipt,
            "source_inode",
            "video.invalid_visual_receipt: source_inode is required",
        )?;
        let size_bytes = receipt
            .get("size_bytes")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_visual_receipt: size_bytes must be positive")?;
        let modified_seconds = receipt
            .get("modified_seconds")
            .and_then(Value::as_i64)
            .ok_or("video.invalid_visual_receipt: modified_seconds is required")?;
        let modified_nanoseconds = receipt
            .get("modified_nanoseconds")
            .and_then(Value::as_i64)
            .filter(|value| (0..1_000_000_000).contains(value))
            .ok_or(
                "video.invalid_visual_receipt: modified_nanoseconds must be within one second",
            )?;
        let sha256 = required_trimmed(
            receipt,
            "sha256",
            "video.invalid_visual_receipt: sha256 is required",
        )?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("video.invalid_visual_receipt: sha256 is invalid".into());
        }
        let mime_type = required_trimmed(
            receipt,
            "mime_type",
            "video.invalid_visual_receipt: mime_type is required",
        )?;
        if !matches!(mime_type, "image/png" | "image/jpeg" | "image/webp") {
            return Err("video.invalid_visual_receipt: mime_type is invalid".into());
        }
        let width = receipt
            .get("width")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_visual_receipt: width must be positive")?;
        let height = receipt
            .get("height")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .ok_or("video.invalid_visual_receipt: height must be positive")?;
        let has_alpha = receipt
            .get("has_alpha")
            .and_then(Value::as_bool)
            .ok_or("video.invalid_visual_receipt: has_alpha is required")?;
        let producer = required_trimmed(
            receipt,
            "producer",
            "video.invalid_visual_receipt: producer is required",
        )?;
        let producer_version = receipt.get("producer_version").and_then(Value::as_str);
        let generation_id = receipt.get("generation_id").and_then(Value::as_str);
        if receipt_kind == "generated_locally"
            && generation_id.is_none_or(|value| value.trim().is_empty())
        {
            return Err(
                "video.invalid_visual_receipt: generated receipts require generation_id".into(),
            );
        }
        if receipt_kind == "user_selected" && generation_id.is_some() {
            return Err(
                "video.invalid_visual_receipt: user selections cannot claim generation provenance"
                    .into(),
            );
        }
        let trust_context = receipt
            .get("trust_context")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if !trust_context.is_object() {
            return Err("video.invalid_visual_receipt: trust_context must be an object".into());
        }
        let issued_at = required_trimmed(
            receipt,
            "issued_at",
            "video.invalid_visual_receipt: issued_at is required",
        )?;
        let expires_at = required_trimmed(
            receipt,
            "expires_at",
            "video.invalid_visual_receipt: expires_at is required",
        )?;
        let issued_timestamp = chrono::DateTime::parse_from_rfc3339(issued_at)
            .map_err(|_| "video.invalid_visual_receipt: issued_at must be RFC 3339")?;
        let expires_timestamp = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| "video.invalid_visual_receipt: expires_at must be RFC 3339")?;
        if expires_timestamp <= issued_timestamp {
            return Err("video.invalid_visual_receipt: expires_at must follow issued_at".into());
        }

        let connection = self.lock()?;
        let current_matches: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM projects p
                    JOIN video_projects v ON v.project_id = p.id
                    WHERE p.id = ?1 AND p.project_kind = 'video'
                      AND p.current_revision = ?2 AND v.current_version_id = ?3
                 )",
                params![project_id, expected_revision, expected_version_id],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not bind the visual receipt to the current project: {error}")
            })?;
        if !current_matches {
            return Err(
                "video.revision_conflict: The visual receipt target is no longer current".into(),
            );
        }
        connection
            .execute(
                "INSERT INTO video_visual_source_receipts
                 (id, receipt_kind, project_id, expected_revision, expected_version_id,
                  source_path, source_device, source_inode, size_bytes, modified_seconds,
                  modified_nanoseconds, sha256, mime_type, width, height, has_alpha, producer,
                  producer_version, generation_id, trust_context_json, issued_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                params![
                    id,
                    receipt_kind,
                    project_id,
                    expected_revision,
                    expected_version_id,
                    source_path,
                    source_device,
                    source_inode,
                    size_bytes,
                    modified_seconds,
                    modified_nanoseconds,
                    sha256,
                    mime_type,
                    width,
                    height,
                    has_alpha,
                    producer,
                    producer_version,
                    generation_id,
                    trust_context.to_string(),
                    issued_at,
                    expires_at,
                ],
            )
            .map_err(|error| {
                format!("video.visual_receipt_conflict: Could not register the exact visual source: {error}")
            })?;
        drop(connection);
        self.get_video_visual_source_receipt(id)?
            .ok_or_else(|| "video.store_failed: The visual receipt could not be reloaded".into())
    }

    pub(crate) fn get_video_visual_source_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, receipt_kind, project_id, expected_revision, expected_version_id,
                        source_path, source_device, source_inode, size_bytes, modified_seconds,
                        modified_nanoseconds, sha256, mime_type, width, height, has_alpha, producer,
                        producer_version, generation_id, trust_context_json, issued_at, expires_at,
                        claimed_by_job_id, claimed_at
                 FROM video_visual_source_receipts WHERE id = ?1",
                [receipt_id],
                |row| {
                    let trust_context: String = row.get(19)?;
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "receipt_kind": row.get::<_, String>(1)?,
                        "project_id": row.get::<_, String>(2)?,
                        "expected_revision": row.get::<_, i64>(3)?,
                        "expected_version_id": row.get::<_, String>(4)?,
                        "source_path": row.get::<_, String>(5)?,
                        "source_device": row.get::<_, String>(6)?,
                        "source_inode": row.get::<_, String>(7)?,
                        "size_bytes": row.get::<_, i64>(8)?,
                        "modified_seconds": row.get::<_, i64>(9)?,
                        "modified_nanoseconds": row.get::<_, i64>(10)?,
                        "sha256": row.get::<_, String>(11)?,
                        "mime_type": row.get::<_, String>(12)?,
                        "width": row.get::<_, i64>(13)?,
                        "height": row.get::<_, i64>(14)?,
                        "has_alpha": row.get::<_, bool>(15)?,
                        "producer": row.get::<_, String>(16)?,
                        "producer_version": row.get::<_, Option<String>>(17)?,
                        "generation_id": row.get::<_, Option<String>>(18)?,
                        "trust_context": serde_json::from_str::<Value>(&trust_context).unwrap_or_else(|_| json!({})),
                        "issued_at": row.get::<_, String>(20)?,
                        "expires_at": row.get::<_, String>(21)?,
                        "claimed_by_job_id": row.get::<_, Option<String>>(22)?,
                        "claimed_at": row.get::<_, Option<String>>(23)?,
                    }))
                },
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not read the visual source receipt: {error}")
            })
    }

    /// Claims a receipt for exactly one durable add-visual job. A retry of that same job may
    /// resolve it again, but every other job, project, origin, revision, or version fails closed.
    pub(crate) fn claim_video_visual_source_receipt(
        &self,
        receipt_id: &str,
        receipt_kind: &str,
        project_id: &str,
        expected_revision: i64,
        expected_version_id: &str,
        job_id: &str,
    ) -> Result<Value, String> {
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                format!("video.store_failed: Could not start visual receipt claim: {error}")
            })?;
        let job_matches: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM jobs
                    WHERE id = ?1 AND kind = 'video_add_visual_asset'
                      AND status IN ('queued','preparing','running')
                      AND json_extract(request_json, '$.project_id') = ?2
                      AND json_extract(request_json, '$.expected_revision') = ?3
                      AND json_extract(request_json, '$.expected_version_id') = ?4
                      AND json_extract(request_json, '$.origin.kind') = ?5
                      AND json_extract(request_json, '$.origin.receipt_id') = ?6
                 )",
                params![
                    job_id,
                    project_id,
                    expected_revision,
                    expected_version_id,
                    receipt_kind,
                    receipt_id,
                ],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not validate the visual import job: {error}")
            })?;
        if !job_matches {
            return Err(
                "video.approval_required: The visual receipt is not attached to an active import job"
                    .into(),
            );
        }
        let receipt: Option<Value> = transaction
            .query_row(
                "SELECT id, receipt_kind, project_id, expected_revision, expected_version_id,
                        source_path, source_device, source_inode, size_bytes, modified_seconds,
                        modified_nanoseconds, sha256, mime_type, width, height, has_alpha, producer,
                        producer_version, generation_id, trust_context_json, issued_at, expires_at,
                        claimed_by_job_id, claimed_at
                 FROM video_visual_source_receipts WHERE id = ?1",
                [receipt_id],
                |row| {
                    let trust_context: String = row.get(19)?;
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "receipt_kind": row.get::<_, String>(1)?,
                        "project_id": row.get::<_, String>(2)?,
                        "expected_revision": row.get::<_, i64>(3)?,
                        "expected_version_id": row.get::<_, String>(4)?,
                        "source_path": row.get::<_, String>(5)?,
                        "source_device": row.get::<_, String>(6)?,
                        "source_inode": row.get::<_, String>(7)?,
                        "size_bytes": row.get::<_, i64>(8)?,
                        "modified_seconds": row.get::<_, i64>(9)?,
                        "modified_nanoseconds": row.get::<_, i64>(10)?,
                        "sha256": row.get::<_, String>(11)?,
                        "mime_type": row.get::<_, String>(12)?,
                        "width": row.get::<_, i64>(13)?,
                        "height": row.get::<_, i64>(14)?,
                        "has_alpha": row.get::<_, bool>(15)?,
                        "producer": row.get::<_, String>(16)?,
                        "producer_version": row.get::<_, Option<String>>(17)?,
                        "generation_id": row.get::<_, Option<String>>(18)?,
                        "trust_context": serde_json::from_str::<Value>(&trust_context).unwrap_or_else(|_| json!({})),
                        "issued_at": row.get::<_, String>(20)?,
                        "expires_at": row.get::<_, String>(21)?,
                        "claimed_by_job_id": row.get::<_, Option<String>>(22)?,
                        "claimed_at": row.get::<_, Option<String>>(23)?,
                    }))
                },
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the visual source receipt: {error}")
            })?;
        let receipt = receipt
            .ok_or("video.approval_required: A trusted visual source receipt is required")?;
        let exact_scope = receipt.get("receipt_kind").and_then(Value::as_str) == Some(receipt_kind)
            && receipt.get("project_id").and_then(Value::as_str) == Some(project_id)
            && receipt.get("expected_revision").and_then(Value::as_i64) == Some(expected_revision)
            && receipt.get("expected_version_id").and_then(Value::as_str)
                == Some(expected_version_id);
        if !exact_scope {
            return Err(
                "video.approval_required: The visual receipt does not authorize this exact project version and origin"
                    .into(),
            );
        }
        let claimed_by_job_id = receipt.get("claimed_by_job_id").and_then(Value::as_str);
        if claimed_by_job_id.is_some_and(|claimed| claimed != job_id) {
            return Err(
                "video.approval_required: The visual source receipt was already used".into(),
            );
        }
        if claimed_by_job_id.is_none() {
            let expires_at = receipt
                .get("expires_at")
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .ok_or("video.approval_required: The visual source receipt expiry is invalid")?;
            if expires_at <= Utc::now() {
                return Err("video.approval_required: The visual source receipt expired".into());
            }
        }
        transaction
            .execute(
                "UPDATE video_visual_source_receipts
                 SET claimed_by_job_id = ?2, claimed_at = COALESCE(claimed_at, ?3)
                 WHERE id = ?1 AND (claimed_by_job_id IS NULL OR claimed_by_job_id = ?2)",
                params![receipt_id, job_id, timestamp],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not claim the visual source receipt: {error}")
            })?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the visual source receipt claim: {error}")
        })?;
        Ok(receipt)
    }

    pub fn upsert_video_asset(&self, asset: &Value) -> Result<Value, String> {
        let project_id = required_trimmed(
            asset,
            "project_id",
            "video.invalid_asset: project_id is required",
        )?;
        let kind = required_trimmed(asset, "kind", "video.invalid_asset: kind is required")?;
        let source_kind = required_trimmed(
            asset,
            "source_kind",
            "video.invalid_asset: source_kind is required",
        )?;
        let status = asset
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("ready");
        let id = asset
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let managed_local_path = asset
            .get("local_path")
            .and_then(Value::as_str)
            .map(|path| self.validate_artifact_path(path))
            .transpose()?;
        let timestamp = now();
        let connection = self.lock()?;
        let existing_owner: Option<String> = connection
            .query_row(
                "SELECT project_id FROM video_media_assets WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not inspect media asset ownership: {error}")
            })?;
        if existing_owner
            .as_deref()
            .is_some_and(|owner| owner != project_id)
        {
            return Err(
                "video.ownership_mismatch: A media asset cannot move between projects".into(),
            );
        }
        connection
            .execute(
                "INSERT INTO video_media_assets
                 (id, project_id, kind, source_kind, local_path, original_url, mime_type, content_sha256, size_bytes, duration_us, status, probe_json, provenance_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)
                 ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, source_kind = excluded.source_kind,
                    local_path = excluded.local_path, original_url = excluded.original_url, mime_type = excluded.mime_type,
                    content_sha256 = excluded.content_sha256, size_bytes = excluded.size_bytes, duration_us = excluded.duration_us,
                    status = excluded.status, probe_json = excluded.probe_json, provenance_json = excluded.provenance_json,
                    updated_at = excluded.updated_at",
                params![
                    id,
                    project_id,
                    kind,
                    source_kind,
                    managed_local_path.as_ref().map(|path| path.to_string_lossy()),
                    asset.get("original_url").and_then(Value::as_str),
                    asset.get("mime_type").and_then(Value::as_str),
                    asset.get("content_sha256").and_then(Value::as_str),
                    asset.get("size_bytes").and_then(Value::as_i64),
                    asset.get("duration_us").and_then(Value::as_i64),
                    status,
                    asset.get("probe").cloned().unwrap_or_else(|| json!({})).to_string(),
                    asset.get("provenance").cloned().unwrap_or_else(|| json!({})).to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| format!("video.store_failed: Could not save the media asset: {error}"))?;
        drop(connection);
        self.list_video_assets(project_id)?
            .into_iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .ok_or_else(|| "video.store_failed: The saved asset could not be reloaded".into())
    }

    pub fn list_video_assets(&self, project_id: &str) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, kind, source_kind, local_path, original_url, mime_type, content_sha256,
                        size_bytes, duration_us, status, probe_json, provenance_json, created_at, updated_at
                 FROM video_media_assets WHERE project_id = ?1 ORDER BY created_at, id",
            )
            .map_err(|error| format!("video.store_failed: Could not prepare media assets: {error}"))?;
        let assets = statement
            .query_map([project_id], |row| {
                let probe: String = row.get(10)?;
                let provenance: String = row.get(11)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "project_id": project_id,
                    "kind": row.get::<_, String>(1)?,
                    "source_kind": row.get::<_, String>(2)?,
                    "local_path": row.get::<_, Option<String>>(3)?,
                    "original_url": row.get::<_, Option<String>>(4)?,
                    "mime_type": row.get::<_, Option<String>>(5)?,
                    "content_sha256": row.get::<_, Option<String>>(6)?,
                    "size_bytes": row.get::<_, Option<i64>>(7)?,
                    "duration_us": row.get::<_, Option<i64>>(8)?,
                    "status": row.get::<_, String>(9)?,
                    "probe": serde_json::from_str::<Value>(&probe).unwrap_or_else(|_| json!({})),
                    "provenance": serde_json::from_str::<Value>(&provenance).unwrap_or_else(|_| json!({})),
                    "created_at": row.get::<_, String>(12)?,
                    "updated_at": row.get::<_, String>(13)?,
                }))
            })
            .map_err(|error| format!("video.store_failed: Could not list media assets: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("video.store_failed: Could not read media assets: {error}"))?;
        Ok(assets)
    }

    #[cfg(test)]
    pub(crate) fn publish_video_output(&self, output: &Value) -> Result<Value, String> {
        let project_id = required_trimmed(
            output,
            "project_id",
            "video.invalid_output: project_id is required",
        )?;
        let kind = required_trimmed(output, "kind", "video.invalid_output: kind is required")?;
        let label = required_trimmed(output, "label", "video.invalid_output: label is required")?;
        let version_id = required_trimmed(
            output,
            "version_id",
            "video.invalid_output: version_id is required",
        )?;
        let raw_path = required_trimmed(
            output,
            "artifact_path",
            "video.invalid_output: artifact_path is required",
        )?;
        let path = self.validate_artifact_path(raw_path)?;
        if !path.is_file() {
            return Err("video.invalid_output: The published artifact does not exist".into());
        }
        let size_bytes = fs::metadata(&path)
            .map_err(|error| {
                format!("video.invalid_output: Could not inspect the published artifact: {error}")
            })?
            .len() as i64;
        let checksum = sha256_file(&path)?;
        if let Some(expected) = output.get("sha256").and_then(Value::as_str) {
            if expected != checksum {
                return Err(
                    "video.integrity_failed: The published artifact checksum did not match".into(),
                );
            }
        }
        let id = output
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let primary = output
            .get("is_primary")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(|error| {
            format!("video.store_failed: Could not publish the video output: {error}")
        })?;
        let (version_owned, version_current): (bool, bool) = transaction
            .query_row(
                "SELECT
                    EXISTS(SELECT 1 FROM video_project_versions WHERE id = ?2 AND project_id = ?1),
                    EXISTS(SELECT 1 FROM video_projects WHERE project_id = ?1 AND current_version_id = ?2)",
                params![project_id, version_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not verify output version ownership: {error}")
            })?;
        if !version_owned {
            return Err(
                "video.ownership_mismatch: The output version does not belong to this project"
                    .into(),
            );
        }
        if primary && (!version_current || kind != "master") {
            return Err("video.stale_output: Only a master for the current project version can be promoted as primary".into());
        }
        if primary {
            transaction
                .execute(
                    "UPDATE video_output_records SET is_primary = 0, updated_at = ?2 WHERE project_id = ?1 AND is_primary = 1",
                    params![project_id, timestamp],
                )
                .map_err(|error| format!("video.store_failed: Could not replace the project master: {error}"))?;
        }
        transaction
            .execute(
                "INSERT INTO video_output_records
                 (id, project_id, version_id, job_id, kind, label, artifact_path, mime_type, size_bytes, sha256, duration_us, width, height, status, is_primary, provenance_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'ready', ?14, ?15, ?16, ?16)",
                params![
                    id,
                    project_id,
                    version_id,
                    output.get("job_id").and_then(Value::as_str),
                    kind,
                    label,
                    path.to_string_lossy(),
                    output.get("mime_type").and_then(Value::as_str).unwrap_or("video/mp4"),
                    size_bytes,
                    checksum,
                    output.get("duration_us").and_then(Value::as_i64),
                    output.get("width").and_then(Value::as_i64),
                    output.get("height").and_then(Value::as_i64),
                    primary,
                    output.get("provenance").cloned().unwrap_or_else(|| json!({})).to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| format!("video.store_failed: Could not record the published video: {error}"))?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit the published video: {error}")
        })?;
        drop(connection);
        self.list_video_outputs(project_id)?
            .into_iter()
            .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .ok_or_else(|| "video.store_failed: The published video could not be reloaded".into())
    }

    /// Atomically publishes one integrity-checked output only while the caller still owns the
    /// live project lease and the exact reviewed revision/version remains current.
    pub fn publish_video_output_current(
        &self,
        output: &Value,
        expected_revision: i64,
        expected_version_id: &str,
        lock_token: &str,
    ) -> Result<Value, String> {
        self.publish_video_outputs_current(
            std::slice::from_ref(output),
            expected_revision,
            expected_version_id,
            lock_token,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| "video.store_failed: The published video could not be reloaded".into())
    }

    pub fn publish_video_output_current_cancellable(
        &self,
        output: &Value,
        expected_revision: i64,
        expected_version_id: &str,
        lock_token: &str,
        cancel: &AtomicBool,
    ) -> Result<Value, String> {
        self.publish_video_outputs_current_cancellable(
            std::slice::from_ref(output),
            expected_revision,
            expected_version_id,
            lock_token,
            cancel,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| "video.store_failed: The published video could not be reloaded".into())
    }

    /// Batch form used by variation renders. Artifact inspection and hashing happen before the
    /// database transaction; the transaction then rechecks the live lease and optimistic project
    /// expectation before adopting/inserting every row. Any failure rolls the complete batch back.
    pub fn publish_video_outputs_current(
        &self,
        outputs: &[Value],
        expected_revision: i64,
        expected_version_id: &str,
        lock_token: &str,
    ) -> Result<Vec<Value>, String> {
        self.publish_video_outputs_current_cancellable(
            outputs,
            expected_revision,
            expected_version_id,
            lock_token,
            &NEVER_CANCEL_VIDEO_PUBLICATION,
        )
    }

    pub fn publish_video_outputs_current_cancellable(
        &self,
        outputs: &[Value],
        expected_revision: i64,
        expected_version_id: &str,
        lock_token: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<Value>, String> {
        if outputs.is_empty() || outputs.len() > 64 {
            return Err(
                "video.invalid_output: Publish between one and 64 outputs atomically".into(),
            );
        }
        if expected_revision < 0 || expected_version_id.trim().is_empty() {
            return Err(
                "video.invalid_output: A current project revision and version are required".into(),
            );
        }
        if lock_token.trim().is_empty() || lock_token.len() > 256 {
            return Err("video.lock_lost: The project lease is missing or invalid".into());
        }

        // Do every filesystem operation before taking the SQLite write lock. This keeps lease and
        // version checks as the final publication boundary rather than a stale pre-hash snapshot.
        let prepared = outputs
            .iter()
            .map(|output| self.prepare_video_output(output, lock_token, cancel))
            .collect::<Result<Vec<_>, _>>()?;
        ensure_video_publication_active(cancel)?;
        if prepared.iter().any(|output| !output.explicit_id) {
            return Err("video.output_identity_required: Cancellable atomic publication requires a stable output id".into());
        }
        let project_id = prepared[0].project_id.as_str();
        if prepared
            .iter()
            .any(|output| output.project_id != project_id)
        {
            return Err(
                "video.ownership_mismatch: An output batch cannot span video projects".into(),
            );
        }
        if prepared
            .iter()
            .any(|output| output.version_id.as_deref() != Some(expected_version_id))
        {
            return Err("video.revision_conflict: Every output must target the expected current video version".into());
        }
        let primary_count = prepared.iter().filter(|output| output.is_primary).count();
        if primary_count > 1 {
            return Err(
                "video.invalid_output: An atomic output batch may contain only one primary master"
                    .into(),
            );
        }
        if prepared
            .iter()
            .any(|output| output.is_primary && output.kind != "master")
        {
            return Err("video.invalid_output: Only the canonical master may be primary".into());
        }

        let timestamp = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                format!("video.store_failed: Could not start atomic output publication: {error}")
            })?;
        ensure_video_publication_active(cancel)?;
        let active_lock: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM video_project_locks
                    WHERE project_id = ?1 AND token = ?2 AND lease_expires_at > ?3
                 )",
                params![project_id, lock_token, timestamp],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not validate the project lease: {error}")
            })?;
        if !active_lock {
            return Err(
                "video.lock_lost: The project lease expired or belongs to another editor".into(),
            );
        }
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT p.current_revision, v.current_version_id
                 FROM projects p JOIN video_projects v ON v.project_id = p.id
                 WHERE p.id = ?1 AND p.project_kind = 'video'",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the current video version: {error}")
            })?;
        let Some((current_revision, current_version_id)) = current else {
            return Err("video.not_found: The video project was not found".into());
        };
        if current_revision != expected_revision || current_version_id != expected_version_id {
            return Err(format!(
                "video.revision_conflict: Expected revision {expected_revision} ({expected_version_id}), but the project is at revision {current_revision} ({current_version_id})"
            ));
        }

        if primary_count == 1 {
            transaction
                .execute(
                    "UPDATE video_output_records SET is_primary = 0, updated_at = ?2
                     WHERE project_id = ?1 AND is_primary = 1",
                    params![project_id, timestamp],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not replace the project master: {error}")
                })?;
        }

        let mut published_ids = Vec::with_capacity(prepared.len());
        for output in &prepared {
            ensure_video_publication_active(cancel)?;
            let existing_by_id: Option<(String, Option<String>, String, String)> = transaction
                .query_row(
                    "SELECT project_id, version_id, kind, sha256
                     FROM video_output_records WHERE id = ?1",
                    [&output.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(|error| {
                    format!("video.store_failed: Could not inspect output identity: {error}")
                })?;
            let adopted_id = if let Some((owner, version, kind, sha256)) = existing_by_id {
                if owner != output.project_id || version.as_deref() != output.version_id.as_deref()
                {
                    return Err(
                        "video.ownership_mismatch: An output identity cannot move between project versions"
                            .into(),
                    );
                }
                if kind != output.kind || sha256 != output.sha256 {
                    return Err("video.integrity_failed: An output identity is already bound to different content".into());
                }
                Some(output.id.clone())
            } else {
                None
            };
            if let Some(adopted_id) = adopted_id {
                // The prepared path was just revalidated and rehashed. Rebind every presentation
                // field so adopting a stable semantic id can repair a deleted/tampered old path
                // instead of returning an opaque, unplayable row.
                transaction
                    .execute(
                        "UPDATE video_output_records
                         SET job_id = ?2, label = ?3, artifact_path = ?4, mime_type = ?5,
                             size_bytes = ?6, duration_us = ?7, width = ?8, height = ?9,
                             status = 'ready', is_primary = ?10, provenance_json = ?11,
                             updated_at = ?12
                         WHERE id = ?1",
                        params![
                            adopted_id,
                            output.job_id,
                            output.label,
                            output.artifact_path.to_string_lossy(),
                            output.mime_type,
                            output.size_bytes,
                            output.duration_us,
                            output.width,
                            output.height,
                            output.is_primary,
                            output.provenance_json,
                            timestamp,
                        ],
                    )
                    .map_err(|error| {
                        format!("video.store_failed: Could not refresh the adopted output: {error}")
                    })?;
                published_ids.push(adopted_id);
                continue;
            }

            transaction
                .execute(
                    "INSERT INTO video_output_records
                     (id, project_id, version_id, job_id, kind, label, artifact_path, mime_type,
                      size_bytes, sha256, duration_us, width, height, status, is_primary,
                      provenance_json, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                             'ready', ?14, ?15, ?16, ?16)",
                    params![
                        output.id,
                        output.project_id,
                        output.version_id,
                        output.job_id,
                        output.kind,
                        output.label,
                        output.artifact_path.to_string_lossy(),
                        output.mime_type,
                        output.size_bytes,
                        output.sha256,
                        output.duration_us,
                        output.width,
                        output.height,
                        output.is_primary,
                        output.provenance_json,
                        timestamp,
                    ],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not record the published video: {error}")
                })?;
            published_ids.push(output.id.clone());
        }
        ensure_video_publication_active(cancel)?;
        transaction.commit().map_err(|error| {
            format!("video.store_failed: Could not commit atomic output publication: {error}")
        })?;
        drop(connection);

        let saved = self.list_video_outputs(project_id)?;
        published_ids
            .into_iter()
            .map(|id| {
                saved
                    .iter()
                    .find(|output| output.get("id").and_then(Value::as_str) == Some(id.as_str()))
                    .cloned()
                    .ok_or_else(|| {
                        "video.store_failed: A published video could not be reloaded".into()
                    })
            })
            .collect()
    }

    fn prepare_video_output(
        &self,
        output: &Value,
        lock_token: &str,
        cancel: &AtomicBool,
    ) -> Result<PreparedVideoOutput, String> {
        let project_id = required_trimmed(
            output,
            "project_id",
            "video.invalid_output: project_id is required",
        )?
        .to_string();
        let version_id = output
            .get("version_id")
            .filter(|value| !value.is_null())
            .map(|_| {
                required_trimmed(
                    output,
                    "version_id",
                    "video.invalid_output: version_id must be non-empty when supplied",
                )
                .map(str::to_string)
            })
            .transpose()?;
        let kind =
            required_trimmed(output, "kind", "video.invalid_output: kind is required")?.to_string();
        if !matches!(
            kind.as_str(),
            "preview" | "master" | "variation" | "publish-package" | "subtitle" | "thumbnail"
        ) {
            return Err("video.invalid_output: Unsupported video output kind".into());
        }
        let label = required_trimmed(output, "label", "video.invalid_output: label is required")?
            .to_string();
        if label.len() > 512 {
            return Err("video.invalid_output: Output labels are limited to 512 bytes".into());
        }
        let raw_path = required_trimmed(
            output,
            "artifact_path",
            "video.invalid_output: artifact_path is required",
        )?;
        let raw_metadata = fs::symlink_metadata(raw_path).map_err(|error| {
            format!("video.invalid_output: Could not inspect the published artifact: {error}")
        })?;
        if raw_metadata.file_type().is_symlink()
            || !raw_metadata.is_file()
            || raw_metadata.nlink() != 1
        {
            return Err("video.invalid_output: Published artifacts must be ordinary, privately owned regular files".into());
        }
        let artifact_path = self.validate_artifact_path(raw_path)?;
        secure_private_file(&artifact_path, "published video artifact")?;
        let metadata = fs::metadata(&artifact_path).map_err(|error| {
            format!("video.invalid_output: Could not inspect the published artifact: {error}")
        })?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(
                "video.invalid_output: Published artifacts must be ordinary single-link files"
                    .into(),
            );
        }
        let size_bytes = i64::try_from(metadata.len())
            .map_err(|_| "video.invalid_output: The published artifact is too large")?;
        if output
            .get("size_bytes")
            .and_then(Value::as_i64)
            .is_some_and(|expected| expected != size_bytes)
        {
            return Err("video.integrity_failed: The published artifact size did not match".into());
        }
        let sha256 =
            self.sha256_video_output_with_lease(&artifact_path, &project_id, lock_token, cancel)?;
        if output
            .get("sha256")
            .and_then(Value::as_str)
            .is_some_and(|expected| expected != sha256)
        {
            return Err(
                "video.integrity_failed: The published artifact checksum did not match".into(),
            );
        }
        let explicit_id = output
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        let id = output
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        if id.len() > 256 {
            return Err("video.invalid_output: Output identity is too long".into());
        }
        let mime_type = output
            .get("mime_type")
            .and_then(Value::as_str)
            .unwrap_or("video/mp4")
            .trim()
            .to_string();
        if mime_type.is_empty() || mime_type.len() > 128 {
            return Err("video.invalid_output: Output MIME type is invalid".into());
        }
        let provenance_json = output
            .get("provenance")
            .cloned()
            .unwrap_or_else(|| json!({}))
            .to_string();
        if provenance_json.len() > 1_048_576 {
            return Err("video.invalid_output: Output provenance exceeds 1 MiB".into());
        }
        Ok(PreparedVideoOutput {
            id,
            explicit_id,
            project_id,
            version_id,
            job_id: output
                .get("job_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            kind,
            label,
            artifact_path,
            mime_type,
            size_bytes,
            sha256,
            duration_us: output.get("duration_us").and_then(Value::as_i64),
            width: output.get("width").and_then(Value::as_i64),
            height: output.get("height").and_then(Value::as_i64),
            is_primary: output
                .get("is_primary")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            provenance_json,
        })
    }

    fn sha256_video_output_with_lease(
        &self,
        path: &Path,
        project_id: &str,
        lock_token: &str,
        cancel: &AtomicBool,
    ) -> Result<String, String> {
        // Hashing a large local master/package can outlive the ordinary 120-second service lease.
        // Renew during the read, then still recheck the token and current version inside the final
        // write transaction. No correctness decision relies on this preflight heartbeat alone.
        ensure_video_publication_active(cancel)?;
        self.heartbeat_video_project_lock(project_id, lock_token, 300)?;
        let mut file = fs::File::open(path)
            .map_err(|error| format!("Could not checksum the published video: {error}"))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut heartbeat_at = std::time::Instant::now();
        loop {
            ensure_video_publication_active(cancel)?;
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("Could not checksum the published video: {error}"))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            if heartbeat_at.elapsed() >= std::time::Duration::from_secs(10) {
                self.heartbeat_video_project_lock(project_id, lock_token, 300)?;
                heartbeat_at = std::time::Instant::now();
            }
        }
        ensure_video_publication_active(cancel)?;
        self.heartbeat_video_project_lock(project_id, lock_token, 300)?;
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn list_video_outputs(&self, project_id: &str) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, version_id, job_id, kind, label, artifact_path, mime_type, size_bytes, sha256,
                        duration_us, width, height, status, is_primary, provenance_json, created_at, updated_at
                 FROM video_output_records WHERE project_id = ?1 ORDER BY is_primary DESC, created_at DESC, id DESC",
            )
            .map_err(|error| format!("video.store_failed: Could not prepare video outputs: {error}"))?;
        let outputs = statement
            .query_map([project_id], |row| {
                let provenance: String = row.get(14)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "project_id": project_id,
                    "version_id": row.get::<_, Option<String>>(1)?,
                    "job_id": row.get::<_, Option<String>>(2)?,
                    "kind": row.get::<_, String>(3)?,
                    "label": row.get::<_, String>(4)?,
                    "artifact_path": row.get::<_, String>(5)?,
                    "mime_type": row.get::<_, String>(6)?,
                    "size_bytes": row.get::<_, i64>(7)?,
                    "sha256": row.get::<_, String>(8)?,
                    "duration_us": row.get::<_, Option<i64>>(9)?,
                    "width": row.get::<_, Option<i64>>(10)?,
                    "height": row.get::<_, Option<i64>>(11)?,
                    "status": row.get::<_, String>(12)?,
                    "is_primary": row.get::<_, bool>(13)?,
                    "provenance": serde_json::from_str::<Value>(&provenance).unwrap_or_else(|_| json!({})),
                    "created_at": row.get::<_, String>(15)?,
                    "updated_at": row.get::<_, String>(16)?,
                }))
            })
            .map_err(|error| format!("video.store_failed: Could not list video outputs: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("video.store_failed: Could not read video outputs: {error}"))?;
        Ok(outputs)
    }

    pub fn upsert_video_stage(&self, stage: &Value) -> Result<Value, String> {
        let project_id = required_trimmed(
            stage,
            "project_id",
            "video.invalid_stage: project_id is required",
        )?;
        let stage_key = required_trimmed(
            stage,
            "stage_key",
            "video.invalid_stage: stage_key is required",
        )?;
        let version_id = required_trimmed(
            stage,
            "version_id",
            "video.invalid_stage: version_id is required",
        )?;
        let scope_key = stage.get("scope_key").and_then(Value::as_str).unwrap_or("");
        let status = stage
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("queued");
        let resource_class = stage
            .get("resource_class")
            .and_then(Value::as_str)
            .unwrap_or("light");
        let input_sha256 = required_trimmed(
            stage,
            "input_sha256",
            "video.invalid_stage: input_sha256 is required",
        )?;
        let timestamp = now();
        let id = stage
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let connection = self.lock()?;
        let version_owned: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM video_project_versions WHERE id = ?2 AND project_id = ?1)",
                params![project_id, version_id],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not verify stage version ownership: {error}")
            })?;
        if !version_owned {
            return Err(
                "video.ownership_mismatch: The workflow stage version does not belong to this project"
                    .into(),
            );
        }
        connection
            .execute(
                "INSERT INTO video_workflow_stages
                 (id, project_id, version_id, stage_key, scope_key, job_id, status, resource_class, attempt, progress, input_sha256, output_sha256, checkpoint_json, error_json, started_at, completed_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         CASE WHEN ?7 = 'running' THEN ?15 ELSE NULL END,
                         CASE WHEN ?7 = 'completed' THEN ?15 ELSE NULL END, ?15, ?15)
                 ON CONFLICT(project_id, version_id, stage_key, scope_key) DO UPDATE SET
                    job_id = COALESCE(excluded.job_id, video_workflow_stages.job_id),
                    status = excluded.status, resource_class = excluded.resource_class,
                    attempt = excluded.attempt, progress = excluded.progress,
                    input_sha256 = excluded.input_sha256, output_sha256 = excluded.output_sha256,
                    checkpoint_json = excluded.checkpoint_json, error_json = excluded.error_json,
                    started_at = CASE WHEN excluded.status = 'running' THEN COALESCE(video_workflow_stages.started_at, excluded.updated_at) ELSE video_workflow_stages.started_at END,
                    completed_at = CASE WHEN excluded.status = 'completed' THEN excluded.updated_at ELSE NULL END,
                    updated_at = excluded.updated_at",
                params![
                    id,
                    project_id,
                    version_id,
                    stage_key,
                    scope_key,
                    stage.get("job_id").and_then(Value::as_str),
                    status,
                    resource_class,
                    stage.get("attempt").and_then(Value::as_i64).unwrap_or(0),
                    stage.get("progress").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0),
                    input_sha256,
                    stage.get("output_sha256").and_then(Value::as_str),
                    stage.get("checkpoint").cloned().unwrap_or_else(|| json!({})).to_string(),
                    stage.get("error").map(Value::to_string),
                    timestamp,
                ],
            )
            .map_err(|error| format!("video.store_failed: Could not checkpoint the video stage: {error}"))?;
        drop(connection);
        self.list_video_stages(project_id)?
            .into_iter()
            .find(|item| {
                item.get("stage_key").and_then(Value::as_str) == Some(stage_key)
                    && item.get("scope_key").and_then(Value::as_str) == Some(scope_key)
                    && item.get("version_id").and_then(Value::as_str) == Some(version_id)
            })
            .ok_or_else(|| "video.store_failed: The saved stage could not be reloaded".into())
    }

    pub fn list_video_stages(&self, project_id: &str) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, version_id, stage_key, scope_key, job_id, status, resource_class, attempt,
                        progress, input_sha256, output_sha256, checkpoint_json, error_json,
                        started_at, completed_at, created_at, updated_at
                 FROM video_workflow_stages WHERE project_id = ?1 ORDER BY created_at, stage_key, scope_key",
            )
            .map_err(|error| format!("video.store_failed: Could not prepare video stages: {error}"))?;
        let stages = statement
            .query_map([project_id], |row| {
                let checkpoint: String = row.get(11)?;
                let error: Option<String> = row.get(12)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "project_id": project_id,
                    "version_id": row.get::<_, Option<String>>(1)?,
                    "stage_key": row.get::<_, String>(2)?,
                    "scope_key": row.get::<_, String>(3)?,
                    "job_id": row.get::<_, Option<String>>(4)?,
                    "status": row.get::<_, String>(5)?,
                    "resource_class": row.get::<_, String>(6)?,
                    "attempt": row.get::<_, i64>(7)?,
                    "progress": row.get::<_, f64>(8)?,
                    "input_sha256": row.get::<_, String>(9)?,
                    "output_sha256": row.get::<_, Option<String>>(10)?,
                    "checkpoint": serde_json::from_str::<Value>(&checkpoint).unwrap_or_else(|_| json!({})),
                    "error": error.and_then(|value| serde_json::from_str::<Value>(&value).ok()),
                    "started_at": row.get::<_, Option<String>>(13)?,
                    "completed_at": row.get::<_, Option<String>>(14)?,
                    "created_at": row.get::<_, String>(15)?,
                    "updated_at": row.get::<_, String>(16)?,
                }))
            })
            .map_err(|error| format!("video.store_failed: Could not list video stages: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("video.store_failed: Could not read video stages: {error}"))?;
        Ok(stages)
    }

    pub fn get_video_cache(&self, cache_key: &str) -> Result<Option<Value>, String> {
        let record: Option<(
            String,
            String,
            Option<String>,
            String,
            String,
            String,
            i64,
            i64,
            String,
            String,
        )> = {
            let connection = self.lock()?;
            connection
                .query_row(
                    "SELECT cache_key, namespace, project_id, input_json, artifact_path, artifact_sha256,
                            size_bytes, hit_count, created_at, last_used_at
                     FROM video_cache_entries WHERE cache_key = ?1",
                    [cache_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?)),
                )
                .optional()
                .map_err(|error| format!("video.store_failed: Could not inspect the render cache: {error}"))?
        };
        let Some((
            key,
            namespace,
            project_id,
            input_json,
            path,
            checksum,
            size_bytes,
            hit_count,
            created_at,
            _,
        )) = record
        else {
            return Ok(None);
        };
        let valid = self
            .validate_artifact_path(&path)
            .and_then(|path| sha256_file(&path))
            .is_ok_and(|actual| actual == checksum);
        let connection = self.lock()?;
        if !valid {
            connection
                .execute(
                    "DELETE FROM video_cache_entries WHERE cache_key = ?1",
                    [cache_key],
                )
                .map_err(|error| {
                    format!("video.store_failed: Could not discard an invalid cache entry: {error}")
                })?;
            return Ok(None);
        }
        let last_used_at = now();
        connection
            .execute(
                "UPDATE video_cache_entries SET hit_count = hit_count + 1, last_used_at = ?2 WHERE cache_key = ?1",
                params![cache_key, last_used_at],
            )
            .map_err(|error| format!("video.store_failed: Could not update render cache usage: {error}"))?;
        Ok(Some(json!({
            "cache_key": key,
            "namespace": namespace,
            "project_id": project_id,
            "input": serde_json::from_str::<Value>(&input_json).unwrap_or_else(|_| json!({})),
            "artifact_path": path,
            "artifact_sha256": checksum,
            "size_bytes": size_bytes,
            "hit_count": hit_count + 1,
            "created_at": created_at,
            "last_used_at": last_used_at,
        })))
    }

    pub fn put_video_cache(
        &self,
        cache_key: &str,
        namespace: &str,
        project_id: Option<&str>,
        input: &Value,
        artifact_path: &Path,
    ) -> Result<Value, String> {
        let managed = self.validate_artifact_path(&artifact_path.to_string_lossy())?;
        let checksum = sha256_file(&managed)?;
        let size_bytes = fs::metadata(&managed)
            .map_err(|error| {
                format!("video.store_failed: Could not inspect the cached artifact: {error}")
            })?
            .len() as i64;
        let timestamp = now();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO video_cache_entries
                 (cache_key, namespace, project_id, input_json, artifact_path, artifact_sha256, size_bytes, hit_count, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?8)
                 ON CONFLICT(cache_key) DO UPDATE SET namespace = excluded.namespace, project_id = excluded.project_id,
                    input_json = excluded.input_json, artifact_path = excluded.artifact_path,
                    artifact_sha256 = excluded.artifact_sha256, size_bytes = excluded.size_bytes,
                    hit_count = 0, created_at = excluded.created_at, last_used_at = excluded.last_used_at",
                params![cache_key, namespace, project_id, input.to_string(), managed.to_string_lossy(), checksum, size_bytes, timestamp],
            )
            .map_err(|error| format!("video.store_failed: Could not save the render cache entry: {error}"))?;
        Ok(json!({
            "cache_key": cache_key,
            "namespace": namespace,
            "project_id": project_id,
            "input": input,
            "artifact_path": managed,
            "artifact_sha256": checksum,
            "size_bytes": size_bytes,
            "hit_count": 0,
            "created_at": timestamp,
            "last_used_at": timestamp,
        }))
    }

    pub fn record_video_performance(&self, sample: &Value) -> Result<Value, String> {
        let operation = required_trimmed(
            sample,
            "operation",
            "video.invalid_metric: operation is required",
        )?;
        let wall_seconds = sample
            .get("wall_seconds")
            .and_then(Value::as_f64)
            .filter(|value| *value >= 0.0)
            .ok_or("video.invalid_metric: wall_seconds must be non-negative")?;
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO video_performance_samples
                 (id, project_id, job_id, operation, profile, wall_seconds, media_seconds, realtime_factor, gpu_peak_mb, cache_hit, details_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    id,
                    sample.get("project_id").and_then(Value::as_str),
                    sample.get("job_id").and_then(Value::as_str),
                    operation,
                    sample.get("profile").and_then(Value::as_str).unwrap_or("default"),
                    wall_seconds,
                    sample.get("media_seconds").and_then(Value::as_f64),
                    sample.get("realtime_factor").and_then(Value::as_f64),
                    sample.get("gpu_peak_mb").and_then(Value::as_i64),
                    sample.get("cache_hit").and_then(Value::as_bool).unwrap_or(false),
                    sample.get("details").cloned().unwrap_or_else(|| json!({})).to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| format!("video.store_failed: Could not save the performance sample: {error}"))?;
        Ok(
            json!({"id": id, "operation": operation, "wall_seconds": wall_seconds, "created_at": timestamp}),
        )
    }

    pub fn link_assistant_video_artifact(&self, link: &Value) -> Result<Value, String> {
        let thread_id = required_trimmed(
            link,
            "thread_id",
            "video.invalid_link: thread_id is required",
        )?;
        let project_id = required_trimmed(
            link,
            "project_id",
            "video.invalid_link: project_id is required",
        )?;
        let relationship = link
            .get("relationship")
            .and_then(Value::as_str)
            .unwrap_or("project");
        if thread_id.len() > 512 || project_id.len() > 256 {
            return Err(
                "video.invalid_link: assistant relationship identifiers are too long".into(),
            );
        }
        if !matches!(
            relationship,
            "project" | "preview" | "master" | "variation" | "publish-package"
        ) {
            return Err("video.invalid_link: assistant relationship is unsupported".into());
        }
        let turn_id = required_trimmed(link, "turn_id", "video.invalid_link: turn_id is required")?;
        let item_id = required_trimmed(link, "item_id", "video.invalid_link: item_id is required")?;
        if turn_id.len() > 512 || item_id.len() > 512 {
            return Err("video.invalid_link: assistant turn or call identity is too long".into());
        }
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let connection = self.lock()?;
        let output_id = link
            .get("output_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if output_id.is_none() && relationship != "project" {
            return Err(
                "video.invalid_link: an output relationship requires an exact output".into(),
            );
        }
        if output_id.is_some() && relationship == "project" {
            return Err("video.invalid_link: a project relationship cannot name an output".into());
        }
        let project_exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM video_projects WHERE project_id = ?1)",
                [project_id],
                |row| row.get(0),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not verify assistant project ownership: {error}")
            })?;
        if !project_exists {
            return Err("video.not_found: The assistant video project was not found".into());
        }
        if let Some(output_id) = output_id {
            let output_kind = connection
                .query_row(
                    "SELECT kind FROM video_output_records
                     WHERE id = ?2 AND project_id = ?1 AND status = 'ready'",
                    params![project_id, output_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| {
                    format!(
                        "video.store_failed: Could not verify assistant output ownership: {error}"
                    )
                })?;
            let Some(output_kind) = output_kind else {
                return Err("video.ownership_mismatch: The assistant output does not belong to this project".into());
            };
            if output_kind != relationship {
                return Err(
                    "video.ownership_mismatch: The assistant output role does not match its registered kind"
                        .into(),
                );
            }
        }
        connection
            .execute(
                "INSERT INTO assistant_video_artifacts
                 (id, thread_id, turn_id, item_id, project_id, output_id, relationship, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT DO UPDATE SET
                    relationship = excluded.relationship",
                params![
                    id,
                    thread_id,
                    turn_id,
                    item_id,
                    project_id,
                    output_id,
                    relationship,
                    timestamp,
                ],
            )
            .map_err(|error| {
                format!("video.store_failed: Could not link the assistant artifact: {error}")
            })?;
        let (saved_id, saved_created_at): (String, String) = connection
            .query_row(
                "SELECT id, created_at FROM assistant_video_artifacts
                 WHERE thread_id = ?1
                   AND COALESCE(item_id, '') = COALESCE(?2, '')
                   AND project_id = ?3
                   AND COALESCE(output_id, '') = COALESCE(?4, '')",
                params![thread_id, item_id, project_id, output_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| {
                format!("video.store_failed: Could not reload the assistant artifact: {error}")
            })?;
        Ok(json!({
            "id": saved_id,
            "thread_id": thread_id,
            "turn_id": turn_id,
            "item_id": item_id,
            "project_id": project_id,
            "output_id": output_id,
            "relationship": relationship,
            "created_at": saved_created_at,
        }))
    }

    /// Resolves the newest persisted Video Studio relationship for one Codex thread. A linked
    /// output is returned only while it is a ready artifact on the project's current version and
    /// its managed file still exists with the registered size; otherwise callers safely reopen
    /// the current project/draft instead of surfacing a stale or opaque path.
    pub fn latest_assistant_video_artifact(
        &self,
        thread_id: &str,
    ) -> Result<Option<Value>, String> {
        if thread_id.trim().is_empty() || thread_id.len() > 512 {
            return Err("video.invalid_link: thread_id is required and bounded".into());
        }
        let candidates = {
            let connection = self.lock()?;
            let mut statement = connection
                .prepare(
                    "SELECT a.id, a.turn_id, a.item_id, a.project_id, a.output_id,
                            a.relationship, a.created_at, vp.current_version_id,
                            o.version_id, o.status, o.artifact_path, o.size_bytes, o.sha256
                     FROM assistant_video_artifacts a
                     JOIN video_projects vp ON vp.project_id = a.project_id
                     LEFT JOIN video_output_records o
                       ON o.id = a.output_id AND o.project_id = a.project_id
                     WHERE a.thread_id = ?1
                     ORDER BY a.created_at DESC, a.rowid DESC
                     LIMIT 128",
                )
                .map_err(|error| {
                    format!(
                        "video.store_failed: Could not prepare assistant video recovery: {error}"
                    )
                })?;
            let candidates = statement
                .query_map([thread_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<i64>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                    ))
                })
                .map_err(|error| {
                    format!(
                        "video.store_failed: Could not inspect assistant video recovery: {error}"
                    )
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    format!("video.store_failed: Could not read assistant video recovery: {error}")
                })?;
            candidates
        };
        let Some((
            id,
            turn_id,
            item_id,
            project_id,
            linked_output_id,
            relationship,
            created_at,
            current_version_id,
            output_version_id,
            output_status,
            artifact_path,
            registered_size,
            registered_sha256,
        )) = candidates.into_iter().next()
        else {
            return Ok(None);
        };

        let current_output = linked_output_id
            .as_ref()
            .zip(output_version_id.as_ref())
            .filter(|_| output_status.as_deref() == Some("ready"))
            .filter(|(_, version_id)| *version_id == &current_version_id)
            .and_then(|(output_id, _)| {
                let path = self
                    .validate_artifact_path(artifact_path.as_deref()?)
                    .ok()?;
                let metadata = fs::symlink_metadata(&path).ok()?;
                let ordinary_exact_file = metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.nlink() == 1
                    && i64::try_from(metadata.len()).ok() == registered_size;
                (ordinary_exact_file
                    && registered_sha256.as_deref().is_some_and(|expected| {
                        sha256_file(&path).is_ok_and(|observed| observed == expected)
                    }))
                .then(|| output_id.clone())
            });
        let has_current_output = current_output.is_some();
        Ok(Some(json!({
            "id": id,
            "thread_id": thread_id,
            "turn_id": turn_id,
            "item_id": item_id,
            "project_id": project_id,
            "output_id": current_output,
            "relationship": if has_current_output { relationship } else { "project".to_string() },
            "created_at": created_at,
        })))
    }

    pub fn attach_project_master(
        &self,
        project_id: &str,
        history: &Value,
        export: &Value,
    ) -> Result<Value, String> {
        let connection = self.lock()?;
        let (name, raw_document): (String, String) = connection
            .query_row(
                "SELECT name, document_json FROM projects WHERE id = ?1",
                [project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the project master: {error}"))?
            .ok_or("Project was not found")?;
        drop(connection);
        let mut document =
            serde_json::from_str::<Value>(&raw_document).unwrap_or_else(|_| json!({}));
        let master = document
            .as_object_mut()
            .ok_or("Project document is invalid")?
            .entry("master")
            .or_insert_with(|| json!({}));
        let master = master
            .as_object_mut()
            .ok_or("Project master metadata is invalid")?;
        master.insert(
            "history_id".into(),
            history
                .get("id")
                .cloned()
                .ok_or("Master history has no identifier")?,
        );
        master.insert(
            "audio_path".into(),
            history
                .get("audio_path")
                .cloned()
                .ok_or("Master history has no audio path")?,
        );
        for key in ["title", "duration_seconds", "sample_rate", "created_at"] {
            if let Some(value) = history.get(key) {
                master.insert(key.into(), value.clone());
            }
        }
        if let Some(path) = history.get("audio_path").and_then(Value::as_str) {
            master.insert(
                "format".into(),
                json!(Path::new(path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("wav")),
            );
        }
        if let Some(value) = export.get("manifest_path") {
            master.insert("manifest_path".into(), value.clone());
        }
        self.save_project(&json!({ "id": project_id, "name": name, "document": document }))
    }

    fn reconcile_project_masters(&self) -> Result<(), String> {
        let candidates = {
            let connection = self.lock()?;
            let mut statement = connection
                .prepare("SELECT id, name, document_json FROM projects ORDER BY updated_at DESC")
                .map_err(|error| format!("Could not prepare project master recovery: {error}"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|error| format!("Could not inspect project masters: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read project masters: {error}"))?
        };

        for (project_id, project_name, raw_document) in candidates {
            let document =
                serde_json::from_str::<Value>(&raw_document).unwrap_or_else(|_| json!({}));
            let Some(master) = document.get("master") else {
                continue;
            };
            if master.get("history_id").and_then(Value::as_str).is_some() {
                continue;
            }
            let Some(raw_path) = master.get("audio_path").and_then(Value::as_str) else {
                continue;
            };
            let path = match self.validate_artifact_path(raw_path) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let path_string = path.to_string_lossy().to_string();
            let existing = self.list_history(None)?.into_iter().find(|item| {
                item.get("audio_path").and_then(Value::as_str) == Some(path_string.as_str())
            });
            let history = if let Some(history) = existing {
                history
            } else {
                let request = json!({
                    "operation": "adopt_project_master",
                    "project_id": project_id,
                    "generation_kind": "speech",
                    "title": master.get("title").and_then(Value::as_str).unwrap_or(&project_name),
                    "voice_name": "Project sequence",
                    "text": document.get("script").and_then(Value::as_str).unwrap_or("Project master"),
                });
                let job_id = self.create_job("project-master", &request)?;
                self.complete_synthesis(
                    &job_id,
                    &request,
                    &json!({
                        "id": Uuid::new_v4().simple().to_string(),
                        "audio_path": path_string,
                        "model_id": "soundar/project-master",
                        "engine": "finishing",
                        "generation_kind": "speech",
                        "sample_rate": master.get("sample_rate").and_then(Value::as_i64).unwrap_or(48_000),
                        "duration_seconds": master.get("duration_seconds").and_then(Value::as_f64).unwrap_or(0.0),
                        "inference_seconds": 0.0,
                        "rtf": 0.0,
                        "vram_peak_mb": 0,
                    }),
                )?
            };
            let format = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("wav");
            let settings = json!({
                "format": format,
                "sample_rate": history.get("sample_rate").and_then(Value::as_i64).unwrap_or(48_000),
                "gap_ms": master.get("gap_ms").and_then(Value::as_i64).unwrap_or(250),
                "fade_ms": master.get("fade_ms").and_then(Value::as_i64).unwrap_or(12),
                "target_lufs": master.get("target_lufs").and_then(Value::as_f64).unwrap_or(-16.0),
                "recovered": true,
            });
            let manifest_path = path.with_extension(format!("{format}.provenance.json"));
            if !manifest_path.exists() {
                write_store_json_atomically(
                    &manifest_path,
                    &json!({
                        "schema_version": 1,
                        "application": "soundAr",
                        "project": { "id": project_id, "name": project_name },
                        "output": { "path": path, "sha256": sha256_file(&path)?, "recovered_at": now() },
                        "recovered": true,
                    }),
                )?;
            }
            let export = self.record_project_export(
                &project_id,
                history
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Master history has no identifier")?,
                &settings,
                &manifest_path,
            )?;
            self.attach_project_master(&project_id, &history, &export)?;
        }
        Ok(())
    }

    pub fn project_master_plan(&self, id: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let (name, document): (String, String) = connection
            .query_row(
                "SELECT name, document_json FROM projects WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect the project: {error}"))?
            .ok_or("Project was not found")?;
        let mut statement = connection
            .prepare(
                "SELECT c.id, c.title, c.text, c.history_id, h.audio_path, h.model_id, h.engine, h.voice, h.sample_rate, h.duration_seconds, a.sha256
                 FROM project_clips c
                 LEFT JOIN history h ON h.id = c.history_id
                 LEFT JOIN artifacts a ON a.id = h.artifact_id
                 WHERE c.project_id = ?1 AND trim(c.text) != '' ORDER BY c.position ASC",
            )
            .map_err(|error| format!("Could not prepare the project master: {error}"))?;
        let rows = statement
            .query_map([id], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "title": row.get::<_, String>(1)?,
                    "text": row.get::<_, String>(2)?,
                    "history_id": row.get::<_, Option<String>>(3)?,
                    "audio_path": row.get::<_, Option<String>>(4)?,
                    "model_id": row.get::<_, Option<String>>(5)?,
                    "engine": row.get::<_, Option<String>>(6)?,
                    "voice": row.get::<_, Option<String>>(7)?,
                    "sample_rate": row.get::<_, Option<i64>>(8)?,
                    "duration_seconds": row.get::<_, Option<f64>>(9)?,
                    "sha256": row.get::<_, Option<String>>(10)?,
                }))
            })
            .map_err(|error| format!("Could not read project clips: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read project clips: {error}"))?;
        if rows.is_empty() {
            return Err("Project has no written chapters to export".to_string());
        }
        let mut audio_paths = Vec::with_capacity(rows.len());
        for row in &rows {
            let title = row
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Chapter");
            let path = row
                .get("audio_path")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("Render {title} before exporting the project"))?;
            audio_paths.push(
                self.validate_artifact_path(path)?
                    .to_string_lossy()
                    .to_string(),
            );
        }
        Ok(json!({
            "project_id": id,
            "name": name,
            "document": serde_json::from_str::<Value>(&document).unwrap_or_else(|_| json!({})),
            "audio_paths": audio_paths,
            "clips": rows,
        }))
    }

    pub fn record_project_export(
        &self,
        project_id: &str,
        history_id: &str,
        settings: &Value,
        manifest_path: &Path,
    ) -> Result<Value, String> {
        let id = Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO project_exports (id, project_id, history_id, settings_json, manifest_path, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, project_id, history_id, settings.to_string(), manifest_path.to_string_lossy(), timestamp],
            )
            .map_err(|error| format!("Could not record the project export: {error}"))?;
        Ok(json!({
            "id": id,
            "project_id": project_id,
            "history_id": history_id,
            "settings": settings,
            "manifest_path": manifest_path.to_string_lossy(),
            "created_at": timestamp,
        }))
    }

    pub fn delete_project(&self, id: &str) -> Result<bool, String> {
        let connection = self.lock()?;
        connection
            .execute("DELETE FROM projects WHERE id = ?1", [id])
            .map(|count| count > 0)
            .map_err(|error| format!("Could not delete the project: {error}"))
    }

    pub fn create_voice(&self, request: &Value) -> Result<Value, String> {
        let name = required_trimmed(request, "name", "Voice name is required")?;
        let style = request
            .get("style")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Custom voice");
        let source_path = PathBuf::from(required_trimmed(
            request,
            "source_path",
            "A reference audio file is required",
        )?);
        if !source_path.is_file() {
            return Err("The selected reference audio file no longer exists".to_string());
        }
        let extension = source_path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or("The reference audio file has no extension")?;
        if !matches!(extension.as_str(), "wav" | "flac" | "mp3" | "m4a" | "ogg") {
            return Err("Reference audio must be WAV, FLAC, MP3, M4A, or OGG".to_string());
        }
        if request.get("consent_confirmed").and_then(Value::as_bool) != Some(true) {
            return Err("Consent acknowledgement is required before importing a voice".to_string());
        }
        let consent_basis = required_trimmed(
            request,
            "consent_basis",
            "Describe the consent basis for this voice",
        )?;
        let speaker_relationship = required_trimmed(
            request,
            "speaker_relationship",
            "Select the speaker relationship",
        )?;
        let permitted_uses =
            required_trimmed(request, "permitted_uses", "Describe the permitted uses")?;

        let id = Uuid::new_v4().simple().to_string();
        let profile_root = self.voices_root.join(&id);
        fs::create_dir_all(&profile_root)
            .map_err(|error| format!("Could not create the voice profile directory: {error}"))?;
        let destination = profile_root.join(format!("original.{extension}"));
        let temporary = destination.with_extension(format!("{extension}.partial"));
        fs::copy(&source_path, &temporary)
            .map_err(|error| format!("Could not import reference audio: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("Could not finalize reference audio: {error}"))?;
        let label = source_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Imported reference");
        let timestamp = now();
        let source_date = request
            .get("source_date")
            .and_then(Value::as_str)
            .unwrap_or("");
        let engines = json!(["Chatterbox", "XTTS"]);
        let reference_id = Uuid::new_v4().simple().to_string();
        let connection = self.lock()?;
        connection.execute(
                "INSERT INTO voices (id, name, style, sample_label, sample_seconds, engines_json, consent, state, color, local_path, source_kind, consent_basis, speaker_relationship, permitted_uses, source_date, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, 'confirmed', 'draft', 'coral', ?6, 'imported', ?7, ?8, ?9, ?10, ?11, ?11)",
                params![id, name, style, label, engines.to_string(), destination.to_string_lossy(), consent_basis, speaker_relationship, permitted_uses, source_date, timestamp],
            )
            .map_err(|error| format!("Could not save the voice profile: {error}"))?;
        connection.execute(
            "INSERT INTO voice_references (id, voice_id, original_path, processed_path, original_sha256, analysis_json, processing_json, active, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4, '{}', '{}', 0, ?5)",
            params![reference_id, id, destination.to_string_lossy(), sha256_file(&destination)?, timestamp],
        ).map_err(|error| format!("Could not save voice reference provenance: {error}"))?;
        connection.execute(
            "INSERT INTO consent_records (id, voice_id, basis, speaker_relationship, permitted_uses, source_date, acknowledged_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            params![Uuid::new_v4().simple().to_string(), id, consent_basis, speaker_relationship, permitted_uses, source_date, timestamp],
        ).map_err(|error| format!("Could not save consent evidence: {error}"))?;
        Ok(json!({
            "id": id,
            "name": name,
            "style": style,
            "sample_label": label,
            "sample_seconds": 0,
            "engines": engines,
            "consent": "confirmed",
            "state": "draft",
            "color": "coral",
            "local_path": destination,
            "source_kind": "imported",
            "consent_basis": consent_basis,
            "speaker_relationship": speaker_relationship,
            "permitted_uses": permitted_uses,
            "source_date": source_date,
            "active_reference_id": reference_id,
        }))
    }

    pub fn add_voice_reference(&self, id: &str, raw_path: &str) -> Result<Value, String> {
        let source = PathBuf::from(raw_path);
        if !source.is_file() {
            return Err("The selected reference audio file no longer exists".to_string());
        }
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or("The reference audio file has no extension")?;
        if !matches!(extension.as_str(), "wav" | "flac" | "mp3" | "m4a" | "ogg") {
            return Err("Reference audio must be WAV, FLAC, MP3, M4A, or OGG".to_string());
        }
        let connection = self.lock()?;
        let approved: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM voices WHERE id = ?1 AND consent = 'confirmed' AND state != 'preset')",
                [id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not verify the voice profile: {error}"))?;
        if !approved {
            return Err(
                "Additional references require an existing consent-backed voice profile"
                    .to_string(),
            );
        }
        let profile_root = self.voices_root.join(id);
        let reference_id = Uuid::new_v4().simple().to_string();
        let destination = profile_root.join(format!("original-{reference_id}.{extension}"));
        let temporary = destination.with_extension(format!("{extension}.partial"));
        fs::copy(&source, &temporary)
            .map_err(|error| format!("Could not import reference audio: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("Could not finalize reference audio: {error}"))?;
        connection
            .execute(
                "INSERT INTO voice_references (id, voice_id, original_path, processed_path, original_sha256, analysis_json, processing_json, active, created_at)
                 VALUES (?1, ?2, ?3, NULL, ?4, '{}', '{}', 0, ?5)",
                params![reference_id, id, destination.to_string_lossy(), sha256_file(&destination)?, now()],
            )
            .map_err(|error| format!("Could not save voice reference provenance: {error}"))?;
        Ok(json!({
            "id": reference_id,
            "voice_id": id,
            "original_path": destination,
            "original_sha256": sha256_file(&destination)?,
            "created_at": now(),
        }))
    }

    pub fn finalize_voice_reference(
        &self,
        id: &str,
        reference_id: &str,
        prepared: &Value,
    ) -> Result<Value, String> {
        let analysis = prepared
            .get("analysis")
            .ok_or("Voice preparation returned no analysis")?;
        let processed_path = required_trimmed(
            prepared,
            "audio_path",
            "Voice preparation returned no audio",
        )?;
        let processed = PathBuf::from(processed_path)
            .canonicalize()
            .map_err(|error| format!("Could not resolve processed voice audio: {error}"))?;
        let profile_root = self
            .voices_root
            .join(id)
            .canonicalize()
            .map_err(|error| format!("Could not resolve voice profile storage: {error}"))?;
        if !processed.starts_with(profile_root) {
            return Err("Processed voice audio is outside its managed profile".to_string());
        }
        validate_audio_file(&processed)?;
        let duration = analysis
            .get("duration_seconds")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let quality_passes = duration >= 3.0
            && duration <= 120.0
            && analysis
                .get("sample_rate")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                >= 16_000
            && analysis
                .get("silence_ratio")
                .and_then(numeric_value)
                .unwrap_or(1.0)
                <= 0.45
            && analysis
                .get("clipping_ratio")
                .and_then(numeric_value)
                .unwrap_or(1.0)
                <= 0.001;
        let state = if quality_passes { "ready" } else { "draft" };
        let color = if state == "ready" { "green" } else { "coral" };
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize voice profile: {error}"))?;
        if quality_passes {
            transaction
                .execute(
                    "UPDATE voice_references SET active = 0 WHERE voice_id = ?1",
                    [id],
                )
                .map_err(|error| format!("Could not update the active voice reference: {error}"))?;
            transaction
                .execute(
                "UPDATE voices SET local_path = ?2, sample_seconds = ?3, sample_rate = ?4, channels = ?5, peak_dbfs = ?6, silence_ratio = ?7, clipping_ratio = ?8, analysis_json = ?9, state = ?10, color = ?11, updated_at = ?12 WHERE id = ?1",
                params![
                    id,
                    processed.to_string_lossy(),
                    duration,
                    analysis.get("sample_rate").and_then(Value::as_i64).unwrap_or(0),
                    analysis.get("channels").and_then(Value::as_i64).unwrap_or(0),
                    analysis.get("peak_dbfs").and_then(Value::as_f64).unwrap_or(-120.0),
                    analysis.get("silence_ratio").and_then(Value::as_f64).unwrap_or(0.0),
                    analysis.get("clipping_ratio").and_then(Value::as_f64).unwrap_or(0.0),
                    analysis.to_string(),
                    state,
                    color,
                    now(),
                ],
                )
                .map_err(|error| format!("Could not save voice analysis: {error}"))?;
        } else {
            transaction
                .execute(
                    "UPDATE voices SET updated_at = ?2 WHERE id = ?1",
                    params![id, now()],
                )
                .map_err(|error| format!("Could not retain voice readiness: {error}"))?;
        }
        transaction.execute(
            "UPDATE voice_references SET processed_path = ?3, processed_sha256 = ?4, analysis_json = ?5, processing_json = ?6, active = ?7 WHERE voice_id = ?1 AND id = ?2",
            params![id, reference_id, processed.to_string_lossy(), sha256_file(&processed)?, analysis.to_string(), prepared.get("processing").cloned().unwrap_or_else(|| json!({})).to_string(), quality_passes],
        ).map_err(|error| format!("Could not save processed voice provenance: {error}"))?;
        transaction.execute(
            "INSERT INTO voice_reference_revisions (id, reference_id, processed_path, processed_sha256, analysis_json, processing_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![Uuid::new_v4().simple().to_string(), reference_id, processed.to_string_lossy(), sha256_file(&processed)?, analysis.to_string(), prepared.get("processing").cloned().unwrap_or_else(|| json!({})).to_string(), now()],
        ).map_err(|error| format!("Could not save voice reference revision: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit voice readiness: {error}"))?;
        drop(connection);
        self.get_voice(id)?
            .ok_or_else(|| "Voice profile disappeared during analysis".to_string())
    }

    pub fn voice_reference_for_processing(
        &self,
        voice_id: &str,
        reference_id: &str,
    ) -> Result<Value, String> {
        let connection = self.lock()?;
        connection.query_row(
            "SELECT r.original_path, r.processed_path, r.analysis_json, r.processing_json
             FROM voice_references r JOIN voices v ON v.id = r.voice_id
             WHERE r.voice_id = ?1 AND r.id = ?2 AND v.consent = 'confirmed' AND v.state != 'preset'",
            params![voice_id, reference_id],
            |row| {
                let analysis: String = row.get(2)?;
                let processing: String = row.get(3)?;
                Ok(json!({
                    "original_path": row.get::<_, String>(0)?,
                    "processed_path": row.get::<_, Option<String>>(1)?,
                    "analysis": serde_json::from_str::<Value>(&analysis).unwrap_or_else(|_| json!({})),
                    "processing": serde_json::from_str::<Value>(&processing).unwrap_or_else(|_| json!({})),
                }))
            },
        ).optional().map_err(|error| format!("Could not read the voice reference: {error}"))?
            .ok_or_else(|| "The consent-backed voice reference was not found".to_string())
    }

    pub fn update_voice_reference_transcript(
        &self,
        voice_id: &str,
        reference_id: &str,
        transcript: &str,
        source: &str,
    ) -> Result<Value, String> {
        let transcript = transcript.trim();
        if transcript.len() > 20_000 {
            return Err("Reference transcripts cannot exceed 20,000 characters".to_string());
        }
        if !matches!(source, "automatic" | "corrected" | "none") {
            return Err("Transcript source must be automatic, corrected, or none".to_string());
        }
        let connection = self.lock()?;
        let changed = connection.execute(
            "UPDATE voice_references SET transcript_text = ?3, transcript_source = ?4 WHERE voice_id = ?1 AND id = ?2",
            params![voice_id, reference_id, transcript, source],
        ).map_err(|error| format!("Could not save the reference transcript: {error}"))?;
        if changed == 0 {
            return Err("The voice reference was not found".to_string());
        }
        drop(connection);
        self.get_voice(voice_id)?
            .ok_or_else(|| "The voice profile was not found".to_string())
    }

    pub fn save_voice_evaluation(&self, evaluation: &Value) -> Result<Value, String> {
        let voice_id = required_trimmed(evaluation, "voice_id", "Voice profile is required")?;
        let reference_id =
            required_trimmed(evaluation, "reference_id", "Voice reference is required")?;
        let model_id = required_trimmed(evaluation, "model_id", "Evaluation model is required")?;
        let history_id =
            required_trimmed(evaluation, "history_id", "Evaluation audio is required")?;
        let script = required_trimmed(evaluation, "script", "Evaluation script is required")?;
        let decision = evaluation
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if !matches!(decision, "pending" | "accepted" | "rejected") {
            return Err("Evaluation decision must be pending, accepted, or rejected".to_string());
        }
        let notes = evaluation
            .get("notes")
            .and_then(Value::as_str)
            .unwrap_or("");
        let id = evaluation
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let timestamp = now();
        let connection = self.lock()?;
        let evidence: Option<(String, String, String)> = connection
            .query_row(
                "SELECT vr.processed_path, history.model_id, jobs.request_json
             FROM voice_references AS vr
             JOIN history ON history.id = ?3
             JOIN jobs ON jobs.id = history.job_id
             WHERE vr.id = ?2 AND vr.voice_id = ?1 AND vr.processed_path IS NOT NULL",
                params![voice_id, reference_id, history_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|error| format!("Could not verify voice evaluation evidence: {error}"))?;
        let Some((reference_path, history_model_id, request_json)) = evidence else {
            return Err("Evaluation audio must belong to the selected voice reference".to_string());
        };
        let request: Value = serde_json::from_str(&request_json)
            .map_err(|error| format!("Stored evaluation settings are invalid: {error}"))?;
        let requested_reference = request.get("reference_audio_path").and_then(Value::as_str);
        if history_model_id != model_id || requested_reference != Some(reference_path.as_str()) {
            return Err(
                "Evaluation audio must use the selected model and exact voice reference revision"
                    .to_string(),
            );
        }
        connection.execute(
            "INSERT INTO voice_evaluations (id, voice_id, reference_id, model_id, history_id, script, decision, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(id) DO UPDATE SET decision = excluded.decision, notes = excluded.notes, updated_at = excluded.updated_at",
            params![id, voice_id, reference_id, model_id, history_id, script, decision, notes, timestamp],
        ).map_err(|error| format!("Could not save the voice evaluation: {error}"))?;
        let saved: Option<Value> = connection.query_row(
            "SELECT id, voice_id, reference_id, model_id, history_id, script, decision, notes, created_at, updated_at, speaker_similarity, similarity_model_id, similarity_engine, similarity_scoring_version, similarity_inference_seconds, similarity_vram_mb, reference_sha256, candidate_sha256, similarity_measured_at FROM voice_evaluations WHERE id = ?1",
            [&id], voice_evaluation_from_row,
        ).optional().map_err(|error| format!("Could not read the voice evaluation: {error}"))?;
        saved.ok_or_else(|| "The voice evaluation could not be saved".to_string())
    }

    pub fn voice_similarity_request(&self, evaluation_id: &str) -> Result<Value, String> {
        let connection = self.lock()?;
        let evidence: Option<(String, String, String, String)> = connection
            .query_row(
                "SELECT vr.processed_path, vr.processed_sha256, h.audio_path, a.sha256
                 FROM voice_evaluations ve
                 JOIN voice_references vr ON vr.id = ve.reference_id AND vr.voice_id = ve.voice_id
                 JOIN history h ON h.id = ve.history_id
                 JOIN artifacts a ON a.id = h.artifact_id
                 WHERE ve.id = ?1 AND vr.processed_path IS NOT NULL AND vr.processed_sha256 IS NOT NULL",
                [evaluation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| format!("Could not inspect speaker-similarity evidence: {error}"))?;
        drop(connection);
        let (reference_path, reference_sha256, candidate_path, candidate_sha256) =
            evidence.ok_or("The voice evaluation has no complete audio evidence")?;
        let reference_bytes = self.voice_audio_bytes(&reference_path)?;
        if sha256_bytes(&reference_bytes) != reference_sha256 {
            return Err("The voice reference changed on disk and cannot be measured".to_string());
        }
        let candidate_bytes = self.generated_audio_bytes(&candidate_path)?;
        if sha256_bytes(&candidate_bytes) != candidate_sha256 {
            return Err(
                "The generated evaluation changed on disk and cannot be measured".to_string(),
            );
        }
        Ok(json!({
            "reference_audio_path": reference_path,
            "candidate_audio_path": candidate_path,
            "reference_sha256": reference_sha256,
            "candidate_sha256": candidate_sha256,
        }))
    }

    pub fn complete_voice_similarity(
        &self,
        job_id: &str,
        evaluation_id: &str,
        expected: &Value,
        result: &Value,
    ) -> Result<Value, String> {
        let current = self.voice_similarity_request(evaluation_id)?;
        for key in ["reference_sha256", "candidate_sha256"] {
            if current.get(key) != expected.get(key) {
                return Err("Voice evaluation evidence changed during measurement".to_string());
            }
        }
        let similarity = result
            .get("similarity")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && (-1.0..=1.0).contains(value))
            .ok_or("Speaker verifier returned an invalid cosine similarity")?;
        let verifier_model_id =
            required_trimmed(result, "model_id", "Similarity verifier is required")?;
        let verifier_engine = required_trimmed(result, "engine", "Similarity engine is required")?;
        let scoring_version = required_trimmed(
            result,
            "scoring_version",
            "Similarity scoring version is required",
        )?;
        let inference_seconds = result
            .get("inference_seconds")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or("Speaker verifier returned invalid inference timing")?;
        let vram_mb = result
            .get("vram_peak_mb")
            .and_then(numeric_value)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or("Speaker verifier returned invalid VRAM evidence")?;
        let measured_at = now();
        let mut connection = self.lock()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not finalize speaker similarity: {error}"))?;
        let changed = transaction.execute(
            "UPDATE voice_evaluations SET speaker_similarity = ?2, similarity_model_id = ?3, similarity_engine = ?4, similarity_scoring_version = ?5, similarity_inference_seconds = ?6, similarity_vram_mb = ?7, reference_sha256 = ?8, candidate_sha256 = ?9, similarity_measured_at = ?10, updated_at = ?10 WHERE id = ?1",
            params![evaluation_id, similarity, verifier_model_id, verifier_engine, scoring_version, inference_seconds, vram_mb, current["reference_sha256"].as_str(), current["candidate_sha256"].as_str(), measured_at],
        ).map_err(|error| format!("Could not save speaker similarity: {error}"))?;
        if changed == 0 {
            return Err("The voice evaluation was not found".to_string());
        }
        transaction.execute(
            "UPDATE jobs SET status = 'completed', progress = 1, updated_at = ?2 WHERE id = ?1 AND status != 'cancelled'",
            params![job_id, measured_at],
        ).map_err(|error| format!("Could not complete speaker-similarity job: {error}"))?;
        insert_job_event(&transaction, job_id, "completed", 1.0, None, &measured_at)?;
        let value = transaction.query_row(
            "SELECT id, voice_id, reference_id, model_id, history_id, script, decision, notes, created_at, updated_at, speaker_similarity, similarity_model_id, similarity_engine, similarity_scoring_version, similarity_inference_seconds, similarity_vram_mb, reference_sha256, candidate_sha256, similarity_measured_at FROM voice_evaluations WHERE id = ?1",
            [evaluation_id], voice_evaluation_from_row,
        ).map_err(|error| format!("Could not read speaker similarity: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit speaker similarity: {error}"))?;
        Ok(value)
    }

    pub fn mark_voice_processing_error(
        &self,
        id: &str,
        reference_id: &str,
        error: &str,
    ) -> Result<Value, String> {
        let analysis = json!({
            "warnings": [format!("Reference processing failed: {error}")],
            "processing_error": error,
        });
        let connection = self.lock()?;
        let has_ready_reference: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM voice_references WHERE voice_id = ?1 AND active = 1 AND processed_path IS NOT NULL)",
                [id],
                |row| row.get(0),
            )
            .map_err(|cause| format!("Could not inspect existing voice references: {cause}"))?;
        if !has_ready_reference {
            connection
                .execute(
                    "UPDATE voices SET state = 'draft', color = 'coral', analysis_json = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, analysis.to_string(), now()],
                )
                .map_err(|cause| format!("Could not record voice processing failure: {cause}"))?;
        }
        connection
            .execute(
                "UPDATE voice_references SET analysis_json = ?3 WHERE voice_id = ?1 AND id = ?2",
                params![id, reference_id, analysis.to_string()],
            )
            .map_err(|cause| format!("Could not record reference processing failure: {cause}"))?;
        drop(connection);
        self.get_voice(id)?
            .ok_or_else(|| "Voice profile disappeared during processing".to_string())
    }

    pub fn list_voices(&self) -> Result<Vec<Value>, String> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, style, sample_label, sample_seconds, engines_json, consent, state, color, local_path, source_kind, consent_basis, speaker_relationship, permitted_uses, source_date, analysis_json FROM voices ORDER BY CASE WHEN state = 'preset' THEN 0 ELSE 1 END, name",
            )
            .map_err(|error| format!("Could not prepare voices: {error}"))?;
        let rows = statement
            .query_map([], voice_from_row)
            .map_err(|error| format!("Could not list voices: {error}"))?;
        let mut voices = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not read voices: {error}"))?;
        drop(statement);
        for voice in &mut voices {
            if let Some(id) = voice.get("id").and_then(Value::as_str).map(str::to_string) {
                voice["references"] = json!(voice_references(&connection, &id)?);
                voice["evaluations"] = json!(voice_evaluations(&connection, &id)?);
            }
        }
        Ok(voices)
    }

    pub fn delete_voice(&self, id: &str) -> Result<bool, String> {
        let mut connection = self.lock()?;
        let record: Option<(String, String)> = connection
            .query_row(
                "SELECT state, COALESCE(local_path, '') FROM voices WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not find the voice profile: {error}"))?;
        let Some((state, local_path)) = record else {
            return Ok(false);
        };
        if state == "preset" {
            return Err("Built-in voice presets cannot be deleted".to_string());
        }
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not delete the voice profile: {error}"))?;
        transaction
            .execute("DELETE FROM voices WHERE id = ?1", [id])
            .map_err(|error| format!("Could not delete the voice profile: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit voice deletion: {error}"))?;
        if !local_path.is_empty() {
            let path = PathBuf::from(local_path);
            if path.starts_with(&self.voices_root) {
                if let Some(parent) = path.parent() {
                    fs::remove_dir_all(parent).ok();
                }
            }
        }
        Ok(true)
    }

    pub fn validate_voice_reference(&self, raw_path: &str) -> Result<(), String> {
        let root = self
            .voices_root
            .canonicalize()
            .map_err(|error| format!("Could not resolve managed voice storage: {error}"))?;
        let path = PathBuf::from(raw_path)
            .canonicalize()
            .map_err(|error| format!("Reference voice audio was not found: {error}"))?;
        if !path.starts_with(root) {
            return Err(
                "Voice cloning requires a consent-backed voice from the Voice library".to_string(),
            );
        }
        let normalized_path = path.to_string_lossy().to_string();
        let connection = self.lock()?;
        let approved: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM voices WHERE local_path = ?1 AND consent = 'confirmed' AND state = 'ready')",
                [normalized_path],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not verify voice consent: {error}"))?;
        if !approved {
            return Err(
                "The selected voice has not passed consent and audio-readiness checks".to_string(),
            );
        }
        Ok(())
    }

    fn validate_batch_voice_references(
        &self,
        request: &Value,
        normalized_rows: &[Value],
    ) -> Result<(), String> {
        if let Some(reference) = request
            .pointer("/settings/reference_audio_path")
            .and_then(Value::as_str)
        {
            self.validate_voice_reference(reference)?;
        }
        for row in normalized_rows {
            if let Some(reference) = row
                .pointer("/settings/reference_audio_path")
                .and_then(Value::as_str)
            {
                self.validate_voice_reference(reference)?;
            }
        }
        Ok(())
    }

    pub fn voice_reference_for_id(&self, id: &str) -> Result<Option<(String, String)>, String> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT name, local_path FROM voices WHERE id = ?1 AND consent = 'confirmed' AND state = 'ready' AND local_path IS NOT NULL",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| format!("Could not resolve the API voice profile: {error}"))
    }

    fn get_voice(&self, id: &str) -> Result<Option<Value>, String> {
        let connection = self.lock()?;
        let mut voice = connection
            .query_row(
                "SELECT id, name, style, sample_label, sample_seconds, engines_json, consent, state, color, local_path, source_kind, consent_basis, speaker_relationship, permitted_uses, source_date, analysis_json FROM voices WHERE id = ?1",
                [id],
                voice_from_row,
            )
            .optional()
            .map_err(|error| format!("Could not read the voice profile: {error}"))?;
        if let Some(value) = voice.as_mut() {
            value["references"] = json!(voice_references(&connection, id)?);
            value["evaluations"] = json!(voice_evaluations(&connection, id)?);
        }
        Ok(voice)
    }

    pub fn voice_audio_bytes(&self, raw_path: &str) -> Result<Vec<u8>, String> {
        let root = self
            .voices_root
            .canonicalize()
            .map_err(|error| format!("Could not resolve voice storage: {error}"))?;
        let path = PathBuf::from(raw_path)
            .canonicalize()
            .map_err(|error| format!("Voice audio was not found: {error}"))?;
        if !path.starts_with(root) {
            return Err("Voice playback is restricted to soundAr voice storage".to_string());
        }
        fs::read(path).map_err(|error| format!("Could not read voice audio: {error}"))
    }

    fn validate_artifact_path(&self, raw_path: &str) -> Result<PathBuf, String> {
        let root = self
            .artifacts_root
            .canonicalize()
            .map_err(|error| format!("Could not resolve the artifact directory: {error}"))?;
        let path = PathBuf::from(raw_path)
            .canonicalize()
            .map_err(|error| format!("Generated audio was not found: {error}"))?;
        if !path.starts_with(root) {
            return Err("Generated audio is outside the managed artifact directory".to_string());
        }
        Ok(path)
    }

    fn validate_artifact_path_allow_missing(&self, raw_path: &str) -> Result<PathBuf, String> {
        let root = self
            .artifacts_root
            .canonicalize()
            .map_err(|error| format!("Could not resolve the artifact directory: {error}"))?;
        let path = PathBuf::from(raw_path);
        let parent = path
            .parent()
            .ok_or("The artifact path has no parent")?
            .canonicalize()
            .map_err(|error| format!("Could not resolve the artifact directory: {error}"))?;
        if !parent.starts_with(root) {
            return Err("Artifact deletion is restricted to soundAr storage".to_string());
        }
        Ok(path)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.connection
            .lock()
            .map_err(|_| "The soundAr database lock is poisoned".to_string())
    }
}

fn migrate(connection: &mut Connection, from_version: i64) -> Result<(), String> {
    if from_version < 0 || from_version > SCHEMA_VERSION {
        return Err(format!(
            "No migration path exists from schema {from_version}"
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start database migration: {error}"))?;
    if from_version < 1 {
        transaction
            .execute_batch(
            "CREATE TABLE jobs (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL CHECK(status IN ('queued','preparing','running','completed','failed','cancelled')),
                request_json TEXT NOT NULL,
                progress REAL NOT NULL DEFAULT 0,
                attempt INTEGER NOT NULL DEFAULT 1,
                error TEXT,
                output_artifact_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX jobs_status_created_idx ON jobs(status, created_at DESC);
            CREATE TABLE artifacts (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                path TEXT NOT NULL UNIQUE,
                format TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE history (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                artifact_id TEXT NOT NULL REFERENCES artifacts(id),
                title TEXT NOT NULL,
                voice TEXT NOT NULL,
                text TEXT NOT NULL,
                model_id TEXT NOT NULL,
                engine TEXT NOT NULL,
                generation_kind TEXT NOT NULL DEFAULT 'speech' CHECK(generation_kind IN ('speech', 'music')),
                audio_path TEXT NOT NULL,
                sample_rate INTEGER NOT NULL,
                duration_seconds REAL NOT NULL,
                inference_seconds REAL NOT NULL,
                rtf REAL NOT NULL,
                vram_peak_mb INTEGER NOT NULL,
                waveform_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                favorite INTEGER NOT NULL DEFAULT 0,
                notes TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX history_created_idx ON history(created_at DESC);
            CREATE INDEX history_model_idx ON history(model_id, created_at DESC);
            CREATE TABLE voices (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                style TEXT NOT NULL,
                sample_label TEXT NOT NULL,
                sample_seconds REAL NOT NULL DEFAULT 0,
                engines_json TEXT NOT NULL,
                consent TEXT NOT NULL,
                state TEXT NOT NULL,
                color TEXT NOT NULL,
                local_path TEXT,
                source_kind TEXT NOT NULL,
                consent_basis TEXT NOT NULL DEFAULT '',
                speaker_relationship TEXT NOT NULL DEFAULT '',
                permitted_uses TEXT NOT NULL DEFAULT '',
                source_date TEXT NOT NULL DEFAULT '',
                sample_rate INTEGER NOT NULL DEFAULT 0,
                channels INTEGER NOT NULL DEFAULT 0,
                peak_dbfs REAL NOT NULL DEFAULT -120,
                silence_ratio REAL NOT NULL DEFAULT 0,
                clipping_ratio REAL NOT NULL DEFAULT 0,
                analysis_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE presets (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                settings_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                document_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE benchmark_runs (
                id TEXT PRIMARY KEY,
                model_id TEXT NOT NULL,
                result_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
            )
            .map_err(|error| format!("Could not create the soundAr database: {error}"))?;
        seed_builtin_voices(&transaction)?;
    }
    if from_version < 2 {
        transaction
            .execute_batch(
                "CREATE TABLE batch_runs (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('queued','running','completed','failed','cancelled')),
                    total_items INTEGER NOT NULL,
                    completed_items INTEGER NOT NULL DEFAULT 0,
                    failed_items INTEGER NOT NULL DEFAULT 0,
                    request_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .map_err(|error| format!("Could not migrate batch storage: {error}"))?;
    }
    if from_version < 3 {
        transaction
            .execute_batch(
                "CREATE TABLE transcriptions (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                source_path TEXT NOT NULL,
                model_id TEXT NOT NULL,
                engine TEXT NOT NULL,
                text TEXT NOT NULL,
                segments_json TEXT NOT NULL,
                audio_duration_seconds REAL NOT NULL,
                inference_seconds REAL NOT NULL,
                rtf REAL NOT NULL,
                vram_peak_mb INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX transcriptions_created_idx ON transcriptions(created_at DESC);",
            )
            .map_err(|error| format!("Could not migrate transcription storage: {error}"))?;
    }
    if from_version < 4 {
        transaction
            .execute_batch(
                "ALTER TABLE batch_runs ADD COLUMN error TEXT;
                CREATE TABLE batch_items (
                    id TEXT PRIMARY KEY,
                    batch_id TEXT NOT NULL REFERENCES batch_runs(id) ON DELETE CASCADE,
                    item_index INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('queued','running','completed','failed','cancelled')),
                    history_id TEXT REFERENCES history(id) ON DELETE SET NULL,
                    error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(batch_id, item_index)
                );
                CREATE INDEX batch_items_batch_idx ON batch_items(batch_id, item_index);
                CREATE TABLE comparisons (
                    id TEXT PRIMARY KEY,
                    left_history_id TEXT NOT NULL REFERENCES history(id) ON DELETE CASCADE,
                    right_history_id TEXT NOT NULL REFERENCES history(id) ON DELETE CASCADE,
                    script TEXT NOT NULL,
                    winner TEXT CHECK(winner IN ('A','B','tie')),
                    notes TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX comparisons_updated_idx ON comparisons(updated_at DESC);",
            )
            .map_err(|error| format!("Could not migrate batch and comparison storage: {error}"))?;
    }
    if from_version < 5 {
        transaction
            .execute_batch(
                "CREATE TABLE voice_references (
                    id TEXT PRIMARY KEY,
                    voice_id TEXT NOT NULL REFERENCES voices(id) ON DELETE CASCADE,
                    original_path TEXT NOT NULL,
                    processed_path TEXT,
                    original_sha256 TEXT NOT NULL,
                    processed_sha256 TEXT,
                    analysis_json TEXT NOT NULL DEFAULT '{}',
                    processing_json TEXT NOT NULL DEFAULT '{}',
                    active INTEGER NOT NULL DEFAULT 1,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX voice_references_voice_idx ON voice_references(voice_id, active, created_at);
                CREATE TABLE consent_records (
                    id TEXT PRIMARY KEY,
                    voice_id TEXT NOT NULL REFERENCES voices(id) ON DELETE CASCADE,
                    basis TEXT NOT NULL,
                    speaker_relationship TEXT NOT NULL,
                    permitted_uses TEXT NOT NULL,
                    source_date TEXT NOT NULL,
                    acknowledged_at TEXT NOT NULL,
                    revoked_at TEXT
                );
                CREATE INDEX consent_records_voice_idx ON consent_records(voice_id, acknowledged_at);",
            )
            .map_err(|error| format!("Could not migrate voice provenance storage: {error}"))?;
    }
    if from_version < 6 {
        transaction
            .execute_batch(
                "CREATE TABLE project_clips (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    title TEXT NOT NULL,
                    text TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('empty','stale','rendering','rendered','failed')),
                    history_id TEXT REFERENCES history(id) ON DELETE SET NULL,
                    settings_json TEXT NOT NULL DEFAULT '{}',
                    content_hash TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE INDEX project_clips_project_idx ON project_clips(project_id, position);
                CREATE TABLE project_revisions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    document_json TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX project_revisions_project_idx ON project_revisions(project_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not migrate project production storage: {error}"))?;
    }
    if from_version < 7 {
        transaction
            .execute_batch(
                "CREATE TABLE project_exports (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    history_id TEXT REFERENCES history(id) ON DELETE SET NULL,
                    settings_json TEXT NOT NULL,
                    manifest_path TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX project_exports_project_idx ON project_exports(project_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not create project export storage: {error}"))?;
    }
    if from_version < 8 {
        transaction
            .execute_batch(
                "ALTER TABLE batch_items ADD COLUMN job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL;
                CREATE INDEX batch_items_job_idx ON batch_items(job_id);",
            )
            .map_err(|error| format!("Could not add executable batch job links: {error}"))?;
    }
    if from_version < 9 {
        transaction
            .execute_batch(
                "ALTER TABLE voice_references ADD COLUMN transcript_text TEXT NOT NULL DEFAULT '';
                 ALTER TABLE voice_references ADD COLUMN transcript_source TEXT NOT NULL DEFAULT 'none' CHECK(transcript_source IN ('none','automatic','corrected'));
                 CREATE TABLE voice_reference_revisions (
                    id TEXT PRIMARY KEY,
                    reference_id TEXT NOT NULL REFERENCES voice_references(id) ON DELETE CASCADE,
                    processed_path TEXT NOT NULL,
                    processed_sha256 TEXT NOT NULL,
                    analysis_json TEXT NOT NULL,
                    processing_json TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 );
                 CREATE INDEX voice_reference_revisions_ref_idx ON voice_reference_revisions(reference_id, created_at DESC);
                 CREATE TABLE voice_evaluations (
                    id TEXT PRIMARY KEY,
                    voice_id TEXT NOT NULL REFERENCES voices(id) ON DELETE CASCADE,
                    reference_id TEXT NOT NULL REFERENCES voice_references(id) ON DELETE CASCADE,
                    model_id TEXT NOT NULL,
                    history_id TEXT NOT NULL REFERENCES history(id) ON DELETE CASCADE,
                    script TEXT NOT NULL,
                    decision TEXT NOT NULL CHECK(decision IN ('pending','accepted','rejected')),
                    notes TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE INDEX voice_evaluations_voice_idx ON voice_evaluations(voice_id, updated_at DESC);",
            )
            .map_err(|error| format!("Could not add Voice Lab revision and evaluation storage: {error}"))?;
    }
    if from_version < 10 {
        transaction.execute_batch(
            "ALTER TABLE batch_runs ADD COLUMN pause_requested INTEGER NOT NULL DEFAULT 0 CHECK(pause_requested IN (0,1));",
        ).map_err(|error| format!("Could not add resumable batch controls: {error}"))?;
    }
    if from_version < 11 {
        transaction.execute_batch(
            "CREATE TABLE engine_events (
                id TEXT PRIMARY KEY,
                engine TEXT NOT NULL,
                event TEXT NOT NULL CHECK(event IN ('started','recovered','failed','stopped')),
                detail TEXT NOT NULL,
                created_at TEXT NOT NULL
             );
             CREATE INDEX engine_events_engine_created_idx ON engine_events(engine, created_at DESC);",
        ).map_err(|error| format!("Could not add persistent engine lifecycle evidence: {error}"))?;
    }
    if from_version < 12 {
        transaction.execute_batch(
            "ALTER TABLE jobs ADD COLUMN dismissed INTEGER NOT NULL DEFAULT 0 CHECK(dismissed IN (0,1));
             CREATE INDEX jobs_visible_created_idx ON jobs(dismissed, created_at DESC);",
        ).map_err(|error| format!("Could not add durable task dismissal: {error}"))?;
    }
    if from_version < 13 {
        transaction
            .execute_batch(
                "CREATE TABLE artifact_publications (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                staging_path TEXT NOT NULL,
                final_path TEXT NOT NULL,
                created_at TEXT NOT NULL
             );
             CREATE INDEX artifact_publications_job_idx ON artifact_publications(job_id);",
            )
            .map_err(|error| {
                format!("Could not add atomic artifact publication recovery: {error}")
            })?;
    }
    if from_version < 14 {
        transaction.execute_batch(
            "CREATE TABLE job_events (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                status TEXT NOT NULL CHECK(status IN ('queued','preparing','running','completed','failed','cancelled')),
                progress REAL NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL
             );
             CREATE INDEX job_events_job_created_idx ON job_events(job_id, created_at, id);",
        ).map_err(|error| format!("Could not add durable job progress events: {error}"))?;
        transaction.execute(
            "INSERT INTO job_events (id, job_id, status, progress, error, created_at) SELECT lower(hex(randomblob(16))), id, status, progress, error, updated_at FROM jobs",
            [],
        ).map_err(|error| format!("Could not backfill durable job events: {error}"))?;
    }
    if from_version < 15 {
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS api_job_submissions (
                operation TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                request_sha256 TEXT NOT NULL,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                PRIMARY KEY(operation, idempotency_key)
             );
             CREATE UNIQUE INDEX IF NOT EXISTS api_job_submissions_job_idx ON api_job_submissions(job_id);",
        ).map_err(|error| format!("Could not add durable API idempotency: {error}"))?;
    }
    if from_version < 16 {
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS api_batch_submissions (
                idempotency_key TEXT PRIMARY KEY,
                request_sha256 TEXT NOT NULL,
                batch_id TEXT NOT NULL REFERENCES batch_runs(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS api_batch_submissions_batch_idx ON api_batch_submissions(batch_id);",
        ).map_err(|error| format!("Could not add durable batch idempotency: {error}"))?;
    }
    if from_version < 17 {
        transaction
            .execute_batch(
                "ALTER TABLE batch_items ADD COLUMN name TEXT NOT NULL DEFAULT '';
             ALTER TABLE batch_items ADD COLUMN settings_json TEXT NOT NULL DEFAULT '{}';
             ALTER TABLE batch_items ADD COLUMN output_name TEXT NOT NULL DEFAULT '';
             UPDATE batch_items
             SET name = printf('Row %04d', item_index + 1),
                 output_name = printf('%04d-row', item_index + 1)
             WHERE name = '' OR output_name = '';",
            )
            .map_err(|error| format!("Could not add named batch rows: {error}"))?;
    }
    if from_version < 18 {
        transaction
            .execute_batch("ALTER TABLE batch_items ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0;")
            .map_err(|error| format!("Could not add batch row attempts: {error}"))?;
    }
    if from_version < 19 {
        transaction
            .execute_batch(
                "ALTER TABLE jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 1 CHECK(priority BETWEEN 0 AND 3);
                 ALTER TABLE batch_runs ADD COLUMN priority INTEGER NOT NULL DEFAULT 1 CHECK(priority BETWEEN 0 AND 3);
                 ALTER TABLE batch_items ADD COLUMN priority INTEGER NOT NULL DEFAULT 1 CHECK(priority BETWEEN 0 AND 3);
                 CREATE INDEX jobs_priority_created_idx ON jobs(status, priority DESC, created_at);
                 CREATE INDEX batch_items_priority_idx ON batch_items(batch_id, status, priority DESC, item_index);",
            )
            .map_err(|error| format!("Could not add durable queue priorities: {error}"))?;
    }
    if from_version < 20 {
        transaction
            .execute_batch(
                "CREATE TABLE comparison_runs (
                    id TEXT PRIMARY KEY,
                    script TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('queued','running','completed','partial','failed','cancelled')),
                    blind INTEGER NOT NULL DEFAULT 1 CHECK(blind IN (0,1)),
                    revealed INTEGER NOT NULL DEFAULT 0 CHECK(revealed IN (0,1)),
                    winner_take_id TEXT,
                    promoted_take_id TEXT,
                    notes TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE TABLE comparison_takes (
                    id TEXT PRIMARY KEY,
                    comparison_id TEXT NOT NULL REFERENCES comparison_runs(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 3),
                    label TEXT NOT NULL,
                    request_json TEXT NOT NULL,
                    job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
                    history_id TEXT REFERENCES history(id) ON DELETE SET NULL,
                    status TEXT NOT NULL CHECK(status IN ('queued','preparing','running','completed','failed','cancelled')),
                    rating INTEGER CHECK(rating BETWEEN 1 AND 5),
                    notes TEXT NOT NULL DEFAULT '',
                    favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1)),
                    error TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(comparison_id, position)
                 );
                 CREATE INDEX comparison_runs_updated_idx ON comparison_runs(updated_at DESC);
                 CREATE INDEX comparison_takes_run_idx ON comparison_takes(comparison_id, position);
                 INSERT INTO comparison_runs (id, script, status, blind, revealed, winner_take_id, notes, created_at, updated_at)
                 SELECT id, script, 'completed', 0, 1,
                    CASE winner WHEN 'A' THEN id || ':A' WHEN 'B' THEN id || ':B' ELSE NULL END,
                    notes, created_at, updated_at
                 FROM comparisons;
                 INSERT INTO comparison_takes (id, comparison_id, position, label, request_json, history_id, status, created_at, updated_at)
                 SELECT id || ':A', id, 0, 'A', '{}', left_history_id, 'completed', created_at, updated_at FROM comparisons;
                 INSERT INTO comparison_takes (id, comparison_id, position, label, request_json, history_id, status, created_at, updated_at)
                 SELECT id || ':B', id, 1, 'B', '{}', right_history_id, 'completed', created_at, updated_at FROM comparisons;",
            )
            .map_err(|error| format!("Could not add durable comparison runs and takes: {error}"))?;
    }
    if from_version < 21 {
        transaction
            .execute_batch(
                "ALTER TABLE comparison_runs ADD COLUMN tie INTEGER NOT NULL DEFAULT 0 CHECK(tie IN (0,1));
                 UPDATE comparison_runs SET tie = 1
                 WHERE id IN (SELECT id FROM comparisons WHERE winner = 'tie');",
            )
            .map_err(|error| format!("Could not preserve comparison tie verdicts: {error}"))?;
    }
    if from_version < 22 {
        transaction
            .execute_batch(
                "CREATE TABLE history_exports (
                    id TEXT PRIMARY KEY,
                    history_id TEXT NOT NULL REFERENCES history(id) ON DELETE CASCADE,
                    destination_path TEXT NOT NULL,
                    format TEXT NOT NULL CHECK(format IN ('wav','flac')),
                    size_bytes INTEGER NOT NULL,
                    sha256 TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 );
                 CREATE INDEX history_exports_history_idx ON history_exports(history_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not add history export receipts: {error}"))?;
    }
    if from_version < 23 {
        transaction
            .execute_batch(
                "ALTER TABLE benchmark_runs ADD COLUMN history_id TEXT REFERENCES history(id) ON DELETE SET NULL;
                 ALTER TABLE benchmark_runs ADD COLUMN transcription_id TEXT REFERENCES transcriptions(id) ON DELETE SET NULL;
                 CREATE INDEX benchmark_runs_history_idx ON benchmark_runs(history_id, created_at DESC);
                 CREATE INDEX benchmark_runs_transcription_idx ON benchmark_runs(transcription_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not add benchmark evidence links: {error}"))?;
    }
    if from_version < 24 {
        transaction
            .execute_batch(
                "ALTER TABLE history ADD COLUMN runtime_worker_state TEXT NOT NULL DEFAULT 'unknown';
                 ALTER TABLE history ADD COLUMN end_to_end_seconds REAL NOT NULL DEFAULT 0;
                 ALTER TABLE history ADD COLUMN runtime_overhead_seconds REAL NOT NULL DEFAULT 0;",
            )
            .map_err(|error| format!("Could not add native runtime timing evidence: {error}"))?;
    }
    if from_version < 25 {
        transaction
            .execute_batch(
                "ALTER TABLE voice_evaluations ADD COLUMN speaker_similarity REAL CHECK(speaker_similarity BETWEEN -1 AND 1);
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_model_id TEXT;
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_engine TEXT;
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_scoring_version TEXT;
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_inference_seconds REAL;
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_vram_mb REAL;
                 ALTER TABLE voice_evaluations ADD COLUMN reference_sha256 TEXT;
                 ALTER TABLE voice_evaluations ADD COLUMN candidate_sha256 TEXT;
                 ALTER TABLE voice_evaluations ADD COLUMN similarity_measured_at TEXT;",
            )
            .map_err(|error| format!("Could not add speaker-similarity evidence: {error}"))?;
    }
    if from_version < 26 {
        let original_path_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('transcriptions') WHERE name = 'original_source_path')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect transcription evidence columns: {error}"))?;
        let processing_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('transcriptions') WHERE name = 'processing_json')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect transcription evidence columns: {error}"))?;
        if !original_path_exists {
            transaction
                .execute_batch("ALTER TABLE transcriptions ADD COLUMN original_source_path TEXT NOT NULL DEFAULT '';")
                .map_err(|error| format!("Could not add original transcription evidence: {error}"))?;
        }
        if !processing_exists {
            transaction
                .execute_batch("ALTER TABLE transcriptions ADD COLUMN processing_json TEXT NOT NULL DEFAULT '{}';")
                .map_err(|error| format!("Could not add transcription processing evidence: {error}"))?;
        }
        transaction
            .execute("UPDATE transcriptions SET original_source_path = source_path WHERE original_source_path = ''", [])
            .map_err(|error| format!("Could not backfill original transcription evidence: {error}"))?;
    }
    if from_version < 27 {
        for (column, definition) in [
            ("words_json", "TEXT NOT NULL DEFAULT '[]'"),
            ("detected_language", "TEXT"),
            ("language_confidence", "REAL CHECK(language_confidence BETWEEN 0 AND 1)"),
            (
                "evidence_json",
                "TEXT NOT NULL DEFAULT '{\"schema_version\":0,\"timing_source\":\"unavailable\",\"language_source\":\"unavailable\",\"word_confidence_source\":\"unavailable\"}'",
            ),
        ] {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('transcriptions') WHERE name = ?1)",
                    [column],
                    |row| row.get(0),
                )
                .map_err(|error| format!("Could not inspect transcription evidence schema: {error}"))?;
            if !exists {
                transaction
                    .execute_batch(&format!("ALTER TABLE transcriptions ADD COLUMN {column} {definition};"))
                    .map_err(|error| format!("Could not add transcription evidence column {column}: {error}"))?;
            }
        }
    }
    if from_version < 28 {
        transaction
            .execute_batch(
                "CREATE TABLE transcription_revisions (
                    id TEXT PRIMARY KEY,
                    transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                    text TEXT NOT NULL,
                    segments_json TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX transcription_revisions_latest_idx ON transcription_revisions(transcription_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not add transcript correction revisions: {error}"))?;
    }
    if from_version < 29 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS transcription_diarizations (
                    id TEXT PRIMARY KEY,
                    transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    model_id TEXT NOT NULL,
                    engine TEXT NOT NULL,
                    speakers_json TEXT NOT NULL,
                    turns_json TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    inference_seconds REAL NOT NULL CHECK(inference_seconds >= 0),
                    vram_peak_mb REAL NOT NULL CHECK(vram_peak_mb >= 0),
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS transcription_diarizations_latest_idx
                    ON transcription_diarizations(transcription_id, created_at DESC);
                CREATE TABLE IF NOT EXISTS transcription_speaker_label_revisions (
                    id TEXT PRIMARY KEY,
                    transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                    diarization_id TEXT NOT NULL REFERENCES transcription_diarizations(id) ON DELETE CASCADE,
                    labels_json TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS transcription_speaker_label_revisions_latest_idx
                    ON transcription_speaker_label_revisions(transcription_id, diarization_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not add speaker diarization evidence: {error}"))?;
    }
    if from_version < 30 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS transcription_alignments (
                    id TEXT PRIMARY KEY,
                    transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                    model_id TEXT NOT NULL,
                    engine TEXT NOT NULL,
                    source_revision INTEGER NOT NULL CHECK(source_revision >= 0),
                    source_text_sha256 TEXT NOT NULL,
                    words_json TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    mean_alignment_score REAL NOT NULL CHECK(mean_alignment_score BETWEEN 0 AND 1),
                    inference_seconds REAL NOT NULL CHECK(inference_seconds >= 0),
                    vram_peak_mb REAL NOT NULL CHECK(vram_peak_mb >= 0),
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS transcription_alignments_latest_idx
                    ON transcription_alignments(transcription_id, created_at DESC);",
            )
            .map_err(|error| format!("Could not add forced-alignment evidence: {error}"))?;
    }
    if from_version < 31 {
        let generation_kind_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('history') WHERE name = 'generation_kind')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect durable generation kinds: {error}"))?;
        if !generation_kind_exists {
            transaction
                .execute(
                    "ALTER TABLE history ADD COLUMN generation_kind TEXT NOT NULL DEFAULT 'speech' CHECK(generation_kind IN ('speech', 'music'))",
                    [],
                )
                .map_err(|error| format!("Could not add durable generation kinds: {error}"))?;
        }
        transaction
            .execute_batch(
                "CREATE INDEX IF NOT EXISTS history_generation_kind_created_idx ON history(generation_kind, created_at DESC);",
            )
            .map_err(|error| format!("Could not index durable generation kinds: {error}"))?;
    }
    if from_version < 32 {
        for (name, definition) in [
            ("preview_audio_path", "TEXT"),
            ("preview_duration_seconds", "REAL"),
            ("first_audio_seconds", "REAL"),
        ] {
            let exists: bool = transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('jobs') WHERE name = ?1)",
                    [name],
                    |row| row.get(0),
                )
                .map_err(|error| {
                    format!("Could not inspect progressive audio previews: {error}")
                })?;
            if !exists {
                transaction
                    .execute(
                        &format!("ALTER TABLE jobs ADD COLUMN {name} {definition}"),
                        [],
                    )
                    .map_err(|error| {
                        format!("Could not add progressive audio previews: {error}")
                    })?;
            }
        }
    }
    if from_version < 33 {
        let project_kind_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name = 'project_kind')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect project-kind storage: {error}"))?;
        if !project_kind_exists {
            transaction
                .execute(
                    "ALTER TABLE projects ADD COLUMN project_kind TEXT NOT NULL DEFAULT 'audio' CHECK(project_kind IN ('audio','video'))",
                    [],
                )
                .map_err(|error| format!("Could not add video project kinds: {error}"))?;
        }
        let project_revision_exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name = 'current_revision')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not inspect project revision storage: {error}"))?;
        if !project_revision_exists {
            transaction
                .execute(
                    "ALTER TABLE projects ADD COLUMN current_revision INTEGER NOT NULL DEFAULT 0 CHECK(current_revision >= 0)",
                    [],
                )
                .map_err(|error| format!("Could not add project revision counters: {error}"))?;
        }
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS video_projects (
                    project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    status TEXT NOT NULL CHECK(status IN ('draft','ingesting','analyzing','review','ready','rendering','completed','failed','archived')),
                    aspect_ratio TEXT NOT NULL DEFAULT '9:16',
                    duration_us INTEGER NOT NULL DEFAULT 0 CHECK(duration_us >= 0),
                    current_version_id TEXT,
                    source_summary_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS video_project_versions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    revision INTEGER NOT NULL CHECK(revision > 0),
                    schema_version INTEGER NOT NULL CHECK(schema_version > 0),
                    manifest_json TEXT NOT NULL,
                    manifest_sha256 TEXT NOT NULL,
                    base_revision INTEGER CHECK(base_revision >= 0),
                    actor TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    UNIQUE(project_id, revision)
                 );
                 CREATE INDEX IF NOT EXISTS video_project_versions_latest_idx
                    ON video_project_versions(project_id, revision DESC);
                 CREATE TABLE IF NOT EXISTS video_project_locks (
                    project_id TEXT PRIMARY KEY REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    token TEXT NOT NULL UNIQUE,
                    owner TEXT NOT NULL,
                    acquired_at TEXT NOT NULL,
                    heartbeat_at TEXT NOT NULL,
                    lease_expires_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS projects_kind_updated_idx
                    ON projects(project_kind, updated_at DESC);",
            )
            .map_err(|error| format!("Could not add versioned video projects: {error}"))?;
    }
    if from_version < 34 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS video_rights_receipts (
                    id TEXT PRIMARY KEY,
                    project_id TEXT REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    canonical_url TEXT NOT NULL,
                    url_sha256 TEXT NOT NULL,
                    assertion_version INTEGER NOT NULL CHECK(assertion_version > 0),
                    statement TEXT NOT NULL,
                    confirmed_by TEXT NOT NULL,
                    confirmed_at TEXT NOT NULL,
                    revoked_at TEXT
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS video_rights_active_url_idx
                    ON video_rights_receipts(project_id, url_sha256, assertion_version)
                    WHERE revoked_at IS NULL;
                 CREATE TABLE IF NOT EXISTS video_media_assets (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    kind TEXT NOT NULL CHECK(kind IN ('source','audio','speech','music','image','caption','proxy','thumbnail','waveform','analysis','other')),
                    source_kind TEXT NOT NULL CHECK(source_kind IN ('local','link','generated','derived')),
                    local_path TEXT,
                    original_url TEXT,
                    mime_type TEXT,
                    content_sha256 TEXT,
                    size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes >= 0),
                    duration_us INTEGER CHECK(duration_us IS NULL OR duration_us >= 0),
                    status TEXT NOT NULL CHECK(status IN ('pending','ready','invalid','missing','failed')),
                    probe_json TEXT NOT NULL DEFAULT '{}',
                    provenance_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_media_assets_project_idx
                    ON video_media_assets(project_id, kind, created_at);
                 CREATE INDEX IF NOT EXISTS video_media_assets_hash_idx
                    ON video_media_assets(content_sha256) WHERE content_sha256 IS NOT NULL;
                 CREATE TABLE IF NOT EXISTS video_output_records (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    version_id TEXT REFERENCES video_project_versions(id) ON DELETE SET NULL,
                    job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('preview','master','variation','publish-package','subtitle','thumbnail')),
                    label TEXT NOT NULL,
                    artifact_path TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                    sha256 TEXT NOT NULL,
                    duration_us INTEGER CHECK(duration_us IS NULL OR duration_us >= 0),
                    width INTEGER CHECK(width IS NULL OR width > 0),
                    height INTEGER CHECK(height IS NULL OR height > 0),
                    status TEXT NOT NULL CHECK(status IN ('publishing','ready','invalid','missing','failed')),
                    is_primary INTEGER NOT NULL DEFAULT 0 CHECK(is_primary IN (0,1)),
                    provenance_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_output_project_idx
                    ON video_output_records(project_id, is_primary DESC, created_at DESC);
                 CREATE UNIQUE INDEX IF NOT EXISTS video_output_primary_idx
                    ON video_output_records(project_id) WHERE is_primary = 1 AND status = 'ready';",
            )
            .map_err(|error| format!("Could not add video provenance and artifact storage: {error}"))?;
    }
    if from_version < 35 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS video_workflow_stages (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    version_id TEXT REFERENCES video_project_versions(id) ON DELETE CASCADE,
                    stage_key TEXT NOT NULL,
                    scope_key TEXT NOT NULL DEFAULT '',
                    job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
                    status TEXT NOT NULL CHECK(status IN ('queued','running','interrupted','completed','failed','cancelled','superseded')),
                    resource_class TEXT NOT NULL CHECK(resource_class IN ('light','medium','heavy','exclusive')),
                    attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
                    progress REAL NOT NULL DEFAULT 0 CHECK(progress >= 0 AND progress <= 1),
                    input_sha256 TEXT NOT NULL,
                    output_sha256 TEXT,
                    checkpoint_json TEXT NOT NULL DEFAULT '{}',
                    error_json TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(project_id, version_id, stage_key, scope_key)
                 );
                 CREATE INDEX IF NOT EXISTS video_stage_resume_idx
                    ON video_workflow_stages(status, updated_at);
                 CREATE TABLE IF NOT EXISTS video_stage_dependencies (
                    stage_id TEXT NOT NULL REFERENCES video_workflow_stages(id) ON DELETE CASCADE,
                    depends_on_stage_id TEXT NOT NULL REFERENCES video_workflow_stages(id) ON DELETE CASCADE,
                    PRIMARY KEY(stage_id, depends_on_stage_id),
                    CHECK(stage_id <> depends_on_stage_id)
                 );
                 CREATE TABLE IF NOT EXISTS video_cache_entries (
                    cache_key TEXT PRIMARY KEY,
                    namespace TEXT NOT NULL,
                    project_id TEXT REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    input_json TEXT NOT NULL,
                    artifact_path TEXT NOT NULL,
                    artifact_sha256 TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0),
                    hit_count INTEGER NOT NULL DEFAULT 0 CHECK(hit_count >= 0),
                    created_at TEXT NOT NULL,
                    last_used_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_cache_lru_idx
                    ON video_cache_entries(last_used_at, namespace);
                 CREATE TABLE IF NOT EXISTS video_review_records (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    version_id TEXT NOT NULL REFERENCES video_project_versions(id) ON DELETE CASCADE,
                    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('candidate','scene','transcript','export')),
                    subject_id TEXT NOT NULL,
                    source_fingerprint TEXT NOT NULL,
                    status TEXT NOT NULL CHECK(status IN ('pending','approved','rejected','changes-requested')),
                    review_json TEXT NOT NULL DEFAULT '{}',
                    reviewed_by TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    UNIQUE(project_id, version_id, subject_kind, subject_id)
                 );",
            )
            .map_err(|error| format!("Could not add resumable video workflows: {error}"))?;
    }
    if from_version < 36 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS assistant_video_artifacts (
                    id TEXT PRIMARY KEY,
                    thread_id TEXT NOT NULL,
                    turn_id TEXT,
                    item_id TEXT,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    output_id TEXT REFERENCES video_output_records(id) ON DELETE CASCADE,
                    relationship TEXT NOT NULL CHECK(relationship IN ('project','preview','master','variation','publish-package')),
                    created_at TEXT NOT NULL,
                    UNIQUE(thread_id, item_id, project_id, output_id)
                 );
                 CREATE INDEX IF NOT EXISTS assistant_video_thread_idx
                    ON assistant_video_artifacts(thread_id, created_at DESC);
                 CREATE TABLE IF NOT EXISTS video_performance_samples (
                    id TEXT PRIMARY KEY,
                    project_id TEXT REFERENCES video_projects(project_id) ON DELETE SET NULL,
                    job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
                    operation TEXT NOT NULL,
                    profile TEXT NOT NULL,
                    wall_seconds REAL NOT NULL CHECK(wall_seconds >= 0),
                    media_seconds REAL CHECK(media_seconds IS NULL OR media_seconds >= 0),
                    realtime_factor REAL CHECK(realtime_factor IS NULL OR realtime_factor >= 0),
                    gpu_peak_mb INTEGER CHECK(gpu_peak_mb IS NULL OR gpu_peak_mb >= 0),
                    cache_hit INTEGER NOT NULL DEFAULT 0 CHECK(cache_hit IN (0,1)),
                    details_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_performance_operation_idx
                    ON video_performance_samples(operation, created_at DESC);
                 CREATE TABLE IF NOT EXISTS video_publish_receipts (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    output_id TEXT REFERENCES video_output_records(id) ON DELETE SET NULL,
                    destination_path TEXT NOT NULL,
                    package_manifest_json TEXT NOT NULL,
                    package_sha256 TEXT NOT NULL,
                    created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_publish_project_idx
                    ON video_publish_receipts(project_id, created_at DESC);
                 CREATE TABLE IF NOT EXISTS video_project_events (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    version_id TEXT REFERENCES video_project_versions(id) ON DELETE SET NULL,
                    event_kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL DEFAULT '{}',
                    created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS video_project_events_idx
                    ON video_project_events(project_id, created_at, id);",
            )
            .map_err(|error| format!("Could not add assistant and performance video evidence: {error}"))?;
    }
    if from_version < 37 {
        transaction
            .execute_batch(
                "UPDATE video_workflow_stages
                    SET version_id = (
                        SELECT current_version_id FROM video_projects
                        WHERE video_projects.project_id = video_workflow_stages.project_id
                    )
                  WHERE version_id IS NULL;
                 DELETE FROM video_workflow_stages
                  WHERE rowid NOT IN (
                    SELECT MAX(rowid) FROM video_workflow_stages
                    GROUP BY project_id, COALESCE(version_id, ''), stage_key, scope_key
                  );
                 CREATE UNIQUE INDEX IF NOT EXISTS video_stage_identity_v37
                    ON video_workflow_stages(project_id, COALESCE(version_id, ''), stage_key, scope_key);
                 DELETE FROM video_rights_receipts
                  WHERE rowid NOT IN (
                    SELECT MAX(rowid) FROM video_rights_receipts
                    WHERE revoked_at IS NULL
                    GROUP BY COALESCE(project_id, ''), url_sha256, assertion_version
                    UNION ALL
                    SELECT rowid FROM video_rights_receipts WHERE revoked_at IS NOT NULL
                  );
                 CREATE UNIQUE INDEX IF NOT EXISTS video_rights_identity_v37
                    ON video_rights_receipts(COALESCE(project_id, ''), url_sha256, assertion_version)
                    WHERE revoked_at IS NULL;
                 DELETE FROM assistant_video_artifacts
                  WHERE rowid NOT IN (
                    SELECT MIN(rowid) FROM assistant_video_artifacts
                    GROUP BY thread_id, COALESCE(item_id, ''), project_id, COALESCE(output_id, '')
                  );
                 CREATE UNIQUE INDEX IF NOT EXISTS assistant_video_identity_v37
                    ON assistant_video_artifacts(thread_id, COALESCE(item_id, ''), project_id, COALESCE(output_id, ''));",
            )
            .map_err(|error| {
                format!("Could not harden Video Studio ownership identities: {error}")
            })?;
    }
    if from_version < 38 {
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS video_visual_source_receipts (
                    id TEXT PRIMARY KEY,
                    receipt_kind TEXT NOT NULL CHECK(receipt_kind IN ('user_selected','generated_locally')),
                    project_id TEXT NOT NULL REFERENCES video_projects(project_id) ON DELETE CASCADE,
                    expected_revision INTEGER NOT NULL CHECK(expected_revision > 0),
                    expected_version_id TEXT NOT NULL REFERENCES video_project_versions(id) ON DELETE CASCADE,
                    source_path TEXT NOT NULL,
                    source_device TEXT NOT NULL,
                    source_inode TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
                    modified_seconds INTEGER NOT NULL,
                    modified_nanoseconds INTEGER NOT NULL CHECK(modified_nanoseconds >= 0 AND modified_nanoseconds < 1000000000),
                    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
                    mime_type TEXT NOT NULL CHECK(mime_type IN ('image/png','image/jpeg','image/webp')),
                    width INTEGER NOT NULL CHECK(width > 0),
                    height INTEGER NOT NULL CHECK(height > 0),
                    has_alpha INTEGER NOT NULL CHECK(has_alpha IN (0,1)),
                    producer TEXT NOT NULL,
                    producer_version TEXT,
                    generation_id TEXT,
                    trust_context_json TEXT NOT NULL DEFAULT '{}',
                    issued_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL,
                    claimed_by_job_id TEXT REFERENCES jobs(id),
                    claimed_at TEXT,
                    CHECK(
                        (receipt_kind = 'user_selected' AND generation_id IS NULL)
                        OR
                        (receipt_kind = 'generated_locally' AND length(generation_id) > 0)
                    ),
                    CHECK(
                        (claimed_by_job_id IS NULL AND claimed_at IS NULL)
                        OR
                        (claimed_by_job_id IS NOT NULL AND claimed_at IS NOT NULL)
                    )
                 );
                 CREATE INDEX IF NOT EXISTS video_visual_receipt_expiry_idx
                    ON video_visual_source_receipts(expires_at)
                    WHERE claimed_by_job_id IS NULL;
                 CREATE UNIQUE INDEX IF NOT EXISTS video_generated_visual_identity_idx
                    ON video_visual_source_receipts(project_id, generation_id)
                    WHERE receipt_kind = 'generated_locally';",
            )
            .map_err(|error| {
                format!("Could not add trusted visual source receipts: {error}")
            })?;
    }
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("Could not record database schema version: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit database migration: {error}"))
}

fn transcription_schema_requires_repair(connection: &Connection) -> Result<bool, String> {
    let present: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN (
                'transcription_revisions',
                'transcription_diarizations',
                'transcription_speaker_label_revisions',
                'transcription_alignments'
             )",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not inspect the transcription schema: {error}"))?;
    Ok(present != 4)
}

fn repair_transcription_schema(connection: &mut Connection) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| format!("Could not start transcription schema repair: {error}"))?;
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS transcription_revisions (
                id TEXT PRIMARY KEY,
                transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                text TEXT NOT NULL,
                segments_json TEXT NOT NULL,
                created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS transcription_revisions_latest_idx
                ON transcription_revisions(transcription_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS transcription_diarizations (
                id TEXT PRIMARY KEY,
                transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                model_id TEXT NOT NULL,
                engine TEXT NOT NULL,
                speakers_json TEXT NOT NULL,
                turns_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                inference_seconds REAL NOT NULL CHECK(inference_seconds >= 0),
                vram_peak_mb REAL NOT NULL CHECK(vram_peak_mb >= 0),
                created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS transcription_diarizations_latest_idx
                ON transcription_diarizations(transcription_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS transcription_speaker_label_revisions (
                id TEXT PRIMARY KEY,
                transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                diarization_id TEXT NOT NULL REFERENCES transcription_diarizations(id) ON DELETE CASCADE,
                labels_json TEXT NOT NULL,
                created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS transcription_speaker_label_revisions_latest_idx
                ON transcription_speaker_label_revisions(transcription_id, diarization_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS transcription_alignments (
                id TEXT PRIMARY KEY,
                transcription_id TEXT NOT NULL REFERENCES transcriptions(id) ON DELETE CASCADE,
                job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
                model_id TEXT NOT NULL,
                engine TEXT NOT NULL,
                source_revision INTEGER NOT NULL CHECK(source_revision >= 0),
                source_text_sha256 TEXT NOT NULL,
                words_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                mean_alignment_score REAL NOT NULL CHECK(mean_alignment_score BETWEEN 0 AND 1),
                inference_seconds REAL NOT NULL CHECK(inference_seconds >= 0),
                vram_peak_mb REAL NOT NULL CHECK(vram_peak_mb >= 0),
                created_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS transcription_alignments_latest_idx
                ON transcription_alignments(transcription_id, created_at DESC);",
        )
        .map_err(|error| format!("Could not repair the transcription schema: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("Could not commit transcription schema repair: {error}"))
}

fn verify_database_integrity(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA quick_check")
        .map_err(|error| format!("integrity check could not start: {error}"))?;
    let findings = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("integrity check could not run: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("integrity check could not finish: {error}"))?;
    if findings.len() == 1 && findings[0].eq_ignore_ascii_case("ok") {
        let mut foreign_keys = connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(|error| format!("foreign-key check could not start: {error}"))?;
        let violations = foreign_keys
            .query_map([], |row| {
                Ok(format!(
                    "{} row {} references {}",
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?
                        .map_or_else(|| "unknown".to_string(), |value| value.to_string()),
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("foreign-key check could not run: {error}"))?
            .take(3)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("foreign-key check could not finish: {error}"))?;
        if violations.is_empty() {
            return Ok(());
        }
        return Err(format!("foreign-key violations: {}", violations.join("; ")));
    }
    let summary = findings.into_iter().take(3).collect::<Vec<_>>().join("; ");
    Err(if summary.is_empty() {
        "integrity check returned no result".to_string()
    } else {
        summary
    })
}

fn database_recovery_error(database_path: &Path, data_root: &Path, detail: &str) -> String {
    format!(
        "The soundAr database failed its integrity check ({detail}). No data was changed. Database: {}. Migration backups: {}. Close soundAr, preserve these files, and restore the newest verified backup or report the issue before retrying.",
        database_path.display(),
        data_root.display(),
    )
}

fn create_migration_backup(
    connection: &Connection,
    database_path: &Path,
) -> Result<PathBuf, String> {
    let timestamp = Utc::now().format("%Y%m%d%H%M%S%3f");
    let backup_path = database_path.with_extension(format!("sqlite3.backup-{timestamp}"));
    let temporary_path =
        database_path.with_extension(format!("sqlite3.backup-{timestamp}.partial"));
    let result = (|| -> Result<(), String> {
        connection
            .backup(DatabaseName::Main, &temporary_path, None)
            .map_err(|error| {
                format!("Could not create a consistent database backup before migration: {error}")
            })?;
        let backup = Connection::open(&temporary_path).map_err(|error| {
            format!("Could not open the migration backup for verification: {error}")
        })?;
        verify_database_integrity(&backup)
            .map_err(|error| format!("The migration backup failed verification: {error}"))?;
        drop(backup);
        fs::rename(&temporary_path, &backup_path)
            .map_err(|error| format!("Could not finalize the migration backup: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        fs::remove_file(&temporary_path).ok();
    }
    result.map(|()| backup_path)
}

fn seed_builtin_voices(transaction: &Transaction<'_>) -> Result<(), String> {
    let timestamp = now();
    for (id, name, style) in [
        ("af_heart", "Heart", "Warm American female"),
        ("af_bella", "Bella", "Clear American female"),
        ("am_adam", "Adam", "Natural American male"),
        ("bf_emma", "Emma", "Natural British female"),
    ] {
        transaction
            .execute(
                "INSERT INTO voices (id, name, style, sample_label, sample_seconds, engines_json, consent, state, color, source_kind, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'Built-in Kokoro voice', 0, '[\"Kokoro\"]', 'not-required', 'preset', 'amber', 'preset', ?4, ?4)",
                params![id, name, style, timestamp],
            )
            .map_err(|error| format!("Could not seed built-in voices: {error}"))?;
    }
    Ok(())
}

pub(crate) fn normalize_batch_rows(request: &Value) -> Result<Vec<Value>, String> {
    let default_priority = priority_value(request.get("priority"))?;
    let source = if let Some(rows) = request.get("rows") {
        rows.as_array()
            .ok_or("Batch rows must be an array")?
            .clone()
    } else {
        request
            .get("scripts")
            .and_then(Value::as_array)
            .ok_or("A batch requires a rows or scripts array")?
            .iter()
            .cloned()
            .collect()
    };
    if source.is_empty() || source.len() > 1_000 {
        return Err("A batch must contain between 1 and 1,000 rows".to_string());
    }
    const ALLOWED_SETTINGS: &[&str] = &[
        "model_id",
        "speaker",
        "language",
        "speed",
        "seed",
        "output_format",
        "exaggeration",
        "cfg_weight",
        "temperature",
        "top_p",
        "repetition_penalty",
        "reference_audio_path",
        "voice_name",
        "input_mode",
    ];
    source
        .into_iter()
        .enumerate()
        .map(|(index, raw)| {
            let (text, name, settings, requested_output, requested_priority) =
                if let Some(text) = raw.as_str() {
                    (text.trim(), "", json!({}), "", None)
                } else {
                    let object = raw.as_object().ok_or_else(|| {
                        format!("Batch row {} must be text or an object", index + 1)
                    })?;
                    for key in object.keys() {
                        if !["text", "name", "settings", "output_name", "priority"]
                            .contains(&key.as_str())
                        {
                            return Err(format!(
                                "Batch row {} uses unsupported field '{key}'",
                                index + 1
                            ));
                        }
                    }
                    let text = object
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let name = object
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let settings = object.get("settings").cloned().unwrap_or_else(|| json!({}));
                    let requested_output = object
                        .get("output_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    let requested_priority = object.get("priority");
                    (text, name, settings, requested_output, requested_priority)
                };
            let priority = requested_priority
                .map(|value| priority_value(Some(value)))
                .transpose()?
                .unwrap_or(default_priority);
            if text.is_empty() {
                return Err(format!("Batch row {} has no text", index + 1));
            }
            if text.chars().count() > 20_000 {
                return Err(format!("Batch row {} exceeds 20,000 characters", index + 1));
            }
            if name.chars().count() > 120 {
                return Err(format!(
                    "Batch row {} name exceeds 120 characters",
                    index + 1
                ));
            }
            let settings = settings
                .as_object()
                .ok_or_else(|| format!("Batch row {} settings must be an object", index + 1))?;
            for (key, value) in settings {
                if !ALLOWED_SETTINGS.contains(&key.as_str()) {
                    return Err(format!(
                        "Batch row {} uses unsupported setting '{key}'",
                        index + 1
                    ));
                }
                let valid_type = match key.as_str() {
                    "seed" => value.as_i64().is_some(),
                    "speed" | "exaggeration" | "cfg_weight" | "temperature" | "top_p"
                    | "repetition_penalty" => value.as_f64().is_some(),
                    _ => value.as_str().is_some(),
                };
                if !valid_type {
                    return Err(format!(
                        "Batch row {} has an invalid '{key}' value",
                        index + 1
                    ));
                }
            }
            let display_name = if name.is_empty() {
                text.split(['.', '!', '?'])
                    .next()
                    .unwrap_or(text)
                    .trim()
                    .chars()
                    .take(80)
                    .collect::<String>()
            } else {
                name.to_string()
            };
            let prefix = format!("{:04}-", index + 1);
            let requested_output = requested_output
                .strip_prefix(&prefix)
                .unwrap_or(requested_output);
            let slug_source = if requested_output.is_empty() {
                &display_name
            } else {
                requested_output
            };
            let mut slug = String::new();
            let mut separator = false;
            for character in slug_source.chars() {
                if character.is_ascii_alphanumeric() {
                    slug.push(character.to_ascii_lowercase());
                    separator = false;
                } else if !slug.is_empty() && !separator {
                    slug.push('-');
                    separator = true;
                }
                if slug.len() >= 56 {
                    break;
                }
            }
            let slug = slug.trim_matches('-');
            let slug = if slug.is_empty() { "row" } else { slug };
            Ok(json!({
                "text": text,
                "name": display_name,
                "settings": Value::Object(settings.clone()),
                "output_name": format!("{prefix}{slug}"),
                "priority": priority_name(priority),
            }))
        })
        .collect()
}

fn batch_value(connection: &Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    let run = connection
        .query_row(
            "SELECT id, name, status, total_items, completed_items, failed_items, request_json, error, created_at, updated_at, pause_requested, priority FROM batch_runs WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?, row.get::<_, String>(9)?, row.get::<_, i64>(10)? != 0,
                    row.get::<_, i64>(11)?,
                ))
            },
        )
        .optional()?;
    let Some((
        run_id,
        name,
        status,
        total,
        completed,
        failed,
        request_json,
        error,
        created_at,
        updated_at,
        paused,
        priority,
    )) = run
    else {
        return Ok(None);
    };
    let mut statement = connection.prepare(
        "SELECT id, item_index, text, status, history_id, error, created_at, updated_at, job_id, name, settings_json, output_name, attempt, priority FROM batch_items WHERE batch_id = ?1 ORDER BY item_index",
    )?;
    let items = statement
        .query_map([id], |row| Ok(json!({
            "id": row.get::<_, String>(0)?, "item_index": row.get::<_, i64>(1)?,
            "text": row.get::<_, String>(2)?, "status": row.get::<_, String>(3)?,
            "history_id": row.get::<_, Option<String>>(4)?, "error": row.get::<_, Option<String>>(5)?,
            "created_at": row.get::<_, String>(6)?, "updated_at": row.get::<_, String>(7)?,
            "job_id": row.get::<_, Option<String>>(8)?, "name": row.get::<_, String>(9)?,
            "settings": serde_json::from_str::<Value>(&row.get::<_, String>(10)?).unwrap_or_else(|_| json!({})),
            "output_name": row.get::<_, String>(11)?, "attempt": row.get::<_, i64>(12)?,
            "priority": priority_name(row.get::<_, i64>(13)?)
        })))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(json!({
        "id": run_id, "name": name, "status": if paused { "paused" } else { &status }, "paused": paused, "total_items": total,
        "completed_items": completed, "failed_items": failed,
        "request": serde_json::from_str::<Value>(&request_json).unwrap_or_else(|_| json!({})),
        "error": error, "priority": priority_name(priority), "items": items, "created_at": created_at, "updated_at": updated_at
    })))
}

fn voice_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let engines: String = row.get(5)?;
    let analysis: String = row.get(15)?;
    Ok(json!({
        "id": row.get::<_, String>(0)?,
        "name": row.get::<_, String>(1)?,
        "style": row.get::<_, String>(2)?,
        "sample_label": row.get::<_, String>(3)?,
        "sample_seconds": row.get::<_, f64>(4)?,
        "engines": serde_json::from_str::<Value>(&engines).unwrap_or_else(|_| json!([])),
        "consent": row.get::<_, String>(6)?,
        "state": row.get::<_, String>(7)?,
        "color": row.get::<_, String>(8)?,
        "local_path": row.get::<_, Option<String>>(9)?,
        "source_kind": row.get::<_, String>(10)?,
        "consent_basis": row.get::<_, String>(11)?,
        "speaker_relationship": row.get::<_, String>(12)?,
        "permitted_uses": row.get::<_, String>(13)?,
        "source_date": row.get::<_, String>(14)?,
        "analysis": serde_json::from_str::<Value>(&analysis).unwrap_or_else(|_| json!({})),
    }))
}

fn voice_references(connection: &Connection, voice_id: &str) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, original_path, processed_path, original_sha256, processed_sha256, analysis_json, processing_json, active, created_at, transcript_text, transcript_source,
                    (SELECT COUNT(*) FROM voice_reference_revisions WHERE reference_id = voice_references.id)
             FROM voice_references WHERE voice_id = ?1 ORDER BY created_at",
        )
        .map_err(|error| format!("Could not prepare voice references: {error}"))?;
    let rows = statement
        .query_map([voice_id], |row| {
            let analysis: String = row.get(5)?;
            let processing: String = row.get(6)?;
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "original_path": row.get::<_, String>(1)?,
                "processed_path": row.get::<_, Option<String>>(2)?,
                "original_sha256": row.get::<_, String>(3)?,
                "processed_sha256": row.get::<_, Option<String>>(4)?,
                "analysis": serde_json::from_str::<Value>(&analysis).unwrap_or_else(|_| json!({})),
                "processing": serde_json::from_str::<Value>(&processing).unwrap_or_else(|_| json!({})),
                "active": row.get::<_, bool>(7)?,
                "created_at": row.get::<_, String>(8)?,
                "transcript_text": row.get::<_, String>(9)?,
                "transcript_source": row.get::<_, String>(10)?,
                "revision_count": row.get::<_, i64>(11)?,
            }))
        })
        .map_err(|error| format!("Could not list voice references: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read voice references: {error}"))
}

fn voice_evaluation_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": row.get::<_, String>(0)?, "voice_id": row.get::<_, String>(1)?,
        "reference_id": row.get::<_, String>(2)?, "model_id": row.get::<_, String>(3)?,
        "history_id": row.get::<_, String>(4)?, "script": row.get::<_, String>(5)?,
        "decision": row.get::<_, String>(6)?, "notes": row.get::<_, String>(7)?,
        "created_at": row.get::<_, String>(8)?, "updated_at": row.get::<_, String>(9)?,
        "speaker_similarity": row.get::<_, Option<f64>>(10)?,
        "similarity_model_id": row.get::<_, Option<String>>(11)?,
        "similarity_engine": row.get::<_, Option<String>>(12)?,
        "similarity_scoring_version": row.get::<_, Option<String>>(13)?,
        "similarity_inference_seconds": row.get::<_, Option<f64>>(14)?,
        "similarity_vram_mb": row.get::<_, Option<f64>>(15)?,
        "reference_sha256": row.get::<_, Option<String>>(16)?,
        "candidate_sha256": row.get::<_, Option<String>>(17)?,
        "similarity_measured_at": row.get::<_, Option<String>>(18)?,
    }))
}

fn voice_evaluations(connection: &Connection, voice_id: &str) -> Result<Vec<Value>, String> {
    let mut statement = connection.prepare(
        "SELECT id, voice_id, reference_id, model_id, history_id, script, decision, notes, created_at, updated_at, speaker_similarity, similarity_model_id, similarity_engine, similarity_scoring_version, similarity_inference_seconds, similarity_vram_mb, reference_sha256, candidate_sha256, similarity_measured_at FROM voice_evaluations WHERE voice_id = ?1 ORDER BY updated_at DESC",
    ).map_err(|error| format!("Could not prepare voice evaluations: {error}"))?;
    let rows = statement
        .query_map([voice_id], voice_evaluation_from_row)
        .map_err(|error| format!("Could not list voice evaluations: {error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read voice evaluations: {error}"))
}

fn backfill_voice_provenance(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare(
            "SELECT id, local_path, consent_basis, speaker_relationship, permitted_uses, source_date, created_at
             FROM voices
             WHERE source_kind != 'preset' AND local_path IS NOT NULL
               AND NOT EXISTS(SELECT 1 FROM voice_references WHERE voice_id = voices.id)",
        )
        .map_err(|error| format!("Could not inspect legacy voice profiles: {error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("Could not read legacy voice profiles: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read legacy voice profile: {error}"))?;
    drop(statement);

    for (voice_id, path, basis, relationship, uses, source_date, created_at) in rows {
        let managed_path = PathBuf::from(&path);
        if !managed_path.is_file() {
            continue;
        }
        let checksum = sha256_file(&managed_path)?;
        connection
            .execute(
                "INSERT INTO voice_references (id, voice_id, original_path, processed_path, original_sha256, processed_sha256, analysis_json, processing_json, active, created_at)
                 VALUES (?1, ?2, ?3, ?3, ?4, ?4, '{}', '{\"schema_version\":0,\"legacy_import\":true}', 1, ?5)",
                params![Uuid::new_v4().simple().to_string(), voice_id, path, checksum, created_at],
            )
            .map_err(|error| format!("Could not preserve legacy voice provenance: {error}"))?;
        connection
            .execute(
                "INSERT INTO consent_records (id, voice_id, basis, speaker_relationship, permitted_uses, source_date, acknowledged_at, revoked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![Uuid::new_v4().simple().to_string(), voice_id, basis, relationship, uses, source_date, created_at],
            )
            .map_err(|error| format!("Could not preserve legacy consent evidence: {error}"))?;
    }
    Ok(())
}

fn history_value(
    id: &str,
    job_id: &str,
    title: &str,
    voice: &str,
    text: &str,
    model_id: &str,
    engine: &str,
    generation_kind: &str,
    audio_path: &Path,
    sample_rate: i64,
    duration_seconds: f64,
    inference_seconds: f64,
    rtf: f64,
    vram_peak_mb: i64,
    waveform: Value,
    created_at: &str,
    artifact_state: &str,
    favorite: bool,
    notes: &str,
    runtime_worker_state: &str,
    end_to_end_seconds: f64,
    runtime_overhead_seconds: f64,
) -> Value {
    json!({
        "id": id,
        "job_id": job_id,
        "title": title,
        "voice": voice,
        "text": text,
        "model_id": model_id,
        "engine": engine,
        "generation_kind": generation_kind,
        "audio_path": audio_path.to_string_lossy(),
        "sample_rate": sample_rate,
        "duration_seconds": duration_seconds,
        "inference_seconds": inference_seconds,
        "rtf": rtf,
        "vram_peak_mb": vram_peak_mb,
        "waveform": waveform,
        "created_at": created_at,
        "preview": false,
        "missing": artifact_state == "missing",
        "artifact_state": artifact_state,
        "favorite": favorite,
        "notes": notes,
        "runtime_worker_state": runtime_worker_state,
        "end_to_end_seconds": end_to_end_seconds,
        "runtime_overhead_seconds": runtime_overhead_seconds,
    })
}

fn artifact_file_state(path: &Path, expected_size: i64) -> String {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() as i64 == expected_size => "available",
        Ok(_) => "modified",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "modified",
    }
    .to_string()
}

fn cleanup_partial_artifacts(root: &Path) -> Result<(), String> {
    let entries = fs::read_dir(root)
        .map_err(|error| format!("Could not inspect partial artifacts: {error}"))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Could not inspect a partial artifact: {error}"))?;
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".partial"))
        {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "Could not remove incomplete artifact {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn cleanup_preview_artifacts(root: &Path) -> Result<(), String> {
    let entries = fs::read_dir(root)
        .map_err(|error| format!("Could not inspect preview artifacts: {error}"))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Could not inspect a preview artifact: {error}"))?;
        let path = entry.path();
        let is_preview = path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".preview-") && name.ends_with(".wav"));
        if is_preview {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "Could not remove interrupted preview {}: {error}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn recover_incomplete_publications(
    connection: &mut Connection,
    artifacts_root: &Path,
) -> Result<(), String> {
    let root = artifacts_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve artifact recovery storage: {error}"))?;
    let publications = {
        let mut statement = connection
            .prepare("SELECT id, job_id, staging_path, final_path FROM artifact_publications")
            .map_err(|error| {
                format!("Could not inspect interrupted artifact publications: {error}")
            })?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|error| format!("Could not read interrupted artifact publications: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                format!("Could not read interrupted artifact publications: {error}")
            })?;
        rows
    };
    for (publication_id, job_id, staging_raw, final_raw) in publications {
        for raw in [&staging_raw, &final_raw] {
            let path = PathBuf::from(raw);
            let parent = path.parent().and_then(|value| value.canonicalize().ok());
            if parent
                .as_ref()
                .is_some_and(|value| value.starts_with(&root))
            {
                fs::remove_file(path).ok();
            }
        }
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not recover artifact publication: {error}"))?;
        transaction
            .execute(
                "UPDATE jobs SET status = 'failed', error = 'Audio publication was interrupted; retry this task', updated_at = ?2 WHERE id = ?1 AND status != 'cancelled'",
                params![job_id, now()],
            )
            .map_err(|error| format!("Could not recover the interrupted generation: {error}"))?;
        let progress: f64 = transaction
            .query_row(
                "SELECT progress FROM jobs WHERE id = ?1",
                [&job_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read interrupted publication progress: {error}"))?;
        insert_job_event(
            &transaction,
            &job_id,
            "failed",
            progress,
            Some("Audio publication was interrupted; retry this task"),
            &now(),
        )?;
        transaction
            .execute(
                "DELETE FROM artifact_publications WHERE id = ?1",
                [publication_id],
            )
            .map_err(|error| {
                format!("Could not clear interrupted artifact publication: {error}")
            })?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit artifact recovery: {error}"))?;
    }
    Ok(())
}

fn validate_job_status(status: &str) -> Result<(), String> {
    if matches!(
        status,
        "queued" | "preparing" | "running" | "completed" | "failed" | "cancelled"
    ) {
        Ok(())
    } else {
        Err("Invalid job state".to_string())
    }
}

fn insert_job_event(
    transaction: &Transaction<'_>,
    job_id: &str,
    status: &str,
    progress: f64,
    error: Option<&str>,
    created_at: &str,
) -> Result<(), String> {
    transaction.execute(
        "INSERT INTO job_events (id, job_id, status, progress, error, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![Uuid::new_v4().simple().to_string(), job_id, status, progress.clamp(0.0, 1.0), error, created_at],
    ).map_err(|cause| format!("Could not record the job event: {cause}"))?;
    Ok(())
}

fn validate_audio_file(path: &Path) -> Result<(), String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("Could not open generated audio: {error}"))?;
    let mut header = [0u8; 12];
    let read = file
        .read(&mut header)
        .map_err(|error| format!("Could not inspect generated audio: {error}"))?;
    let valid_wav = read >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WAVE";
    let valid_flac = read >= 4 && &header[..4] == b"fLaC";
    if !valid_wav && !valid_flac {
        return Err("The inference engine produced an invalid audio file".to_string());
    }
    Ok(())
}

fn bounded_optional_text(
    value: &Value,
    key: &str,
    maximum: usize,
) -> Result<Option<String>, String> {
    let Some(text) = value.get(key).and_then(Value::as_str) else {
        return Ok(None);
    };
    let text = text.trim();
    if text.len() > maximum {
        return Err(format!("Benchmark {key} is too long"));
    }
    Ok((!text.is_empty()).then(|| text.to_string()))
}

fn durable_request(request: &Value) -> Value {
    let mut durable = request.clone();
    if let Some(object) = durable.as_object_mut() {
        object.remove("benchmark_token");
    }
    durable
}

fn normalize_metric_words(value: &str) -> Vec<String> {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            (character.is_alphanumeric() || character == '\'' || character.is_whitespace())
                .then_some(character)
                .unwrap_or(' ')
        })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn edit_distance<T: Eq>(expected: &[T], actual: &[T]) -> usize {
    let mut prior = (0..=actual.len()).collect::<Vec<_>>();
    for (row, expected_value) in expected.iter().enumerate() {
        let mut current = vec![row + 1];
        for (column, actual_value) in actual.iter().enumerate() {
            let substitution = prior[column] + usize::from(expected_value != actual_value);
            current.push(
                (prior[column + 1] + 1)
                    .min(current[column] + 1)
                    .min(substitution),
            );
        }
        prior = current;
    }
    prior[actual.len()]
}

fn metric_error_rate(errors: usize, reference_length: usize, actual_length: usize) -> f64 {
    if reference_length == 0 {
        f64::from(actual_length > 0)
    } else {
        errors as f64 / reference_length as f64
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not checksum generated audio: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not checksum generated audio: {error}"))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_store_json_atomically(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not prepare project provenance storage: {error}"))?;
    }
    let temporary = path.with_extension(format!(
        "{}.partial",
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or("json")
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("Could not encode project provenance: {error}"))?;
    fs::write(&temporary, bytes)
        .map_err(|error| format!("Could not write project provenance: {error}"))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("Could not publish project provenance: {error}"))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn optional_history_filter(
    filters: Option<&Value>,
    key: &str,
    maximum: usize,
) -> Result<Option<String>, String> {
    let value = filters
        .and_then(|filters| filters.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if value.is_some_and(|value| value.len() > maximum) {
        return Err(format!("History {key} filter is too long"));
    }
    Ok(value.map(str::to_string))
}

fn valid_speaker_id(value: &str) -> bool {
    value
        .strip_prefix("speaker-")
        .and_then(|suffix| suffix.parse::<u8>().ok())
        .is_some_and(|index| (1..=8).contains(&index))
}

fn latest_transcription_diarization(
    connection: &Connection,
    transcription_id: &str,
) -> Result<Option<Value>, String> {
    let record: Option<(
        String, String, String, String, String, String, String, f64, f64, String,
    )> = connection
        .query_row(
            "SELECT id, job_id, model_id, engine, speakers_json, turns_json, evidence_json, inference_seconds, vram_peak_mb, created_at FROM transcription_diarizations WHERE transcription_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [transcription_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read diarization evidence: {error}"))?;
    let Some((
        id,
        job_id,
        model_id,
        engine,
        speakers_json,
        turns_json,
        evidence_json,
        inference_seconds,
        vram_peak_mb,
        created_at,
    )) = record
    else {
        return Ok(None);
    };
    let label_revision: Option<(String, String)> = connection
        .query_row(
            "SELECT labels_json, created_at FROM transcription_speaker_label_revisions WHERE transcription_id = ?1 AND diarization_id = ?2 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![transcription_id, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read speaker labels: {error}"))?;
    let label_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM transcription_speaker_label_revisions WHERE transcription_id = ?1 AND diarization_id = ?2",
            params![transcription_id, id],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not count speaker-label revisions: {error}"))?;
    let speakers = serde_json::from_str::<Value>(&speakers_json).unwrap_or_else(|_| json!([]));
    let defaults = speakers
        .as_array()
        .map(|items| {
            Value::Object(
                items
                    .iter()
                    .filter_map(|speaker| {
                        Some((
                            speaker.get("id")?.as_str()?.to_string(),
                            json!(speaker.get("default_name")?.as_str()?),
                        ))
                    })
                    .collect(),
            )
        })
        .unwrap_or_else(|| json!({}));
    let labels = label_revision
        .as_ref()
        .and_then(|(raw, _)| serde_json::from_str::<Value>(raw).ok())
        .unwrap_or(defaults);
    Ok(Some(json!({
        "id": id,
        "job_id": job_id,
        "model_id": model_id,
        "engine": engine,
        "speakers": speakers,
        "turns": serde_json::from_str::<Value>(&turns_json).unwrap_or_else(|_| json!([])),
        "evidence": serde_json::from_str::<Value>(&evidence_json).unwrap_or_else(|_| json!({})),
        "inference_seconds": inference_seconds,
        "vram_peak_mb": vram_peak_mb,
        "labels": labels,
        "label_revision_count": label_count,
        "labels_updated_at": label_revision.map(|(_, created_at)| created_at),
        "created_at": created_at,
    })))
}

fn latest_transcription_alignment(
    connection: &Connection,
    transcription_id: &str,
    current_revision: i64,
    current_text_sha256: &str,
) -> Result<Option<Value>, String> {
    let record: Option<(
        String, String, String, String, i64, String, String, String, f64, f64, f64, String,
    )> = connection
        .query_row(
            "SELECT id, job_id, model_id, engine, source_revision, source_text_sha256, words_json, evidence_json, mean_alignment_score, inference_seconds, vram_peak_mb, created_at
             FROM transcription_alignments WHERE transcription_id = ?1
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            [transcription_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read forced-alignment evidence: {error}"))?;
    let Some((
        id,
        job_id,
        model_id,
        engine,
        source_revision,
        source_text_sha256,
        words_json,
        evidence_json,
        mean_alignment_score,
        inference_seconds,
        vram_peak_mb,
        created_at,
    )) = record
    else {
        return Ok(None);
    };
    Ok(Some(json!({
        "id": id,
        "job_id": job_id,
        "model_id": model_id,
        "engine": engine,
        "source_revision": source_revision,
        "source_text_sha256": source_text_sha256,
        "words": serde_json::from_str::<Value>(&words_json).unwrap_or_else(|_| json!([])),
        "evidence": serde_json::from_str::<Value>(&evidence_json).unwrap_or_else(|_| json!({})),
        "mean_alignment_score": mean_alignment_score,
        "inference_seconds": inference_seconds,
        "vram_peak_mb": vram_peak_mb,
        "current": source_revision == current_revision && source_text_sha256 == current_text_sha256,
        "created_at": created_at,
    })))
}

fn required_trimmed<'a>(request: &'a Value, key: &str, message: &str) -> Result<&'a str, String> {
    request
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| message.to_string())
}

fn numeric_value(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|number| number as f64))
        .or_else(|| value.as_u64().map(|number| number as f64))
}

fn alignment_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_alphabetic() || (character == '\'' && !current.is_empty()) {
            current.push(character);
        } else if !current.is_empty() {
            if current.ends_with('\'') {
                current.pop();
            }
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        }
    }
    if current.ends_with('\'') {
        current.pop();
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn title_from_text(text: &str) -> String {
    let value = text
        .split(['.', '!', '?', '\n'])
        .next()
        .unwrap_or("Untitled generation")
        .trim();
    if value.is_empty() {
        return "Untitled generation".to_string();
    }
    value.chars().take(56).collect()
}

fn manifest_duration_us(manifest: &Value) -> i64 {
    manifest
        .get("timeline_duration_us")
        .or_else(|| manifest.get("duration_us"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0)
}

fn manifest_aspect_ratio(manifest: &Value) -> &'static str {
    let width = manifest
        .pointer("/layout/canvas/width")
        .and_then(Value::as_u64)
        .unwrap_or(1080);
    let height = manifest
        .pointer("/layout/canvas/height")
        .and_then(Value::as_u64)
        .unwrap_or(1920);
    if width == height {
        "1:1"
    } else if width > height {
        "16:9"
    } else {
        "9:16"
    }
}

fn ensure_video_publication_active(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("video.cancelled: Video output publication was cancelled".into())
    } else {
        Ok(())
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

fn ensure_private_directory(path: &Path, label: &str) -> Result<(), String> {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .map_err(|error| format!("Could not create the {label} directory: {error}"))?;
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the {label} directory: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "The {label} directory must be a regular directory owned by the current user"
        ));
    }
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(format!(
            "The {label} directory is not owned by the current user"
        ));
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!("Could not secure the {label} directory for private local media: {error}")
        })?;
    }
    Ok(())
}

fn secure_private_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the {label}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "The {label} must be a regular file owned by the current user"
        ));
    }
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(format!("The {label} is not owned by the current user"));
    }
    if metadata.permissions().mode() & 0o777 != 0o600 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!("Could not secure the {label} for private local data: {error}")
        })?;
    }
    Ok(())
}

pub(crate) fn priority_value(value: Option<&Value>) -> Result<i64, String> {
    match value {
        None | Some(Value::Null) => Ok(1),
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(0),
            "normal" => Ok(1),
            "high" => Ok(2),
            "urgent" => Ok(3),
            _ => Err("Priority must be low, normal, high, or urgent".to_string()),
        },
        Some(Value::Number(value)) => value
            .as_i64()
            .filter(|value| (0..=3).contains(value))
            .ok_or_else(|| "Priority must be an integer from 0 to 3".to_string()),
        _ => {
            Err("Priority must be low, normal, high, urgent, or an integer from 0 to 3".to_string())
        }
    }
}

fn priority_name(value: i64) -> &'static str {
    match value {
        0 => "low",
        2 => "high",
        3 => "urgent",
        _ => "normal",
    }
}

#[cfg(test)]
mod tests {
    use super::{create_migration_backup, sha256_file, Store};
    use rusqlite::params;
    use serde_json::json;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        thread,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    fn test_store() -> (Store, PathBuf) {
        let root = std::env::temp_dir().join(format!("soundar-store-{}", Uuid::new_v4()));
        let store = Store::open(root.join("data"), root.join("artifacts")).expect("open store");
        (store, root)
    }

    #[test]
    fn managed_state_and_media_roots_are_owner_only() {
        let root = std::env::temp_dir().join(format!("soundar-private-store-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create permissive data directory");
        fs::create_dir_all(&artifacts).expect("create permissive artifact directory");
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755))
            .expect("make data fixture permissive");
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o755))
            .expect("make artifact fixture permissive");

        let store = Store::open(data.clone(), artifacts.clone()).expect("secure managed roots");
        assert_eq!(
            fs::metadata(&data)
                .expect("data metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&artifacts)
                .expect("artifact metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(data.join("soundar.sqlite3"))
                .expect("database metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(artifacts.join("video"))
                .expect("video directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fresh_database_seeds_real_builtin_voices() {
        let (store, root) = test_store();
        let voices = store.list_voices().expect("list voices");
        assert_eq!(voices.len(), 4);
        assert!(voices.iter().all(|voice| voice["state"] == "preset"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn application_settings_are_typed_and_survive_restart() {
        let (store, root) = test_store();
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let defaults = store.application_settings().expect("default settings");
        assert_eq!(
            defaults,
            json!({"theme": "light", "dense_tables": true, "reduced_motion": false})
        );
        store
            .save_application_setting("theme", &json!("light"))
            .expect("save theme");
        store
            .save_application_setting("dense_tables", &json!(false))
            .expect("save density");
        store
            .save_application_setting("reduced_motion", &json!(true))
            .expect("save motion");
        assert!(store
            .save_application_setting("theme", &json!("purple"))
            .is_err());
        assert!(store
            .save_application_setting("unknown", &json!(true))
            .is_err());
        drop(store);

        let reopened = Store::open(data, artifacts).expect("reopen settings store");
        assert_eq!(
            reopened.application_settings().expect("persisted settings"),
            json!({
                "theme": "light", "dense_tables": false, "reduced_motion": true
            })
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn benchmark_scores_are_derived_from_matching_durable_evidence() {
        let (store, root) = test_store();
        let audio = root.join("artifacts/benchmark-evidence.wav");
        fs::write(&audio, b"RIFF\x10\x00\x00\x00WAVEbenchmark-evidence")
            .expect("write benchmark audio");
        let request = json!({
            "model_id": "hexgrad/Kokoro-82M", "text": "One two three.",
            "speaker": "af_heart", "language": "en", "output_format": "wav"
        });
        let generation_job = store
            .create_job("synthesis", &request)
            .expect("generation job");
        store.start_job(&generation_job).expect("start generation");
        store
            .complete_synthesis(
                &generation_job,
                &request,
                &json!({
                    "id": "benchmark-history", "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                    "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.5,
                    "inference_seconds": 0.3, "rtf": 0.2, "vram_peak_mb": 640, "waveform": [0.4],
                    "runtime_worker_state": "cold", "end_to_end_seconds": 0.8,
                    "runtime_overhead_seconds": 0.5
                }),
            )
            .expect("complete generation");
        let imported = store
            .import_transcription_source(
                root.join("artifacts/benchmark-evidence.wav")
                    .to_str()
                    .expect("audio path"),
            )
            .expect("import generated audio for transcription");
        let transcription_request =
            json!({"model_id": "openai/whisper-tiny", "audio_path": imported});
        let transcription_job = store
            .create_job("transcription", &transcription_request)
            .expect("transcription job");
        store
            .start_job(&transcription_job)
            .expect("start transcription");
        let transcription = store
            .complete_transcription(
                &transcription_job,
                imported.to_str().expect("import path"),
                imported.to_str().expect("original import path"),
                &json!({ "algorithm": "none" }),
                &json!({
                    "model_id": "openai/whisper-tiny", "engine": "transformers",
                    "text": "one four three", "segments": [],
                    "words": [{"text":"one","start_seconds":0.1,"end_seconds":0.4,"confidence":null,"end_inferred":false}],
                    "detected_language": "en", "language_confidence": 0.97,
                    "evidence": {"schema_version":1,"timing_source":"whisper-token-alignment","language_source":"whisper-decoder-logits","word_confidence_source":"unavailable"},
                    "audio_duration_seconds": 1.5,
                    "inference_seconds": 0.2, "rtf": 0.13, "vram_peak_mb": 400
                }),
            )
            .expect("complete transcription");
        assert_eq!(
            transcription["original_source_path"],
            imported.to_string_lossy().as_ref()
        );
        assert_eq!(transcription["processing"]["algorithm"], "none");
        let persisted_transcription = store
            .list_transcriptions()
            .expect("list transcription evidence")
            .into_iter()
            .find(|value| value["id"] == transcription["id"])
            .expect("persisted transcription");
        assert_eq!(persisted_transcription["processing"]["algorithm"], "none");
        assert_eq!(persisted_transcription["words"][0]["text"], "one");
        assert_eq!(persisted_transcription["detected_language"], "en");
        assert_eq!(persisted_transcription["language_confidence"], 0.97);
        assert_eq!(
            persisted_transcription["evidence"]["timing_source"],
            "whisper-token-alignment"
        );
        let benchmark = store
            .save_benchmark(&json!({
                "history_id": "benchmark-history", "transcription_id": transcription["id"],
                "warm_state": "warm", "word_error_rate": 0.0, "character_error_rate": 0.0,
                "gpu_name": "Test GPU", "app_version": "test"
            }))
            .expect("save benchmark evidence");
        assert_eq!(benchmark["model_id"], "hexgrad/Kokoro-82M");
        assert_eq!(benchmark["verifier_model_id"], "openai/whisper-tiny");
        assert_eq!(benchmark["word_errors"], 1);
        assert_eq!(benchmark["reference_words"], 3);
        assert!(
            (benchmark["word_error_rate"].as_f64().expect("WER") - 1.0 / 3.0).abs() < 0.000_001
        );
        assert!(benchmark["character_error_rate"].as_f64().expect("CER") > 0.0);
        assert_eq!(benchmark["scoring_version"], "soundar-unicode-v1");
        assert_eq!(benchmark["warm_state"], "cold");
        assert_eq!(benchmark["end_to_end_seconds"], 0.8);
        assert_eq!(store.list_benchmarks().expect("list benchmarks").len(), 1);

        let other = root.join("unrelated.wav");
        fs::write(&other, b"RIFF\x10\x00\x00\x00WAVEunrelated-audio").expect("write other audio");
        let other_import = store
            .import_transcription_source(other.to_str().expect("other path"))
            .expect("import other audio");
        let other_job = store
            .create_job("transcription", &json!({"model_id":"openai/whisper-tiny"}))
            .expect("other job");
        store.start_job(&other_job).expect("start other job");
        let other_transcription = store.complete_transcription(&other_job, other_import.to_str().expect("other import"), other_import.to_str().expect("other original"), &json!({ "algorithm": "none" }), &json!({
            "model_id":"openai/whisper-tiny", "engine":"transformers", "text":"one two three",
            "segments":[], "audio_duration_seconds":1.0, "inference_seconds":0.1, "rtf":0.1, "vram_peak_mb":100
        })).expect("other transcription");
        assert!(store
            .save_benchmark(&json!({
                "history_id":"benchmark-history", "transcription_id":other_transcription["id"]
            }))
            .expect_err("reject unrelated audio")
            .contains("exact generated artifact"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transcript_corrections_are_append_only_timed_and_restart_safe() {
        let (store, root) = test_store();
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let source = root.join("source.wav");
        fs::write(&source, b"RIFF\x10\x00\x00\x00WAVEtranscript-source")
            .expect("write transcript source");
        let imported = store
            .import_transcription_source(source.to_str().expect("source path"))
            .expect("import transcript source");
        let job = store
            .create_job("transcription", &json!({"model_id":"openai/whisper-tiny"}))
            .expect("create transcription job");
        store.start_job(&job).expect("start transcription job");
        let transcript = store
            .complete_transcription(
                &job,
                imported.to_str().expect("imported path"),
                imported.to_str().expect("original path"),
                &json!({"algorithm":"none"}),
                &json!({
                    "model_id":"openai/whisper-tiny", "engine":"transformers",
                    "text":"Model words", "segments":[{"text":"Model words","start_seconds":0.2,"end_seconds":1.4}],
                    "words":[], "audio_duration_seconds":1.5, "inference_seconds":0.1,
                    "rtf":0.06, "vram_peak_mb":400
                }),
            )
            .expect("complete transcription");
        let id = transcript["id"].as_str().expect("transcript id");
        let corrected_segments =
            json!([{"text":"Corrected words","start_seconds":0.2,"end_seconds":1.4}]);
        let first = store
            .update_transcription(id, "Corrected words", &corrected_segments)
            .expect("save first correction");
        assert_eq!(first["revision_count"], 1);
        let duplicate = store
            .update_transcription(id, "Corrected words", &corrected_segments)
            .expect("deduplicate correction");
        assert_eq!(duplicate["revision_count"], 1);
        let tampered = json!([{"text":"Tampered","start_seconds":0.3,"end_seconds":1.4}]);
        assert!(store
            .update_transcription(id, "Tampered", &tampered)
            .expect_err("reject timing edit")
            .contains("cannot change measured timestamps"));
        let final_segments =
            json!([{"text":"Final corrected words","start_seconds":0.2,"end_seconds":1.4}]);
        let final_revision = store
            .update_transcription(id, "Final corrected words", &final_segments)
            .expect("save final correction");
        assert_eq!(final_revision["revision_count"], 2);
        drop(store);

        let reopened = Store::open(data, artifacts).expect("reopen corrected transcript store");
        let persisted = reopened
            .list_transcriptions()
            .expect("list corrected transcripts")
            .into_iter()
            .find(|record| record["id"] == id)
            .expect("persisted corrected transcript");
        assert_eq!(persisted["text"], "Final corrected words");
        assert_eq!(persisted["segments"][0]["text"], "Final corrected words");
        assert_eq!(persisted["original_text"], "Model words");
        assert_eq!(persisted["revision_count"], 2);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn speaker_diarization_and_label_revisions_are_durable_and_bounded() {
        let (store, root) = test_store();
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let source = root.join("conversation.wav");
        fs::write(&source, b"RIFF\x10\x00\x00\x00WAVEconversation")
            .expect("write conversation source");
        let imported = store
            .import_transcription_source(source.to_str().expect("source path"))
            .expect("import conversation source");
        let transcription_job = store
            .create_job("transcription", &json!({"model_id":"openai/whisper-tiny"}))
            .expect("create transcription job");
        store
            .start_job(&transcription_job)
            .expect("start transcription job");
        let transcript = store
            .complete_transcription(
                &transcription_job,
                imported.to_str().expect("imported path"),
                imported.to_str().expect("original path"),
                &json!({"algorithm":"none"}),
                &json!({
                    "model_id":"openai/whisper-tiny", "engine":"transformers",
                    "text":"Welcome aboard Good to be here",
                    "segments":[
                        {"text":"Welcome aboard","start_seconds":0.1,"end_seconds":1.2},
                        {"text":"Good to be here","start_seconds":1.5,"end_seconds":2.9}
                    ],
                    "words":[
                        {"text":"Welcome","start_seconds":0.1,"end_seconds":0.6},
                        {"text":"aboard","start_seconds":0.65,"end_seconds":1.2},
                        {"text":"Good","start_seconds":1.5,"end_seconds":1.85},
                        {"text":"to","start_seconds":1.9,"end_seconds":2.1},
                        {"text":"be","start_seconds":2.15,"end_seconds":2.35},
                        {"text":"here","start_seconds":2.4,"end_seconds":2.9}
                    ],
                    "evidence":{"schema_version":1,"timing_source":"model-word-timestamps"},
                    "audio_duration_seconds":3.0, "inference_seconds":0.1,
                    "rtf":0.03, "vram_peak_mb":400
                }),
            )
            .expect("complete conversation transcription");
        let transcription_id = transcript["id"].as_str().expect("transcription id");
        let request = store
            .transcription_diarization_request(transcription_id)
            .expect("prepare diarization evidence");
        assert_eq!(request["words"].as_array().map(Vec::len), Some(6));

        let diarization_job = store
            .create_job(
                "speaker-diarization",
                &json!({"model_id":"microsoft/wavlm-base-plus-sv"}),
            )
            .expect("create diarization job");
        store
            .start_job(&diarization_job)
            .expect("start diarization job");
        let diarization = store
            .complete_transcription_diarization(
                &diarization_job,
                transcription_id,
                &json!({
                    "model_id":"microsoft/wavlm-base-plus-sv",
                    "engine":"speaker-verification",
                    "speakers":[
                        {"id":"speaker-1","default_name":"Speaker 1"},
                        {"id":"speaker-2","default_name":"Speaker 2"}
                    ],
                    "turns":[
                        {"speaker_id":"speaker-1","start_seconds":0.1,"end_seconds":1.2,"word_start_index":0,"word_end_index":1,"text":"Welcome aboard","confidence":null},
                        {"speaker_id":"speaker-2","start_seconds":1.5,"end_seconds":2.9,"word_start_index":2,"word_end_index":5,"text":"Good to be here","confidence":null}
                    ],
                    "evidence":{"schema_version":1,"provisional":true,"overlap_detection":false,"confidence_source":"unavailable"},
                    "inference_seconds":0.2,
                    "vram_peak_mb":512
                }),
            )
            .expect("complete diarization");
        assert_eq!(diarization["labels"]["speaker-1"], "Speaker 1");
        assert_eq!(diarization["evidence"]["provisional"], true);

        let labels = json!({"speaker-1":"Host","speaker-2":"Guest"});
        let first = store
            .update_transcription_speaker_labels(transcription_id, &labels)
            .expect("save speaker labels");
        assert_eq!(first["label_revision_count"], 1);
        let duplicate = store
            .update_transcription_speaker_labels(transcription_id, &labels)
            .expect("deduplicate speaker labels");
        assert_eq!(duplicate["label_revision_count"], 1);
        assert!(store
            .update_transcription_speaker_labels(
                transcription_id,
                &json!({"speaker-1":"Host", "speaker-3":"Unknown"}),
            )
            .expect_err("reject unknown speaker")
            .contains("every speaker"));
        drop(store);

        let reopened = Store::open(data, artifacts).expect("reopen diarization store");
        let persisted = reopened
            .list_transcriptions()
            .expect("list diarized transcripts")
            .into_iter()
            .find(|record| record["id"] == transcription_id)
            .expect("persisted diarization");
        assert_eq!(persisted["diarization"]["labels"]["speaker-1"], "Host");
        assert_eq!(persisted["diarization"]["labels"]["speaker-2"], "Guest");
        assert_eq!(persisted["diarization"]["label_revision_count"], 1);
        assert_eq!(
            persisted["diarization"]["turns"].as_array().map(Vec::len),
            Some(2)
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn forced_alignment_is_revision_linked_and_preserves_source_timing() {
        let (store, root) = test_store();
        let source = root.join("alignment.wav");
        fs::write(&source, b"RIFF\x10\x00\x00\x00WAVEalignment").expect("write source");
        let imported = store
            .import_transcription_source(source.to_str().expect("source path"))
            .expect("import source");
        let transcription_job = store.create_job("transcription", &json!({})).expect("job");
        store.start_job(&transcription_job).expect("start job");
        let transcript = store.complete_transcription(
            &transcription_job,
            imported.to_str().expect("import path"),
            imported.to_str().expect("original path"),
            &json!({"algorithm":"none"}),
            &json!({
                "model_id":"openai/whisper-tiny", "engine":"transformers",
                "text":"Hello world", "segments":[{"text":"Hello world","start_seconds":0.2,"end_seconds":1.4}],
                "words":[], "audio_duration_seconds":1.5, "inference_seconds":0.1,
                "rtf":0.06, "vram_peak_mb":400
            }),
        ).expect("transcript");
        let id = transcript["id"].as_str().expect("transcript id");
        let request = store
            .transcription_alignment_request(id)
            .expect("alignment request");
        let alignment_job = store
            .create_job("forced-alignment", &json!({}))
            .expect("alignment job");
        store.start_job(&alignment_job).expect("start alignment");
        let result = json!({
            "model_id":"facebook/wav2vec2-base-960h", "engine":"alignment",
            "source_revision":request["source_revision"], "source_text_sha256":request["source_text_sha256"],
            "words":[
                {"text":"Hello","start_seconds":0.25,"end_seconds":0.7,"alignment_score":0.91,"segment_index":0},
                {"text":"world","start_seconds":0.75,"end_seconds":1.3,"alignment_score":0.86,"segment_index":0}
            ],
            "mean_alignment_score":0.885, "inference_seconds":0.2, "vram_peak_mb":512,
            "evidence":{"source_revision_linked":true,"score_calibrated":false,"original_timestamps_preserved":true,"provisional":true}
        });
        let stored = store
            .complete_transcription_alignment(&alignment_job, id, &result)
            .expect("store alignment");
        assert_eq!(stored["current"], true);
        assert_eq!(stored["words"][1]["text"], "world");

        store
            .update_transcription(
                id,
                "Hello there",
                &json!([{"text":"Hello there","start_seconds":0.2,"end_seconds":1.4}]),
            )
            .expect("save correction");
        let stale = store
            .list_transcriptions()
            .expect("list")
            .into_iter()
            .find(|record| record["id"] == id)
            .expect("persisted transcript");
        assert_eq!(stale["alignment"]["current"], false);
        let second_job = store
            .create_job("forced-alignment", &json!({}))
            .expect("second job");
        store.start_job(&second_job).expect("start second");
        assert!(store
            .complete_transcription_alignment(&second_job, id, &result)
            .expect_err("reject stale revision")
            .contains("changed while alignment was running"));

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn job_state_changes_append_a_durable_event_trail() {
        let (store, root) = test_store();
        let id = store
            .create_job("synthesis", &json!({"text": "Event trail"}))
            .expect("create job");
        store
            .update_job(&id, "preparing", 0.1)
            .expect("prepare job");
        store.update_job(&id, "running", 0.4).expect("run job");
        store.fail_job(&id, "expected failure").expect("fail job");
        let connection = store.connection.lock().expect("database");
        let events = {
            let mut statement = connection.prepare("SELECT status, progress, error FROM job_events WHERE job_id = ?1 ORDER BY rowid").expect("prepare events");
            statement
                .query_map([&id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .expect("query events")
                .collect::<Result<Vec<_>, _>>()
                .expect("read events")
        };
        assert_eq!(
            events
                .iter()
                .map(|event| event.0.as_str())
                .collect::<Vec<_>>(),
            ["queued", "preparing", "running", "failed"]
        );
        assert_eq!(events[2].1, 0.4);
        assert_eq!(events[3].2.as_deref(), Some("expected failure"));
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn progressive_preview_is_scoped_to_its_active_job_and_removed_on_completion() {
        let (store, root) = test_store();
        let id = store
            .create_job("synthesis", &json!({"text": "Progressive preview"}))
            .expect("create preview job");
        store.start_job(&id).expect("start preview job");
        let preview = root.join("artifacts").join(format!(".preview-{id}.wav"));
        fs::write(&preview, b"RIFF\x10\x00\x00\x00WAVEprogressive").expect("write preview");
        store
            .update_job_preview(
                &id,
                preview.to_str().expect("preview path"),
                0.5,
                0.25,
                0.82,
            )
            .expect("record preview");
        let jobs = store.list_jobs().expect("list preview job");
        assert_eq!(jobs[0]["preview_duration_seconds"], 0.5);
        assert_eq!(jobs[0]["first_audio_seconds"], 0.25);
        assert!(store
            .job_preview_audio(&id)
            .expect("read preview")
            .starts_with(b"RIFF"));

        store.complete_job(&id).expect("complete preview job");
        assert!(!preview.exists());
        assert!(store.job_preview_audio(&id).is_err());
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn generic_job_completion_and_failure_never_revive_cancellation() {
        let (store, root) = test_store();
        let id = store
            .create_job("model-load", &json!({"model_id": "test/model"}))
            .expect("create model load");
        store.update_job(&id, "preparing", 0.05).expect("prepare");
        assert!(store.cancel_job(&id).expect("cancel"));
        assert!(!store.complete_job(&id).expect("reject late completion"));
        store
            .fail_job(&id, "late worker exit")
            .expect("ignore late failure");
        let job = store.get_job(&id).expect("get job").expect("job");
        assert_eq!(job["status"], "cancelled");
        assert!(job["error"].is_null());
        let events = store
            .job_events_since(&id, 0)
            .expect("events")
            .expect("job events");
        assert_eq!(
            events
                .iter()
                .map(|event| event["status"].as_str().unwrap_or(""))
                .collect::<Vec<_>>(),
            ["queued", "preparing", "cancelled"]
        );
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn video_job_resume_is_atomic_scoped_and_preserves_the_durable_request() {
        let (store, root) = test_store();
        let request = json!({
            "project_id": "project-1",
            "expected_revision": 3,
            "profile": "preview",
            "title": "Render preview",
        });
        let id = store
            .create_job("video_timeline_render", &request)
            .expect("create video job");
        store.start_job(&id).expect("start video job");
        store
            .fail_job(&id, "application restarted")
            .expect("interrupt video job");

        assert!(store
            .resume_video_job(&id, &["video_import_local"])
            .expect_err("reject wrong owner")
            .contains("owning workflow"));
        let (resumed, stored_request) = store
            .resume_video_job(&id, &["video_timeline_render"])
            .expect("resume video job");
        assert_eq!(resumed["status"], "preparing");
        assert_eq!(resumed["attempt"], 2);
        assert_eq!(stored_request, request);
        assert!(store
            .resume_video_job(&id, &["video_timeline_render"])
            .expect_err("reject active job")
            .contains("failed, interrupted, or cancelled"));

        let audio = store
            .create_job("synthesis", &json!({"text": "not video"}))
            .expect("create audio job");
        store.fail_job(&audio, "expected").expect("fail audio job");
        assert!(store
            .resume_video_job(&audio, &["video_timeline_render"])
            .expect_err("reject audio job")
            .contains("owning workflow"));

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn starting_a_job_is_atomic_and_never_revives_cancellation() {
        let (store, root) = test_store();
        let id = store
            .create_job("synthesis", &json!({"text": "Cancellation boundary"}))
            .expect("create job");
        assert_eq!(store.start_job(&id).expect("start job"), "running");
        assert!(store.cancel_job(&id).expect("cancel running job"));
        assert_eq!(store.start_job(&id).expect("decline restart"), "cancelled");

        let connection = store.connection.lock().expect("database");
        let running_events: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM job_events WHERE job_id = ?1 AND status = 'running'",
                [&id],
                |row| row.get(0),
            )
            .expect("count running events");
        assert_eq!(running_events, 1);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn idempotent_api_jobs_survive_retries_and_reject_key_reuse() {
        let (store, root) = test_store();
        let request = json!({"model_id": "test/model", "text": "One request"});
        let (first, created) = store
            .create_idempotent_job("api-synthesis", "client-request-1", &request)
            .expect("create idempotent job")
            .expect("new key");
        assert!(created);
        let (second, created) = store
            .create_idempotent_job("api-synthesis", "client-request-1", &request)
            .expect("repeat idempotent job")
            .expect("same request");
        assert_eq!(second, first);
        assert!(!created);
        assert!(store
            .create_idempotent_job(
                "api-synthesis",
                "client-request-1",
                &json!({"model_id": "test/model", "text": "Different request"}),
            )
            .expect("detect conflicting key")
            .is_none());
        assert_eq!(store.list_jobs().expect("one durable job").len(), 1);
        drop(store);

        let reopened = Store::open(root.join("data"), root.join("artifacts"))
            .expect("reopen idempotency store");
        let (replayed, created) = reopened
            .create_idempotent_job("api-synthesis", "client-request-1", &request)
            .expect("replay after restart")
            .expect("same request after restart");
        assert_eq!(replayed, first);
        assert!(!created);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_removes_partial_artifacts_without_touching_finished_audio() {
        let root = std::env::temp_dir().join(format!("soundar-partial-{}", Uuid::new_v4()));
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create artifact fixture");
        let partial = artifacts.join("interrupted.wav.partial");
        let finished = artifacts.join("finished.wav");
        let preview = artifacts.join(".preview-interrupted.wav");
        fs::write(&partial, b"incomplete").expect("write partial");
        fs::write(&finished, b"complete").expect("write finished");
        fs::write(&preview, b"preview").expect("write preview");

        let store = Store::open(root.join("data"), artifacts).expect("open store");
        assert!(!partial.exists());
        assert!(!preview.exists());
        assert_eq!(fs::read(&finished).expect("read finished"), b"complete");

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn staged_generation_is_atomically_published_and_registered() {
        let (store, root) = test_store();
        let final_path = root.join("artifacts/atomic.wav");
        let staging_path = root.join("artifacts/atomic.wav.partial");
        let audio = b"RIFF\x04\x00\x00\x00WAVEatomic";
        fs::write(&staging_path, audio).expect("write staged audio");
        let request = json!({"text": "Atomic publication", "speaker": "af_heart"});
        let job = store.create_job("synthesis", &request).expect("create job");
        store.update_job(&job, "running", 0.5).expect("run job");

        let history = store
            .complete_synthesis(
                &job,
                &request,
                &json!({
                    "id": "atomic-history", "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                    "audio_path": final_path, "staging_path": staging_path,
                    "sample_rate": 24000, "duration_seconds": 1.0, "inference_seconds": 0.1,
                    "rtf": 0.1, "vram_peak_mb": 100, "waveform": [0.5]
                }),
            )
            .expect("publish staged generation");

        assert_eq!(history["artifact_state"], "verified");
        assert!(!staging_path.exists());
        assert_eq!(fs::read(&final_path).expect("read final audio"), audio);
        assert_eq!(
            store
                .generated_audio_bytes(final_path.to_str().expect("final path"))
                .expect("verified playback"),
            audio
        );
        let connection = store.connection.lock().expect("database");
        let pending: i64 = connection
            .query_row("SELECT COUNT(*) FROM artifact_publications", [], |row| {
                row.get(0)
            })
            .expect("publication count");
        assert_eq!(pending, 0);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn music_generation_is_durable_distinct_from_speech_and_retryable() {
        let (store, root) = test_store();
        let final_path = root.join("artifacts/music-generation.wav");
        let staging_path = root.join("artifacts/music-generation.wav.partial");
        fs::write(&staging_path, b"RIFF\x04\x00\x00\x00WAVEmusic").expect("write staged music");
        let request = json!({
            "operation": "generate_music",
            "generation_kind": "music",
            "model_id": "ACE-Step/acestep-v15-xl-turbo-diffusers",
            "prompt": "A warm indie-pop track with sparse piano and close-mic lead vocal",
            "lyrics": "[Verse]\nHold the light until the morning comes",
            "vocal_language": "en",
            "duration_seconds": 20,
            "output_format": "wav",
        });
        let job = store
            .create_job("music-generation", &request)
            .expect("create music job");
        store.start_job(&job).expect("start music job");

        let history = store
            .complete_synthesis(
                &job,
                &request,
                &json!({
                    "id": "music-history", "generation_kind": "music",
                    "model_id": "ACE-Step/acestep-v15-xl-turbo-diffusers", "engine": "acestep",
                    "audio_path": final_path, "staging_path": staging_path,
                    "sample_rate": 48000, "duration_seconds": 20.0,
                    "inference_seconds": 4.0, "rtf": 0.2, "vram_peak_mb": 10240,
                    "waveform": [0.2, 0.5],
                }),
            )
            .expect("complete music generation");
        assert_eq!(history["generation_kind"], "music");
        assert_eq!(history["voice"], "Not applicable");
        assert_eq!(history["text"], request["prompt"]);

        let persisted = store
            .get_history("music-history")
            .expect("read music history")
            .expect("music history exists");
        assert_eq!(persisted["generation_kind"], "music");
        let persisted_request = store
            .history_request("music-history")
            .expect("read durable lyric music request");
        assert_eq!(persisted_request["prompt"], request["prompt"]);
        assert_eq!(persisted_request["lyrics"], request["lyrics"]);
        assert_eq!(persisted_request["vocal_language"], "en");
        let duplicate = store
            .duplicate_history("music-history")
            .expect("duplicate music history");
        assert_eq!(duplicate["generation_kind"], "music");

        let failed = store
            .create_job("music-generation", &request)
            .expect("create failed music job");
        store
            .fail_job(&failed, "worker stopped")
            .expect("fail music job");
        let (retried, stored_request) = store
            .retry_synthesis_job(&failed)
            .expect("retry music generation");
        assert_eq!(retried["status"], "preparing");
        assert_eq!(stored_request, request);

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn startup_rolls_back_interrupted_artifact_publication() {
        let (store, root) = test_store();
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let staging = artifacts.join("interrupted.wav.partial");
        let final_path = artifacts.join("interrupted.wav");
        fs::write(&final_path, b"RIFF\x04\x00\x00\x00WAVEinterrupted")
            .expect("write renamed artifact");
        let job = store
            .create_job("synthesis", &json!({"text": "Interrupted"}))
            .expect("create job");
        store.update_job(&job, "running", 0.5).expect("run job");
        let connection = store.connection.lock().expect("database");
        connection.execute(
            "INSERT INTO artifact_publications (id, job_id, staging_path, final_path, created_at) VALUES ('publication', ?1, ?2, ?3, 'now')",
            params![job, staging.to_string_lossy(), final_path.to_string_lossy()],
        ).expect("record interrupted publication");
        drop(connection);
        drop(store);

        let reopened = Store::open(data, artifacts).expect("recover store");
        assert!(!final_path.exists());
        let jobs = reopened.list_jobs().expect("list recovered jobs");
        assert_eq!(jobs[0]["status"], "failed");
        assert!(jobs[0]["error"]
            .as_str()
            .expect("job error")
            .contains("publication was interrupted"));
        let connection = reopened.connection.lock().expect("database");
        let pending: i64 = connection
            .query_row("SELECT COUNT(*) FROM artifact_publications", [], |row| {
                row.get(0)
            })
            .expect("publication count");
        assert_eq!(pending, 0);
        drop(connection);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn failed_synthesis_retry_increments_attempt_and_finished_clear_is_non_destructive() {
        let (store, root) = test_store();
        let request = json!({
            "model_id": "hexgrad/Kokoro-82M",
            "text": "Retry this generation",
            "speaker": "af_heart",
            "priority": "high",
        });
        let failed = store
            .create_job("synthesis", &request)
            .expect("create failed job");
        store.fail_job(&failed, "worker stopped").expect("fail job");
        let (retried, stored_request) =
            store.retry_synthesis_job(&failed).expect("retry synthesis");
        assert_eq!(retried["status"], "preparing");
        assert_eq!(retried["attempt"], 2);
        assert_eq!(retried["priority"], "high");
        assert_eq!(stored_request, request);
        store.cancel_job(&failed).expect("cancel retry");

        let completed = store
            .create_job("synthesis", &request)
            .expect("create completed job");
        store
            .update_job(&completed, "completed", 1.0)
            .expect("complete job");
        let terminal_failure = store
            .create_job("synthesis", &request)
            .expect("create terminal failure");
        store
            .fail_job(&terminal_failure, "not enough GPU memory")
            .expect("fail terminal job");
        assert_eq!(store.clear_finished_jobs().expect("clear finished"), 3);
        assert!(store.list_jobs().expect("visible jobs").is_empty());
        let connection = store.connection.lock().expect("database");
        let retained: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE id IN (?1, ?2, ?3) AND dismissed = 1",
                (&failed, &completed, &terminal_failure),
                |row| row.get(0),
            )
            .expect("count retained jobs");
        assert_eq!(retained, 3);
        drop(connection);

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn batch_jobs_cannot_be_retried_outside_their_batch() {
        let (store, root) = test_store();
        let id = store
            .create_job("batch-synthesis", &json!({"text": "row"}))
            .expect("create batch job");
        store.fail_job(&id, "row failed").expect("fail batch job");
        let error = store
            .retry_synthesis_job(&id)
            .expect_err("batch retry should be rejected");
        assert!(error.contains("owning workflow"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migration_backup_includes_committed_wal_rows() {
        let root = std::env::temp_dir().join(format!("soundar-backup-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create backup fixture");
        let database = root.join("soundar.sqlite3");
        let connection = rusqlite::Connection::open(&database).expect("open database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE evidence (value TEXT NOT NULL);
                 INSERT INTO evidence (value) VALUES ('committed-in-wal');",
            )
            .expect("write WAL fixture");
        assert!(database.with_extension("sqlite3-wal").is_file());

        let backup = create_migration_backup(&connection, &database).expect("create backup");
        let backup_connection = rusqlite::Connection::open(&backup).expect("open backup");
        let value: String = backup_connection
            .query_row("SELECT value FROM evidence", [], |row| row.get(0))
            .expect("read backed-up WAL row");
        assert_eq!(value, "committed-in-wal");
        assert!(!fs::read_dir(&root)
            .expect("list backup fixture")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")));

        drop(backup_connection);
        drop(connection);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn corrupt_database_is_preserved_and_reports_recovery_paths() {
        let root = std::env::temp_dir().join(format!("soundar-corrupt-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create corrupt fixture");
        let database = data.join("soundar.sqlite3");
        let corrupt = b"this is not a sqlite database";
        fs::write(&database, corrupt).expect("write corrupt fixture");

        let error = match Store::open(data.clone(), artifacts) {
            Ok(_) => panic!("corrupt database was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("integrity check"));
        assert!(error.contains("No data was changed"));
        assert!(error.contains(&database.to_string_lossy().to_string()));
        assert!(error.contains(&data.to_string_lossy().to_string()));
        assert_eq!(
            fs::read(&database).expect("read preserved fixture"),
            corrupt
        );
        assert!(!fs::read_dir(&data)
            .expect("list corrupt fixture")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains("backup")));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn voice_audio_reader_rejects_files_outside_managed_storage() {
        let (store, root) = test_store();
        let outside = root.join("outside.wav");
        fs::write(&outside, b"RIFF1234WAVE").expect("write outside fixture");
        assert!(store
            .voice_audio_bytes(outside.to_str().expect("fixture path"))
            .is_err());
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn fresh_capture_is_playable_before_transcription_and_external_paths_are_rejected() {
        let (store, root) = test_store();
        let capture = store.capture_path().expect("managed capture path");
        fs::write(&capture, b"RIFF1234WAVEcapture").expect("write fresh capture");
        assert_eq!(
            store
                .transcription_audio_bytes(capture.to_str().expect("capture path"))
                .expect("read untranscribed managed capture"),
            b"RIFF1234WAVEcapture"
        );

        let outside = root.join("outside-capture.wav");
        fs::write(&outside, b"RIFF1234WAVEoutside").expect("write outside capture");
        assert!(store
            .transcription_audio_bytes(outside.to_str().expect("outside path"))
            .expect_err("reject external playback")
            .contains("outside managed storage"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn consented_voice_import_copies_original_and_records_evidence() {
        let (store, root) = test_store();
        let source = root.join("source.wav");
        fs::write(&source, b"RIFF1234WAVEsource-audio").expect("write source");
        let voice = store
            .create_voice(&json!({
                "name": "Test speaker",
                "style": "Narration",
                "source_path": source,
                "consent_confirmed": true,
                "consent_basis": "Self-recorded",
                "speaker_relationship": "self",
                "permitted_uses": "Local testing",
                "source_date": "2026-08-12"
            }))
            .expect("create voice");
        let managed = PathBuf::from(voice["local_path"].as_str().expect("managed path"));
        assert_ne!(managed, source);
        assert_eq!(
            fs::read(managed).expect("read managed"),
            b"RIFF1234WAVEsource-audio"
        );
        let listed = store.list_voices().expect("list voices");
        let imported = listed
            .iter()
            .find(|item| item["id"] == voice["id"])
            .expect("imported voice");
        assert_eq!(imported["references"].as_array().map(Vec::len), Some(1));
        let connection = store.connection.lock().expect("database");
        let consent_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM consent_records WHERE voice_id = ?1",
                [voice["id"].as_str().expect("voice id")],
                |row| row.get(0),
            )
            .expect("consent count");
        assert_eq!(consent_count, 1);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn voice_reference_edits_preserve_original_and_append_revisions() {
        let (store, root) = test_store();
        let source = root.join("voice-source.wav");
        fs::write(&source, b"RIFF1234WAVEimmutable-source").expect("write source");
        let voice = store.create_voice(&json!({
            "name": "Editable speaker", "style": "Narration", "source_path": source,
            "consent_confirmed": true, "consent_basis": "Self-recorded",
            "speaker_relationship": "self", "permitted_uses": "Local testing", "source_date": "2026-08-12"
        })).expect("create voice");
        let voice_id = voice["id"].as_str().expect("voice id");
        let reference_id = voice["active_reference_id"].as_str().expect("reference id");
        let original_path = PathBuf::from(voice["local_path"].as_str().expect("original path"));
        let original_bytes = fs::read(&original_path).expect("read original");

        for revision in 1..=2 {
            let processed = original_path
                .parent()
                .expect("voice dir")
                .join(format!("processed-{revision}.wav"));
            fs::write(&processed, format!("RIFF1234WAVEprocessed-{revision}"))
                .expect("write processed");
            store.finalize_voice_reference(voice_id, reference_id, &json!({
                "audio_path": processed,
                "analysis": { "duration_seconds": 8.0, "sample_rate": 24000, "channels": 1, "peak_dbfs": -1.0, "silence_ratio": 0.1, "clipping_ratio": 0.0 },
                "processing": { "schema_version": 2, "selection_start_seconds": 0.25, "selection_end_seconds": 8.25 }
            })).expect("finalize revision");
        }

        let updated = store
            .update_voice_reference_transcript(
                voice_id,
                reference_id,
                "Correct reference phrase.",
                "corrected",
            )
            .expect("save transcript");
        let reference = &updated["references"][0];
        assert_eq!(reference["revision_count"], 2);
        assert_eq!(reference["transcript_text"], "Correct reference phrase.");
        assert_eq!(reference["transcript_source"], "corrected");
        assert_eq!(
            fs::read(original_path).expect("read immutable original"),
            original_bytes
        );
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn voice_evaluation_requires_matching_history_model_and_survives_restart() {
        let (store, root) = test_store();
        let source = root.join("evaluation-source.wav");
        fs::write(&source, b"RIFF1234WAVEsource").expect("write source");
        let voice = store.create_voice(&json!({
            "name": "Evaluated speaker", "style": "Narration", "source_path": source,
            "consent_confirmed": true, "consent_basis": "Self-recorded",
            "speaker_relationship": "self", "permitted_uses": "Local testing", "source_date": "2026-08-12"
        })).expect("create voice");
        let voice_id = voice["id"].as_str().expect("voice id").to_string();
        let reference_id = voice["active_reference_id"]
            .as_str()
            .expect("reference id")
            .to_string();
        let audio = root.join("artifacts/evaluation.wav");
        fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write generation");
        let processed = PathBuf::from(voice["local_path"].as_str().expect("managed original"))
            .parent()
            .expect("voice directory")
            .join("evaluation-reference.wav");
        fs::write(&processed, b"RIFF1234WAVEprocessed-reference")
            .expect("write processed reference");
        store.finalize_voice_reference(&voice_id, &reference_id, &json!({
            "audio_path": processed,
            "analysis": { "duration_seconds": 4.0, "sample_rate": 24000, "channels": 1, "peak_dbfs": -1.0, "silence_ratio": 0.0, "clipping_ratio": 0.0 },
            "processing": { "schema_version": 2, "selection_start_seconds": 0.0, "selection_end_seconds": 4.0 }
        })).expect("finalize reference");
        let active_reference_path = store
            .voice_reference_for_processing(&voice_id, &reference_id)
            .expect("reference evidence")["processed_path"]
            .as_str()
            .expect("processed path")
            .to_string();
        let request = json!({"text": "Evaluation phrase", "speaker": voice_id, "model_id": "clone/model", "reference_audio_path": active_reference_path});
        let job = store.create_job("synthesis", &request).expect("create job");
        store.update_job(&job, "running", 0.5).expect("run job");
        let history = store
            .complete_synthesis(
                &job,
                &request,
                &json!({
                    "model_id": "clone/model", "engine": "clone", "audio_path": audio,
                    "sample_rate": 24000, "duration_seconds": 1.0, "inference_seconds": 0.1,
                    "rtf": 0.1, "vram_peak_mb": 100, "waveform": [0.5]
                }),
            )
            .expect("complete generation");
        let saved = store.save_voice_evaluation(&json!({
            "voice_id": voice_id, "reference_id": reference_id, "model_id": "clone/model",
            "history_id": history["id"], "script": "Evaluation phrase", "decision": "accepted", "notes": "Clear and similar"
        })).expect("save evaluation");
        assert_eq!(saved["decision"], "accepted");
        let evaluation_id = saved["id"].as_str().expect("evaluation id");
        let evidence = store
            .voice_similarity_request(evaluation_id)
            .expect("similarity evidence");
        let similarity_job = store
            .create_job("speaker-similarity", &evidence)
            .expect("create similarity job");
        store
            .update_job(&similarity_job, "running", 0.5)
            .expect("run similarity job");
        let measured = store
            .complete_voice_similarity(
                &similarity_job,
                evaluation_id,
                &evidence,
                &json!({
                    "model_id": "microsoft/wavlm-base-plus-sv",
                    "engine": "speaker-verification",
                    "similarity": 0.8125,
                    "inference_seconds": 0.08,
                    "vram_peak_mb": 420.0,
                    "scoring_version": "cosine-normalized-xvector-v1"
                }),
            )
            .expect("complete similarity");
        assert_eq!(measured["speaker_similarity"], 0.8125);
        assert_eq!(measured["reference_sha256"], evidence["reference_sha256"]);
        assert_eq!(measured["candidate_sha256"], evidence["candidate_sha256"]);
        assert!(measured["similarity_measured_at"].is_string());

        let candidate_path = PathBuf::from(
            evidence["candidate_audio_path"]
                .as_str()
                .expect("candidate path"),
        );
        let candidate_bytes = fs::read(&candidate_path).expect("read candidate");
        fs::write(&candidate_path, b"tampered").expect("tamper candidate");
        assert!(store.voice_similarity_request(evaluation_id).is_err());
        fs::write(&candidate_path, candidate_bytes).expect("restore candidate");
        let wrong_request = json!({"text": "Wrong reference", "speaker": voice_id, "model_id": "clone/model", "reference_audio_path": root.join("wrong-reference.wav")});
        let wrong_job = store
            .create_job("synthesis", &wrong_request)
            .expect("create wrong job");
        store
            .update_job(&wrong_job, "running", 0.5)
            .expect("run wrong job");
        let wrong_audio = root.join("artifacts/wrong-evaluation.wav");
        fs::write(&wrong_audio, b"RIFF\x04\x00\x00\x00WAVEwrong").expect("write wrong generation");
        let wrong_history = store
            .complete_synthesis(
                &wrong_job,
                &wrong_request,
                &json!({
                    "model_id": "clone/model", "engine": "clone", "audio_path": wrong_audio,
                    "sample_rate": 24000, "duration_seconds": 1.0, "inference_seconds": 0.1,
                    "rtf": 0.1, "vram_peak_mb": 100, "waveform": [0.5]
                }),
            )
            .expect("complete wrong generation");
        let unrelated = store.save_voice_evaluation(&json!({
            "voice_id": voice_id, "reference_id": reference_id, "model_id": "clone/model",
            "history_id": wrong_history["id"], "script": "Evaluation phrase", "decision": "pending",
            "notes": "", "id": "mismatched-evidence"
        }));
        assert!(unrelated.is_err());
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        drop(store);
        let reopened = Store::open(data, artifacts).expect("reopen store");
        let listed = reopened.list_voices().expect("list voices");
        let evaluated = listed
            .iter()
            .find(|item| item["id"] == voice_id)
            .expect("evaluated voice");
        assert_eq!(evaluated["evaluations"][0]["decision"], "accepted");
        assert_eq!(evaluated["evaluations"][0]["speaker_similarity"], 0.8125);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn jobs_survive_and_failed_jobs_are_queryable() {
        let (store, root) = test_store();
        let id = store
            .create_job("synthesis", &json!({"text": "hello"}))
            .expect("create job");
        store.update_job(&id, "running", 0.5).expect("run job");
        store.fail_job(&id, "test failure").expect("fail job");
        let jobs = store.list_jobs().expect("list jobs");
        assert_eq!(jobs[0]["status"], "failed");
        assert_eq!(jobs[0]["error"], "test failure");
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn project_edit_invalidates_only_the_changed_clip() {
        let (store, root) = test_store();
        let project = store
            .save_project(&json!({
                "id": "book",
                "name": "Book",
                "document": {
                    "script": "One\n\nTwo",
                    "chapters": [
                        {"id": "one", "title": "One", "text": "First text"},
                        {"id": "two", "title": "Two", "text": "Second text"}
                    ],
                    "speaker_assignments": {}
                }
            }))
            .expect("save project");
        for history_id in ["render-one", "render-two"] {
            let audio = root.join("artifacts").join(format!("{history_id}.wav"));
            fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write clip audio");
            let request = json!({"text": history_id, "speaker": "af_heart"});
            let job = store
                .create_job("synthesis", &request)
                .expect("create clip job");
            store
                .update_job(&job, "running", 0.5)
                .expect("run clip job");
            store
                .complete_synthesis(
                    &job,
                    &request,
                    &json!({
                        "id": history_id, "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                        "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.0,
                        "inference_seconds": 0.1, "rtf": 0.1, "vram_peak_mb": 100,
                        "waveform": [0.5]
                    }),
                )
                .expect("complete clip generation");
        }
        let connection = store.connection.lock().expect("database");
        connection
            .execute(
                "UPDATE project_clips SET history_id = 'render-one', status = 'rendered' WHERE id = 'one'",
                [],
            )
            .expect("render first clip");
        connection
            .execute(
                "UPDATE project_clips SET history_id = 'render-two', status = 'rendered' WHERE id = 'two'",
                [],
            )
            .expect("render second clip");
        drop(connection);

        let mut changed = project;
        changed["document"]["chapters"][0]["text"] = json!("First text changed");
        changed["document"]["chapters"][0]["history_id"] = json!("render-one");
        changed["document"]["chapters"][1]["history_id"] = json!("render-two");
        store.save_project(&changed).expect("save edited project");

        let connection = store.connection.lock().expect("database");
        let first: (String, Option<String>) = connection
            .query_row(
                "SELECT status, history_id FROM project_clips WHERE id = 'one'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("first clip");
        let second: (String, Option<String>) = connection
            .query_row(
                "SELECT status, history_id FROM project_clips WHERE id = 'two'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("second clip");
        assert_eq!(first, ("stale".to_string(), None));
        assert_eq!(
            second,
            ("rendered".to_string(), Some("render-two".to_string()))
        );
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn project_listing_recovers_an_unregistered_master_as_a_playable_artifact() {
        let (store, root) = test_store();
        let master_path = root.join("artifacts/local-ai-master.wav");
        fs::write(&master_path, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write master audio");
        store
            .save_project(&json!({
                "id": "local-ai",
                "name": "Local AI, Close to Home",
                "document": {
                    "script": "A complete two-voice dialogue.",
                    "chapters": [],
                    "speaker_assignments": {},
                    "master": {
                        "audio_path": master_path,
                        "title": "Local AI, Close to Home · Full Dialogue",
                        "duration_seconds": 113.88,
                        "sample_rate": 24000
                    }
                }
            }))
            .expect("save project with raw master");

        let projects = store.list_projects().expect("recover project master");
        let master = &projects[0]["document"]["master"];
        assert!(master["history_id"].as_str().is_some());
        assert!(master["manifest_path"].as_str().is_some());
        let history = store.list_history(None).expect("list registered master");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["model_id"], "soundar/project-master");
        assert_eq!(history[0]["audio_path"], master["audio_path"]);
        assert_eq!(
            store
                .generated_audio_bytes(history[0]["audio_path"].as_str().expect("master path"))
                .expect("playable master"),
            b"RIFF\x04\x00\x00\x00WAVEtest"
        );
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn artifact_deletion_cannot_escape_managed_root() {
        let (store, root) = test_store();
        let connection = store.connection.lock().expect("lock database");
        connection.execute(
            "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, created_at, updated_at) VALUES ('job', 'synthesis', 'completed', '{}', 1, 1, 'now', 'now')",
            [],
        ).expect("insert job");
        connection.execute(
            "INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at) VALUES ('artifact', 'job', '/etc/hosts', 'wav', 1, 'x', 'now')",
            [],
        ).expect("insert artifact");
        connection.execute(
            "INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at) VALUES ('history', 'job', 'artifact', 'x', 'x', 'x', 'x', 'x', '/etc/hosts', 1, 1, 1, 1, 1, '[]', 'now')",
            [],
        ).expect("insert history");
        drop(connection);
        assert!(store.delete_history("history", true).is_err());
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schema_one_is_backed_up_and_migrated_to_current_schema() {
        let root = std::env::temp_dir().join(format!("soundar-migration-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create data");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open fixture");
        super::migrate(&mut connection, 0).expect("create current schema");
        connection
            .pragma_update(None, "user_version", 1)
            .expect("downgrade fixture marker");
        connection
            .execute("DROP INDEX jobs_visible_created_idx", [])
            .expect("remove v12 jobs index");
        connection
            .execute("DROP INDEX jobs_priority_created_idx", [])
            .expect("remove v19 jobs index");
        connection
            .execute("DROP TABLE artifact_publications", [])
            .expect("remove v13 artifact publications");
        connection
            .execute("DROP TABLE job_events", [])
            .expect("remove v14 job events");
        connection
            .execute("ALTER TABLE jobs DROP COLUMN dismissed", [])
            .expect("remove v12 jobs column");
        connection
            .execute("ALTER TABLE jobs DROP COLUMN priority", [])
            .expect("remove v19 jobs column");
        connection
            .execute("DROP TABLE comparisons", [])
            .expect("remove v4 comparisons");
        connection
            .execute("DROP TABLE comparison_takes", [])
            .expect("remove v20 comparison takes");
        connection
            .execute("DROP TABLE comparison_runs", [])
            .expect("remove v20 comparison runs");
        connection
            .execute("DROP TABLE history_exports", [])
            .expect("remove v22 history exports");
        connection
            .execute_batch("DROP INDEX benchmark_runs_history_idx; DROP INDEX benchmark_runs_transcription_idx; ALTER TABLE benchmark_runs DROP COLUMN transcription_id; ALTER TABLE benchmark_runs DROP COLUMN history_id;")
            .expect("remove v23 benchmark evidence columns");
        connection
            .execute_batch("ALTER TABLE history DROP COLUMN runtime_overhead_seconds; ALTER TABLE history DROP COLUMN end_to_end_seconds; ALTER TABLE history DROP COLUMN runtime_worker_state;")
            .expect("remove v24 runtime timing columns");
        connection
            .execute_batch("ALTER TABLE voice_evaluations DROP COLUMN similarity_measured_at; ALTER TABLE voice_evaluations DROP COLUMN candidate_sha256; ALTER TABLE voice_evaluations DROP COLUMN reference_sha256; ALTER TABLE voice_evaluations DROP COLUMN similarity_vram_mb; ALTER TABLE voice_evaluations DROP COLUMN similarity_inference_seconds; ALTER TABLE voice_evaluations DROP COLUMN similarity_scoring_version; ALTER TABLE voice_evaluations DROP COLUMN similarity_engine; ALTER TABLE voice_evaluations DROP COLUMN similarity_model_id; ALTER TABLE voice_evaluations DROP COLUMN speaker_similarity;")
            .expect("remove v25 speaker-similarity columns");
        connection
            .execute("DROP TABLE batch_items", [])
            .expect("remove v4 batch items");
        connection
            .execute("DROP TABLE batch_runs", [])
            .expect("remove v2 table");
        connection
            .execute("DROP TABLE transcription_revisions", [])
            .expect("remove v28 table");
        connection
            .execute("DROP TABLE transcriptions", [])
            .expect("remove v3 table");
        connection
            .execute("DROP TABLE voice_evaluations", [])
            .expect("remove v9 voice evaluations");
        connection
            .execute("DROP TABLE voice_reference_revisions", [])
            .expect("remove v9 voice reference revisions");
        connection
            .execute("DROP TABLE engine_events", [])
            .expect("remove v11 engine events");
        connection
            .execute("DROP TABLE consent_records", [])
            .expect("remove v5 consent records");
        connection
            .execute("DROP TABLE voice_references", [])
            .expect("remove v5 voice references");
        connection
            .execute("DROP TABLE project_revisions", [])
            .expect("remove v6 project revisions");
        connection
            .execute("DROP TABLE project_clips", [])
            .expect("remove v6 project clips");
        connection
            .execute("DROP TABLE project_exports", [])
            .expect("remove v7 project exports");
        drop(connection);

        let store = Store::open(data.clone(), artifacts).expect("migrate schema one");
        let connection = store.connection.lock().expect("lock migrated database");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read version");
        let batch_table: String = connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'batch_runs'",
                [],
                |row| row.get(0),
            )
            .expect("batch table");
        assert_eq!(version, super::SCHEMA_VERSION);
        assert_eq!(batch_table, "batch_runs");
        drop(connection);
        drop(store);
        assert!(fs::read_dir(&data).expect("list backups").any(|entry| entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .contains("backup")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schema_seventeen_adds_batch_attempts_without_losing_rows() {
        let root = std::env::temp_dir().join(format!("soundar-schema17-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create schema 17 fixture");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open schema 17 fixture");
        super::migrate(&mut connection, 0).expect("create current fixture");
        connection
            .execute("DROP INDEX jobs_priority_created_idx", [])
            .expect("remove schema 19 jobs index");
        connection
            .execute("DROP INDEX batch_items_priority_idx", [])
            .expect("remove schema 19 batch index");
        connection
            .execute("ALTER TABLE jobs DROP COLUMN priority", [])
            .expect("remove schema 19 jobs column");
        connection
            .execute("ALTER TABLE batch_runs DROP COLUMN priority", [])
            .expect("remove schema 19 batch column");
        connection
            .execute("ALTER TABLE batch_items DROP COLUMN priority", [])
            .expect("remove schema 19 item column");
        connection
            .execute("ALTER TABLE batch_items DROP COLUMN attempt", [])
            .expect("remove schema 18 column");
        connection
            .execute("DROP TABLE comparison_takes", [])
            .expect("remove schema 20 comparison takes");
        connection
            .execute("DROP TABLE comparison_runs", [])
            .expect("remove schema 20 comparison runs");
        connection
            .execute("DROP TABLE history_exports", [])
            .expect("remove schema 22 history exports");
        connection
            .execute_batch("DROP INDEX benchmark_runs_history_idx; DROP INDEX benchmark_runs_transcription_idx; ALTER TABLE benchmark_runs DROP COLUMN transcription_id; ALTER TABLE benchmark_runs DROP COLUMN history_id;")
            .expect("remove schema 23 benchmark evidence columns");
        connection
            .execute_batch("ALTER TABLE history DROP COLUMN runtime_overhead_seconds; ALTER TABLE history DROP COLUMN end_to_end_seconds; ALTER TABLE history DROP COLUMN runtime_worker_state;")
            .expect("remove schema 24 runtime timing columns");
        connection
            .execute_batch("ALTER TABLE voice_evaluations DROP COLUMN similarity_measured_at; ALTER TABLE voice_evaluations DROP COLUMN candidate_sha256; ALTER TABLE voice_evaluations DROP COLUMN reference_sha256; ALTER TABLE voice_evaluations DROP COLUMN similarity_vram_mb; ALTER TABLE voice_evaluations DROP COLUMN similarity_inference_seconds; ALTER TABLE voice_evaluations DROP COLUMN similarity_scoring_version; ALTER TABLE voice_evaluations DROP COLUMN similarity_engine; ALTER TABLE voice_evaluations DROP COLUMN similarity_model_id; ALTER TABLE voice_evaluations DROP COLUMN speaker_similarity;")
            .expect("remove schema 25 speaker-similarity columns");
        connection
            .execute("DROP TABLE transcription_revisions", [])
            .expect("remove schema 28 transcript revisions");
        connection
            .pragma_update(None, "user_version", 17)
            .expect("mark schema 17");
        drop(connection);

        let store = Store::open(data, artifacts).expect("migrate schema 17");
        let connection = store.lock().expect("lock migrated fixture");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema version");
        assert_eq!(version, super::SCHEMA_VERSION);
        let attempt_column: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('batch_items') WHERE name = 'attempt'",
                [],
                |row| row.get(0),
            )
            .expect("inspect attempt column");
        assert_eq!(attempt_column, 1);
        let priority_columns: i64 = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM pragma_table_info('jobs') WHERE name = 'priority') + (SELECT COUNT(*) FROM pragma_table_info('batch_runs') WHERE name = 'priority') + (SELECT COUNT(*) FROM pragma_table_info('batch_items') WHERE name = 'priority')",
                [],
                |row| row.get(0),
            )
            .expect("inspect priority columns");
        assert_eq!(priority_columns, 3);
        let comparison_tables: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('comparison_runs','comparison_takes')",
                [],
                |row| row.get(0),
            )
            .expect("inspect comparison tables");
        assert_eq!(comparison_tables, 2);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schema_twenty_preserves_legacy_tie_verdicts() {
        let root = std::env::temp_dir().join(format!("soundar-schema20-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create schema 20 fixture");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open schema 20 fixture");
        super::migrate(&mut connection, 0).expect("create current fixture");
        connection
            .execute_batch(
                "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at)
                 VALUES ('left-job', 'synthesis', 'completed', '{}', 1, 1, 1, 'now', 'now'),
                        ('right-job', 'synthesis', 'completed', '{}', 1, 1, 1, 'now', 'now');
                 INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at)
                 VALUES ('left-artifact', 'left-job', '/tmp/left.wav', 'wav', 12, 'left', 'now'),
                        ('right-artifact', 'right-job', '/tmp/right.wav', 'wav', 12, 'right', 'now');
                 INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at)
                 VALUES ('left', 'left-job', 'left-artifact', 'Left', 'Voice', 'Same line', 'model', 'test', '/tmp/left.wav', 24000, 1, .1, .1, 1, '[]', 'now'),
                        ('right', 'right-job', 'right-artifact', 'Right', 'Voice', 'Same line', 'model', 'test', '/tmp/right.wav', 24000, 1, .1, .1, 1, '[]', 'now');",
            )
            .expect("insert legacy histories");
        connection
            .execute(
                "INSERT INTO comparisons (id, left_history_id, right_history_id, script, winner, notes, created_at, updated_at)
                 VALUES ('legacy-tie', 'left', 'right', 'Same line', 'tie', '', 'now', 'now')",
                [],
            )
            .expect("insert legacy tie");
        connection
            .execute(
                "INSERT INTO comparison_runs (id, script, status, blind, revealed, notes, created_at, updated_at)
                 VALUES ('legacy-tie', 'Same line', 'completed', 0, 1, '', 'now', 'now')",
                [],
            )
            .expect("insert schema 20 comparison run");
        connection
            .execute("ALTER TABLE comparison_runs DROP COLUMN tie", [])
            .expect("remove schema 21 tie column");
        connection
            .execute("DROP TABLE history_exports", [])
            .expect("remove schema 22 history exports");
        connection
            .execute_batch("DROP INDEX benchmark_runs_history_idx; DROP INDEX benchmark_runs_transcription_idx; ALTER TABLE benchmark_runs DROP COLUMN transcription_id; ALTER TABLE benchmark_runs DROP COLUMN history_id;")
            .expect("remove schema 23 benchmark evidence columns");
        connection
            .execute_batch("ALTER TABLE history DROP COLUMN runtime_overhead_seconds; ALTER TABLE history DROP COLUMN end_to_end_seconds; ALTER TABLE history DROP COLUMN runtime_worker_state;")
            .expect("remove schema 24 runtime timing columns");
        connection
            .execute_batch("ALTER TABLE voice_evaluations DROP COLUMN similarity_measured_at; ALTER TABLE voice_evaluations DROP COLUMN candidate_sha256; ALTER TABLE voice_evaluations DROP COLUMN reference_sha256; ALTER TABLE voice_evaluations DROP COLUMN similarity_vram_mb; ALTER TABLE voice_evaluations DROP COLUMN similarity_inference_seconds; ALTER TABLE voice_evaluations DROP COLUMN similarity_scoring_version; ALTER TABLE voice_evaluations DROP COLUMN similarity_engine; ALTER TABLE voice_evaluations DROP COLUMN similarity_model_id; ALTER TABLE voice_evaluations DROP COLUMN speaker_similarity;")
            .expect("remove schema 25 speaker-similarity columns");
        connection
            .execute("DROP TABLE transcription_revisions", [])
            .expect("remove schema 28 transcript revisions");
        connection
            .pragma_update(None, "user_version", 20)
            .expect("mark schema 20");
        drop(connection);

        let store = Store::open(data, artifacts).expect("migrate schema 20");
        let comparison = store
            .get_comparison("legacy-tie")
            .expect("read migrated comparison")
            .expect("migrated comparison");
        assert_eq!(comparison["tie"], true);
        assert!(comparison["winner_take_id"].is_null());
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schema_twenty_six_adds_truthful_transcription_evidence_defaults() {
        let root = std::env::temp_dir().join(format!("soundar-schema26-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create schema 26 fixture");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open schema 26 fixture");
        super::migrate(&mut connection, 0).expect("create current fixture");
        connection
            .execute("DROP TABLE transcription_revisions", [])
            .expect("remove schema 28 transcript revisions");
        connection
            .execute_batch(
                "ALTER TABLE transcriptions DROP COLUMN evidence_json;
                 ALTER TABLE transcriptions DROP COLUMN language_confidence;
                 ALTER TABLE transcriptions DROP COLUMN detected_language;
                 ALTER TABLE transcriptions DROP COLUMN words_json;",
            )
            .expect("remove schema 27 transcription columns");
        connection
            .pragma_update(None, "user_version", 26)
            .expect("mark schema 26");
        drop(connection);

        let store = Store::open(data, artifacts).expect("migrate schema 26");
        let connection = store.lock().expect("lock migrated fixture");
        let columns: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('transcriptions') WHERE name IN ('words_json','detected_language','language_confidence','evidence_json')",
                [],
                |row| row.get(0),
            )
            .expect("inspect evidence columns");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema version");
        assert_eq!(columns, 4);
        assert_eq!(version, super::SCHEMA_VERSION);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn current_schema_repairs_missing_transcription_tables() {
        let root = std::env::temp_dir().join(format!("soundar-schema-repair-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create repair fixture");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open repair fixture");
        super::migrate(&mut connection, 0).expect("create current fixture");
        connection
            .execute_batch(
                "DROP TABLE transcription_speaker_label_revisions;
                 DROP TABLE transcription_diarizations;
                 DROP TABLE transcription_revisions;",
            )
            .expect("remove transcription tables");
        drop(connection);

        let store = Store::open(data.clone(), artifacts).expect("repair current schema");
        let connection = store.lock().expect("lock repaired fixture");
        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN (
                    'transcription_revisions',
                    'transcription_diarizations',
                    'transcription_speaker_label_revisions',
                    'transcription_alignments'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("inspect repaired tables");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read repaired schema version");
        assert_eq!(table_count, 4);
        assert_eq!(version, super::SCHEMA_VERSION);
        drop(connection);
        drop(store);
        assert!(fs::read_dir(&data)
            .expect("list repair backups")
            .any(|entry| entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains("backup")));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn completed_generation_survives_store_restart() {
        let root = std::env::temp_dir().join(format!("soundar-history-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&artifacts).expect("create artifacts");
        let audio = artifacts.join("generated.wav");
        fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write wav");
        let request = json!({
            "text": "Persistent test generation.",
            "speaker": "af_heart",
            "voice_name": "Heart"
        });
        let store = Store::open(data.clone(), artifacts.clone()).expect("open store");
        let job = store.create_job("synthesis", &request).expect("create job");
        store.update_job(&job, "running", 0.5).expect("run job");
        let result = json!({
            "id": "persistent-result",
            "model_id": "hexgrad/Kokoro-82M",
            "engine": "kokoro",
            "audio_path": audio,
            "sample_rate": 24000,
            "duration_seconds": 1.25,
            "inference_seconds": 0.1,
            "rtf": 0.08,
            "vram_peak_mb": 700.4,
            "waveform": [0.2, 0.8],
            "created_at": "2026-08-12T18:00:00Z"
        });
        store
            .complete_synthesis(&job, &request, &result)
            .expect("complete synthesis");
        drop(store);

        let reopened = Store::open(data, artifacts).expect("reopen store");
        let history = reopened
            .list_history(Some("persistent"))
            .expect("search history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["id"], "persistent-result");
        assert_eq!(history[0]["voice"], "Heart");
        assert_eq!(history[0]["vram_peak_mb"], 700);
        assert_eq!(
            reopened.list_jobs().expect("jobs")[0]["status"],
            "completed"
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn history_filters_duplicate_and_export_preserve_artifact_integrity() {
        let (store, root) = test_store();
        let audio = root.join("artifacts/history-workbench.wav");
        fs::write(&audio, b"RIFF\x10\x00\x00\x00WAVEhistory-workbench")
            .expect("write history audio");
        let request = json!({
            "model_id": "hexgrad/Kokoro-82M", "text": "History workbench source.",
            "speaker": "af_heart", "voice_name": "Heart", "language": "en",
            "speed": 1.0, "seed": 44, "output_format": "wav"
        });
        let job = store
            .create_job("synthesis", &request)
            .expect("create source job");
        store.start_job(&job).expect("start source job");
        let source = store
            .complete_synthesis(
                &job,
                &request,
                &json!({
                    "id": "history-workbench", "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                    "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.0,
                    "inference_seconds": 0.1, "rtf": 0.1, "vram_peak_mb": 500,
                    "waveform": [0.2, 0.8]
                }),
            )
            .expect("complete source generation");
        let registered = store
            .get_registered_history_by_audio_path(
                source["audio_path"].as_str().expect("registered path"),
            )
            .expect("verify registered path")
            .expect("registered history");
        assert_eq!(registered["id"], "history-workbench");
        let unmanaged = root.join("unmanaged.wav");
        fs::write(&unmanaged, b"RIFF\x04\x00\x00\x00WAVEoutside").expect("write unmanaged audio");
        assert!(store
            .get_registered_history_by_audio_path(unmanaged.to_str().expect("unmanaged path"))
            .expect_err("reject audio outside managed artifacts")
            .contains("outside the managed artifact directory"));
        store
            .update_history_metadata("history-workbench", &json!({"favorite": true}))
            .expect("favorite source");

        let filtered = store
            .list_history_filtered(
                Some("workbench"),
                Some(&json!({
                    "model_id": "hexgrad/Kokoro-82M", "voice": "Heart", "favorite": true,
                    "artifact_state": "available"
                })),
            )
            .expect("filter history");
        assert_eq!(filtered.len(), 1);
        assert!(store
            .list_history_filtered(None, Some(&json!({"model_id": "other/model"})))
            .expect("empty model filter")
            .is_empty());

        let duplicate = store
            .duplicate_history("history-workbench")
            .expect("duplicate history");
        assert_ne!(duplicate["id"], source["id"]);
        assert_eq!(duplicate["title"], "History workbench source copy");
        assert_eq!(duplicate["favorite"], false);
        assert!(store
            .generated_audio_bytes(duplicate["audio_path"].as_str().expect("duplicate path"))
            .expect("read duplicate")
            .starts_with(b"RIFF"));
        assert_eq!(
            store
                .history_request(duplicate["id"].as_str().expect("duplicate ID"))
                .expect("duplicate request")["duplicated_from_history_id"],
            "history-workbench"
        );

        let export_dir = root.join("user-exports");
        fs::create_dir_all(&export_dir).expect("create export directory");
        let exported = export_dir.join("history-copy.wav");
        let receipt = store
            .export_history("history-workbench", exported.to_str().expect("export path"))
            .expect("export history");
        assert_eq!(
            fs::read(&exported).expect("read export"),
            store
                .generated_audio_bytes(source["audio_path"].as_str().expect("source path"))
                .expect("source bytes")
        );
        assert_eq!(receipt["format"], "wav");
        assert!(store
            .export_history("history-workbench", exported.to_str().expect("export path"))
            .expect_err("refuse overwrite")
            .contains("already exists"));
        assert!(store
            .export_history(
                "history-workbench",
                export_dir
                    .join("changed.flac")
                    .to_str()
                    .expect("wrong extension")
            )
            .expect_err("refuse extension change")
            .contains("original encoding"));
        let receipt_count: i64 = store
            .lock()
            .expect("database")
            .query_row(
                "SELECT COUNT(*) FROM history_exports WHERE history_id = 'history-workbench'",
                [],
                |row| row.get(0),
            )
            .expect("export receipts");
        assert_eq!(receipt_count, 1);

        fs::write(
            source["audio_path"].as_str().expect("source path"),
            b"tampered",
        )
        .expect("tamper source");
        assert!(store
            .get_registered_history_by_audio_path(
                source["audio_path"].as_str().expect("tampered source path")
            )
            .expect_err("reject a tampered registered artifact")
            .contains("changed on disk"));
        let unavailable = store
            .list_history_filtered(None, Some(&json!({"artifact_state": "unavailable"})))
            .expect("filter unavailable");
        assert!(unavailable
            .iter()
            .any(|item| item["id"] == "history-workbench"));
        assert!(store
            .export_history(
                "history-workbench",
                export_dir
                    .join("tampered.wav")
                    .to_str()
                    .expect("tampered export")
            )
            .expect_err("reject tampered source")
            .contains("changed on disk"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ten_thousand_history_records_search_across_corpus_with_bounded_results() {
        let (store, root) = test_store();
        let audio = root.join("artifacts/history-scale.wav");
        fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEscale").expect("write shared audio");
        let connection = store.connection.lock().expect("database");
        let transaction = connection
            .unchecked_transaction()
            .expect("history fixture transaction");
        transaction.execute(
            "INSERT INTO jobs (id, kind, status, request_json, progress, attempt, created_at, updated_at) VALUES ('scale-job', 'synthesis', 'completed', '{}', 1, 1, '2026-08-12T00:00:00Z', '2026-08-12T00:00:00Z')",
            [],
        ).expect("insert scale job");
        transaction.execute(
            "INSERT INTO artifacts (id, job_id, path, format, size_bytes, sha256, created_at) VALUES ('scale-artifact', 'scale-job', ?1, 'wav', ?2, 'fixture', '2026-08-12T00:00:00Z')",
            params![audio.to_string_lossy(), fs::metadata(&audio).expect("audio metadata").len() as i64],
        ).expect("insert scale artifact");
        {
            let mut insert = transaction.prepare(
                "INSERT INTO history (id, job_id, artifact_id, title, voice, text, model_id, engine, audio_path, sample_rate, duration_seconds, inference_seconds, rtf, vram_peak_mb, waveform_json, created_at) VALUES (?1, 'scale-job', 'scale-artifact', ?2, 'Scale voice', ?3, 'scale/model', 'test', ?4, 24000, 1, 0.1, 0.1, 0, '[]', ?5)",
            ).expect("prepare history fixture");
            for index in 0..10_000 {
                let needle = if index == 9_876 {
                    " unique-search-token"
                } else {
                    ""
                };
                insert
                    .execute(params![
                        format!("history-{index:05}"),
                        format!("Scale generation {index:05}"),
                        format!("Corpus row {index:05}{needle}"),
                        audio.to_string_lossy(),
                        format!("2026-08-12T00:{:02}:{:02}Z", (index / 60) % 60, index % 60),
                    ])
                    .expect("insert history row");
            }
        }
        transaction.commit().expect("commit history fixture");
        drop(connection);

        let started = Instant::now();
        let recent = store.list_history(None).expect("list bounded history");
        assert_eq!(recent.len(), super::HISTORY_RESULT_LIMIT as usize);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bounded history listing was too slow: {:?}",
            started.elapsed()
        );
        let matched = store
            .list_history(Some("unique-search-token"))
            .expect("search full corpus");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0]["id"], "history-09876");

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn concurrent_readers_and_writer_preserve_database_integrity() {
        let (store, root) = test_store();
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        let store = Arc::new(store);
        let writer_store = Arc::clone(&store);
        let writer = thread::spawn(move || {
            for index in 0..250 {
                let id = writer_store
                    .create_job("synthesis", &json!({"text": format!("Concurrent {index}")}))
                    .expect("create concurrent job");
                writer_store
                    .fail_job(&id, "expected fixture failure")
                    .expect("finish concurrent job");
            }
        });
        let readers = (0..4)
            .map(|_| {
                let reader_store = Arc::clone(&store);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let jobs = reader_store.list_jobs().expect("read jobs concurrently");
                        assert!(jobs.len() <= 100);
                        reader_store
                            .list_history(Some("concurrent"))
                            .expect("search history concurrently");
                    }
                })
            })
            .collect::<Vec<_>>();
        writer.join().expect("writer thread");
        for reader in readers {
            reader.join().expect("reader thread");
        }
        drop(store);

        let reopened = Store::open(data, artifacts).expect("reopen concurrent database");
        let connection = reopened.connection.lock().expect("database");
        super::verify_database_integrity(&connection)
            .expect("database integrity after concurrency");
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .expect("job count");
        assert_eq!(count, 250);
        drop(connection);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn history_reports_artifact_damage_and_playback_verifies_checksum() {
        let (store, root) = test_store();
        let audio = root.join("artifacts/integrity.wav");
        let original = b"RIFF\x04\x00\x00\x00WAVEgood";
        fs::write(&audio, original).expect("write trusted audio");
        let request = json!({"text": "Artifact integrity", "speaker": "af_heart"});
        let job = store.create_job("synthesis", &request).expect("create job");
        store.update_job(&job, "running", 0.5).expect("run job");
        store
            .complete_synthesis(
                &job,
                &request,
                &json!({
                    "id": "integrity-history", "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                    "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.0,
                    "inference_seconds": 0.1, "rtf": 0.1, "vram_peak_mb": 100, "waveform": [0.5]
                }),
            )
            .expect("publish artifact");

        let history = store.list_history(None).expect("list trusted history");
        assert_eq!(history[0]["artifact_state"], "available");
        assert_eq!(
            store
                .generated_audio_bytes(audio.to_str().expect("audio path"))
                .expect("trusted playback"),
            original
        );

        fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEevil").expect("tamper same-size audio");
        let checksum_error = store
            .generated_audio_bytes(audio.to_str().expect("audio path"))
            .expect_err("tampered checksum should fail");
        assert!(checksum_error.contains("checksum"));

        fs::write(&audio, b"RIFFshort").expect("tamper audio size");
        assert_eq!(
            store.list_history(None).expect("list modified history")[0]["artifact_state"],
            "modified"
        );
        fs::remove_file(&audio).expect("remove audio");
        let missing = store.list_history(None).expect("list missing history");
        assert_eq!(missing[0]["artifact_state"], "missing");
        assert_eq!(missing[0]["missing"], true);

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn durable_batches_track_each_item_and_recovery() {
        let (store, root) = test_store();
        let batch = store
            .create_batch(&json!({
                "name": "Chapter one",
                "scripts": ["First line", "Second line"],
                "settings": {"model_id": "hexgrad/Kokoro-82M"}
            }))
            .expect("create batch");
        let id = batch["id"].as_str().expect("batch id");
        store
            .update_batch_item(id, 0, "completed", None, None)
            .expect("complete first item");
        store
            .update_batch_item(id, 1, "failed", None, Some("test failure"))
            .expect("fail second item");
        let batches = store.list_batches().expect("list batches");
        assert_eq!(batches[0]["status"], "failed");
        assert_eq!(batches[0]["completed_items"], 1);
        assert_eq!(batches[0]["failed_items"], 1);
        assert_eq!(batches[0]["items"][1]["error"], "test failure");
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rich_batch_rows_are_validated_named_and_persisted() {
        let (store, root) = test_store();
        let batch = store
            .create_batch(&json!({
                "name": "Localized prompts",
                "rows": [
                    {"text": "Welcome to soundAr.", "name": "Welcome", "output_name": "Intro Final", "priority": "urgent", "settings": {"language": "en", "speed": 0.95, "seed": 7}},
                    {"text": "Deuxieme ligne.", "settings": {"language": "fr"}}
                ],
                "priority": "low",
                "settings": {"model_id": "hexgrad/Kokoro-82M"}
            }))
            .expect("create rich batch");
        assert_eq!(batch["items"][0]["name"], "Welcome");
        assert_eq!(batch["items"][0]["output_name"], "0001-intro-final");
        assert_eq!(batch["items"][0]["settings"]["speed"], 0.95);
        assert_eq!(batch["priority"], "low");
        assert_eq!(batch["items"][0]["priority"], "urgent");
        assert_eq!(batch["items"][1]["priority"], "low");
        assert_eq!(batch["items"][1]["output_name"], "0002-deuxieme-ligne");
        let job = store
            .create_job("batch-synthesis", &json!({"text": "Welcome"}))
            .expect("create rich row job");
        assert!(store
            .start_batch_item(batch["id"].as_str().expect("batch id"), 0, &job,)
            .expect("start rich row"));
        let started = store
            .get_batch(batch["id"].as_str().expect("batch id"))
            .expect("read started batch")
            .expect("started batch exists");
        assert_eq!(started["items"][0]["attempt"], 1);
        drop(store);

        let reopened = Store::open(root.join("data"), root.join("artifacts"))
            .expect("reopen rich batch store");
        let persisted = reopened.list_batches().expect("list rich batches");
        assert_eq!(persisted[0]["items"][0]["settings"]["seed"], 7);
        assert_eq!(persisted[0]["items"][0]["attempt"], 1);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rich_batch_rows_reject_unknown_settings_and_oversized_inputs() {
        let (store, root) = test_store();
        let source = root.join("project-reference.wav");
        fs::write(&source, b"RIFF1234WAVEproject-reference").expect("write voice source");
        let voice = store
            .create_voice(&json!({
                "name": "Project narrator",
                "source_path": source,
                "consent_confirmed": true,
                "consent_basis": "Self-recorded",
                "speaker_relationship": "self",
                "permitted_uses": "Project rendering"
            }))
            .expect("create consent-backed voice");
        let managed_reference = voice["local_path"]
            .as_str()
            .expect("managed reference")
            .to_string();
        {
            let connection = store.lock().expect("lock voice store");
            connection
                .execute(
                    "UPDATE voices SET state = 'ready' WHERE id = ?1",
                    [voice["id"].as_str().expect("voice id")],
                )
                .expect("make test voice ready");
        }
        let accepted = store.create_batch(&json!({
            "rows": [{"text": "Hello", "settings": {"reference_audio_path": managed_reference, "voice_name": "Narrator", "input_mode": "text"}}]
        })).expect("persist project-style voice settings");
        assert_eq!(accepted["items"][0]["settings"]["voice_name"], "Narrator");
        let initial_batches = store.list_batches().expect("list accepted batches").len();
        let unmanaged_row = store.create_batch(&json!({
            "rows": [{"text": "Hello", "settings": {"reference_audio_path": "/tmp/private.wav"}}]
        }));
        assert!(unmanaged_row
            .expect_err("reject unmanaged row reference")
            .contains("Reference voice audio was not found"));
        let unmanaged_default = store.create_batch(&json!({
            "scripts": ["Hello"],
            "settings": {"reference_audio_path": "/tmp/private.wav"}
        }));
        assert!(unmanaged_default
            .expect_err("reject unmanaged default reference")
            .contains("Reference voice audio was not found"));
        assert_eq!(
            store.list_batches().expect("list unchanged batches").len(),
            initial_batches
        );
        let unknown = store.create_batch(&json!({
            "rows": [{"text": "Hello", "settings": {"callback_url": "https://example.invalid"}}]
        }));
        assert!(unknown
            .expect_err("reject unsafe row override")
            .contains("unsupported setting"));
        assert!(store
            .create_batch(&json!({"rows": [{"text": "Hello", "priority": "immediate"}]}))
            .expect_err("reject invalid row priority")
            .contains("Priority must be"));
        assert!(store
            .create_job("synthesis", &json!({"text": "Hello", "priority": 8}))
            .expect_err("reject invalid job priority")
            .contains("Priority must be"));
        let rows = (0..1_001)
            .map(|index| json!({"text": format!("Row {index}")}))
            .collect::<Vec<_>>();
        assert!(store
            .create_batch(&json!({"rows": rows}))
            .expect_err("reject oversized batch")
            .contains("1,000"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rich_batch_rows_reject_unknown_row_fields() {
        let request = json!({
            "rows": [{"text": "Hello", "ouptut_name": "typo"}]
        });
        let error = super::normalize_batch_rows(&request).unwrap_err();
        assert!(error.contains("unsupported field 'ouptut_name'"));
    }

    #[test]
    fn idempotent_api_batches_do_not_duplicate_rows() {
        let (store, root) = test_store();
        let request = json!({
            "name": "Idempotent batch",
            "scripts": ["First", "Second"],
            "parallelism": 2,
            "settings": {"model_id": "hexgrad/Kokoro-82M"}
        });
        let (first, created) = store
            .create_idempotent_batch("batch-request-1", &request)
            .expect("create idempotent batch")
            .expect("new batch key");
        assert!(created);
        let (replayed, created) = store
            .create_idempotent_batch("batch-request-1", &request)
            .expect("replay idempotent batch")
            .expect("same batch request");
        assert_eq!(replayed["id"], first["id"]);
        assert!(!created);
        assert_eq!(replayed["items"].as_array().expect("batch items").len(), 2);
        assert!(store
            .create_idempotent_batch(
                "batch-request-1",
                &json!({"name": "Changed", "scripts": ["Different"]}),
            )
            .expect("detect conflicting batch key")
            .is_none());
        assert_eq!(store.list_batches().expect("one batch").len(), 1);
        drop(store);

        let reopened = Store::open(root.join("data"), root.join("artifacts"))
            .expect("reopen batch idempotency store");
        let (after_restart, created) = reopened
            .create_idempotent_batch("batch-request-1", &request)
            .expect("replay batch after restart")
            .expect("same batch after restart");
        assert_eq!(after_restart["id"], first["id"]);
        assert!(!created);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn paused_batches_resume_queued_rows_and_retry_only_failures() {
        let (store, root) = test_store();
        let batch = store
            .create_batch(&json!({
                "name": "Resumable batch",
                "scripts": ["finished", "failed", "waiting"],
                "settings": {"model_id": "hexgrad/Kokoro-82M"}
            }))
            .expect("create batch");
        let id = batch["id"].as_str().expect("batch id");
        store
            .update_batch_item(id, 0, "completed", None, None)
            .expect("complete first row");
        store
            .update_batch_item(id, 1, "failed", None, Some("engine error"))
            .expect("fail second row");

        let paused = store.pause_batch(id).expect("pause batch");
        assert_eq!(paused["status"], "paused");
        assert_eq!(paused["completed_items"], 1);
        let resumed = store.resume_batch(id, false).expect("resume queued row");
        assert_eq!(resumed["status"], "queued");
        assert_eq!(resumed["items"][0]["status"], "completed");
        assert_eq!(resumed["items"][1]["status"], "failed");
        assert_eq!(resumed["items"][2]["status"], "queued");

        store
            .update_batch_item(id, 2, "completed", None, None)
            .expect("complete queued row");
        let retried = store.resume_batch(id, true).expect("retry failed rows");
        assert_eq!(retried["items"][0]["status"], "completed");
        assert_eq!(retried["items"][1]["status"], "queued");
        assert_eq!(retried["items"][2]["status"], "completed");
        assert_eq!(retried["failed_items"], 0);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn engine_lifecycle_evidence_is_sanitized_bounded_and_restart_safe() {
        let (store, root) = test_store();
        assert!(store
            .record_engine_event("kokoro", "failed", "/home/person/private.wav")
            .is_err());
        store
            .record_engine_event("kokoro", "started", "worker_started")
            .expect("record start");
        store
            .record_engine_event("kokoro", "failed", "process_exited")
            .expect("record failure");
        store
            .record_engine_event("kokoro", "started", "worker_started")
            .expect("record restart");
        store
            .record_engine_event("kokoro", "recovered", "worker_recovered")
            .expect("record recovery");
        let summary = store
            .engine_event_summary("kokoro")
            .expect("summarize events");
        assert_eq!(summary["worker_starts"], 2);
        assert_eq!(summary["worker_restarts"], 1);
        assert_eq!(summary["worker_failures"], 1);
        assert_eq!(summary["last_error"], "process_exited");
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        drop(store);
        let reopened = Store::open(data, artifacts).expect("reopen store");
        assert_eq!(
            reopened
                .engine_event_summary("kokoro")
                .expect("persistent summary")["worker_restarts"],
            1
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn comparison_and_history_metadata_are_persistent() {
        let (store, root) = test_store();
        let artifacts = root.join("artifacts");
        for (index, id) in ["left", "right"].iter().enumerate() {
            let audio = artifacts.join(format!("{id}.wav"));
            fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write wav");
            let request = json!({"text": format!("Comparison {id}"), "speaker": "af_heart"});
            let job = store.create_job("synthesis", &request).expect("create job");
            store.update_job(&job, "running", 0.5).expect("run job");
            store
                .complete_synthesis(
                    &job,
                    &request,
                    &json!({
                        "id": id, "model_id": "hexgrad/Kokoro-82M", "engine": "kokoro",
                        "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.0,
                        "inference_seconds": 0.1 + index as f64, "rtf": 0.1,
                        "vram_peak_mb": 95.058, "waveform": [0.5]
                    }),
                )
                .expect("complete generation");
        }
        let updated = store
            .update_history_metadata("left", &json!({"favorite": true, "notes": "Warmest take"}))
            .expect("update metadata");
        assert_eq!(updated["favorite"], true);
        assert_eq!(updated["notes"], "Warmest take");
        let comparison = store
            .save_comparison(&json!({
                "left_history_id": "left", "right_history_id": "right",
                "script": "Same script", "winner": "A", "notes": "Cleaner consonants"
            }))
            .expect("save comparison");
        assert_eq!(comparison["winner"], "A");
        assert_eq!(store.list_comparisons().expect("list comparisons").len(), 1);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn comparison_runs_preserve_take_requests_reviews_and_partial_results() {
        let (store, root) = test_store();
        let run = store
            .create_comparison(&json!({
                "script": "The same phrase should reveal meaningful differences.",
                "blind": true,
                "priority": "high",
                "takes": [
                    {"model_id": "model/a", "speaker": "one", "seed": 10},
                    {"model_id": "model/b", "speaker": "two", "seed": 11},
                    {"model_id": "model/c", "speaker": "three", "seed": 12}
                ]
            }))
            .expect("create comparison run");
        let id = run["id"].as_str().expect("comparison ID");
        assert_eq!(run["takes"].as_array().expect("takes").len(), 3);
        assert_eq!(run["takes"][0]["request"]["priority"], "high");
        assert_eq!(run["takes"][2]["request"]["text"], run["script"]);

        let plan = store
            .comparison_execution_plan(id)
            .expect("comparison plan");
        let first_take = plan[0].0.clone();
        let first_job = plan[0].1.clone();
        store
            .finish_comparison_take(id, &first_take, None, Some("engine failed"))
            .expect("fail first take");
        store
            .fail_job(&first_job, "engine failed")
            .expect("fail first job");

        for (take_id, job_id, request) in plan.into_iter().skip(1) {
            let history_id = format!("history-{take_id}");
            let audio = root.join("artifacts").join(format!("{history_id}.wav"));
            fs::write(&audio, b"RIFF\x04\x00\x00\x00WAVEtest").expect("write comparison audio");
            store.start_job(&job_id).expect("start take job");
            store
                .complete_synthesis(
                    &job_id,
                    &request,
                    &json!({
                        "id": history_id, "model_id": request["model_id"], "engine": "test",
                        "audio_path": audio, "sample_rate": 24000, "duration_seconds": 1.0,
                        "inference_seconds": 0.1, "rtf": 0.1, "vram_peak_mb": 50,
                        "waveform": [0.2, 0.4]
                    }),
                )
                .expect("complete comparison take");
            store
                .finish_comparison_take(id, &take_id, Some(&history_id), None)
                .expect("link comparison take");
        }
        let partial = store
            .get_comparison(id)
            .expect("read comparison")
            .expect("comparison");
        assert_eq!(partial["status"], "partial");
        assert_eq!(partial["takes"][0]["error"], "engine failed");
        let selected = partial["takes"][1]["id"].as_str().expect("take ID");
        let reviewed = store
            .update_comparison_review(
                id,
                &json!({"take_id": selected, "rating": 5, "favorite": true, "notes": "Best pacing"}),
            )
            .expect("review take");
        assert_eq!(reviewed["takes"][1]["rating"], 5);
        let promoted = store
            .update_comparison_review(
                id,
                &json!({"revealed": true, "winner_take_id": selected, "promoted_take_id": selected}),
            )
            .expect("promote take");
        assert_eq!(promoted["winner_take_id"], selected);
        assert_eq!(promoted["promoted_take_id"], selected);
        assert_eq!(promoted["takes"][1]["result"]["favorite"], true);
        let tied = store
            .update_comparison_review(id, &json!({"tie": true}))
            .expect("mark tie");
        assert_eq!(tied["tie"], true);
        assert!(tied["winner_take_id"].is_null());
        let winner = store
            .update_comparison_review(id, &json!({"winner_take_id": selected}))
            .expect("replace tie with winner");
        assert_eq!(winner["tie"], false);
        assert_eq!(winner["winner_take_id"], selected);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn atomic_prompt_project_creation_rolls_back_every_row_on_late_submission_failure() {
        let (store, root) = test_store();
        let project_id = "atomic-prompt-failpoint";
        {
            let connection = store.connection.lock().expect("prompt failpoint database");
            connection
                .execute_batch(
                    "CREATE TRIGGER fail_prompt_submission
                     BEFORE INSERT ON api_job_submissions
                     WHEN NEW.operation = 'video_create_from_prompt'
                     BEGIN
                         SELECT RAISE(ABORT, 'prompt submission failpoint');
                     END;",
                )
                .expect("install late prompt failpoint");
        }
        let manifest = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Atomic prompt draft",
            "revision": 1,
            "timeline_duration_us": 5_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let request = json!({
            "project_id": project_id,
            "prompt": "Create a durable local video.",
            "priority": "normal"
        });
        let error = store
            .create_video_project_with_job(
                "Atomic prompt draft",
                &manifest,
                "test-suite",
                "video_create_from_prompt",
                &request,
                "video-create-prompt:atomic-prompt-failpoint",
            )
            .expect_err("force the final submission insert to fail");
        assert!(
            error.contains("prompt submission failpoint"),
            "unexpected failure: {error}"
        );
        assert!(store
            .get_video_project(project_id)
            .expect("inspect rolled-back prompt project")
            .is_none());
        let connection = store.connection.lock().expect("inspect prompt rollback");
        for (table, predicate) in [
            ("projects", "id = 'atomic-prompt-failpoint'"),
            ("video_projects", "project_id = 'atomic-prompt-failpoint'"),
            (
                "video_project_versions",
                "project_id = 'atomic-prompt-failpoint'",
            ),
            (
                "video_project_events",
                "project_id = 'atomic-prompt-failpoint'",
            ),
            ("jobs", "kind = 'video_create_from_prompt'"),
            (
                "api_job_submissions",
                "operation = 'video_create_from_prompt'",
            ),
        ] {
            let count: i64 = connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE {predicate}"),
                    [],
                    |row| row.get(0),
                )
                .expect("count rolled-back prompt rows");
            assert_eq!(count, 0, "{table} retained a partial prompt workflow");
        }
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn video_projects_require_leases_and_optimistic_revisions() {
        let (store, root) = test_store();
        let manifest = json!({
            "schema_version": 1,
            "project_id": "video-project-one",
            "name": "Interview reel",
            "revision": 1,
            "timeline_duration_us": 5_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let created = store
            .create_video_project("Interview reel", &manifest, "test-suite")
            .expect("create video project");
        assert_eq!(created["revision"], 1);
        assert_eq!(created["project_kind"], "video");
        assert!(store
            .list_projects()
            .expect("list audio projects")
            .is_empty());
        assert!(store
            .save_project(&json!({
                "id": "video-project-one",
                "name": "Collision",
                "document": {"chapters": []},
            }))
            .expect_err("protect video project from the audio workflow")
            .contains("timeline service"));

        let lease = store
            .acquire_video_project_lock("video-project-one", "editor-one", 60)
            .expect("acquire project lease");
        let renewed = store
            .acquire_video_project_lock("video-project-one", "editor-one", 60)
            .expect("renew own lease");
        assert_eq!(lease["token"], renewed["token"]);
        assert!(store
            .acquire_video_project_lock("video-project-one", "editor-two", 60)
            .expect_err("reject competing editor")
            .starts_with("video.project_locked:"));

        let mut revision = manifest.clone();
        revision["revision"] = json!(2);
        revision["timeline_duration_us"] = json!(4_500_000);
        let saved = store
            .commit_video_manifest(
                "video-project-one",
                1,
                &revision,
                "editor-one",
                "Shorten the opening",
                lease["token"].as_str().expect("lease token"),
                Some("review"),
            )
            .expect("save locked revision");
        assert_eq!(saved["revision"], 2);
        assert_eq!(saved["status"], "review");
        assert!(store
            .commit_video_manifest(
                "video-project-one",
                1,
                &revision,
                "editor-one",
                "stale write",
                lease["token"].as_str().expect("lease token"),
                None,
            )
            .expect_err("reject stale revision")
            .starts_with("video.revision_conflict:"));
        assert!(store
            .release_video_project_lock(
                "video-project-one",
                lease["token"].as_str().expect("lease token"),
            )
            .expect("release lease"));
        drop(store);

        let reopened =
            Store::open(root.join("data"), root.join("artifacts")).expect("reopen video store");
        let persisted = reopened
            .get_video_project("video-project-one")
            .expect("load project")
            .expect("persisted project");
        assert_eq!(persisted["revision"], 2);
        assert_eq!(persisted["manifest"]["timeline_duration_us"], 4_500_000);
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn video_rights_outputs_cache_and_recovery_remain_project_scoped() {
        let (store, root) = test_store();
        let project_id = "video-evidence-one";
        let manifest = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Authorized source",
            "revision": 1,
            "timeline_duration_us": 2_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let project = store
            .create_video_project("Authorized source", &manifest, "test-suite")
            .expect("create project");
        let canonical_url = "https://www.youtube.com/watch?v=abcdefghijk";
        store
            .record_video_rights_receipt(
                Some(project_id),
                canonical_url,
                "I own or am authorized to use this exact source.",
                "local-user",
            )
            .expect("record rights receipt");
        let receipt = store
            .record_video_rights_receipt(
                Some(project_id),
                canonical_url,
                "I own or am authorized to use this exact source.",
                "local-user",
            )
            .expect("reuse rights receipt idempotently");
        assert!(receipt["confirmed_at"]
            .as_str()
            .expect("rights timestamp")
            .ends_with('Z'));
        assert!(store
            .has_video_rights_receipt(Some(project_id), canonical_url)
            .expect("match exact URL"));
        assert!(!store
            .has_video_rights_receipt(
                Some(project_id),
                "https://www.youtube.com/watch?v=other-source",
            )
            .expect("reject another URL"));

        let render_dir = root.join("artifacts/video/video-evidence-one/renders");
        fs::create_dir_all(&render_dir).expect("create render directory");
        let master = render_dir.join("master.mp4");
        fs::write(&master, b"validated-video-output").expect("write output fixture");
        let source = render_dir.join("source.mp4");
        fs::write(&source, b"managed-source").expect("write source fixture");
        store
            .upsert_video_asset(&json!({
                "id": "asset-one",
                "project_id": project_id,
                "kind": "source",
                "source_kind": "local",
                "local_path": source,
                "mime_type": "video/mp4",
                "status": "ready",
            }))
            .expect("register managed source");
        let published = store
            .publish_video_output(&json!({
                "project_id": project_id,
                "version_id": project["version"]["id"],
                "kind": "master",
                "label": "Final master",
                "artifact_path": master,
                "mime_type": "video/mp4",
                "duration_us": 2_000_000,
                "width": 1080,
                "height": 1920,
                "is_primary": true,
                "provenance": {"renderer": "ffmpeg"}
            }))
            .expect("publish master");
        assert_eq!(published["is_primary"], true);
        let assistant_link = store
            .link_assistant_video_artifact(&json!({
                "thread_id": "thread-one",
                "turn_id": "turn-one",
                "item_id": "call-one",
                "project_id": project_id,
                "output_id": published["id"],
                "relationship": "master",
            }))
            .expect("link master to assistant");
        let duplicate_link = store
            .link_assistant_video_artifact(&json!({
                "thread_id": "thread-one",
                "turn_id": "turn-one",
                "item_id": "call-one",
                "project_id": project_id,
                "output_id": published["id"],
                "relationship": "master",
            }))
            .expect("link master idempotently");
        assert_eq!(assistant_link["id"], duplicate_link["id"]);

        let other_project_id = "video-evidence-two";
        let other_manifest = json!({
            "schema_version": 1,
            "project_id": other_project_id,
            "name": "Other project",
            "revision": 1,
            "timeline_duration_us": 2_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let other_project = store
            .create_video_project("Other project", &other_manifest, "test-suite")
            .expect("create ownership boundary fixture");
        assert!(store
            .upsert_video_asset(&json!({
                "id": "asset-one",
                "project_id": other_project_id,
                "kind": "source",
                "source_kind": "local",
                "local_path": source,
                "status": "ready",
            }))
            .expect_err("prevent media asset reassignment")
            .starts_with("video.ownership_mismatch:"));
        assert!(store
            .publish_video_output(&json!({
                "project_id": project_id,
                "version_id": other_project["version"]["id"],
                "kind": "master",
                "label": "Foreign version",
                "artifact_path": master,
                "mime_type": "video/mp4",
                "is_primary": true,
            }))
            .expect_err("reject an output for another project's version")
            .starts_with("video.ownership_mismatch:"));
        let foreign_stage_error = store
            .upsert_video_stage(&json!({
                "project_id": project_id,
                "version_id": other_project["version"]["id"],
                "stage_key": "preview",
                "status": "queued",
                "input_sha256": "c".repeat(64),
            }))
            .expect_err("reject a stage for another project's version");
        assert!(
            foreign_stage_error.starts_with("video.ownership_mismatch:"),
            "unexpected stage error: {foreign_stage_error}"
        );
        assert!(store
            .link_assistant_video_artifact(&json!({
                "thread_id": "thread-two",
                "turn_id": "turn-two",
                "item_id": "call-two",
                "project_id": other_project_id,
                "output_id": published["id"],
                "relationship": "master",
            }))
            .expect_err("reject a cross-project assistant output")
            .starts_with("video.ownership_mismatch:"));

        let lease = store
            .acquire_video_project_lock(project_id, "test-suite", 60)
            .expect("lock project for a new version");
        let mut revised_manifest = manifest.clone();
        revised_manifest["revision"] = json!(2);
        let revised = store
            .commit_video_manifest(
                project_id,
                1,
                &revised_manifest,
                "test-suite",
                "Version ownership test",
                lease["token"].as_str().expect("lease token"),
                None,
            )
            .expect("commit second version");
        assert_eq!(revised["revision"], 2);
        assert!(store
            .publish_video_output(&json!({
                "project_id": project_id,
                "version_id": project["version"]["id"],
                "kind": "master",
                "label": "Stale master",
                "artifact_path": master,
                "mime_type": "video/mp4",
                "is_primary": true,
            }))
            .expect_err("reject stale primary promotion")
            .starts_with("video.stale_output:"));
        let cached = store
            .put_video_cache(
                &"a".repeat(64),
                "final-render",
                Some(project_id),
                &json!({"revision": 1}),
                &master,
            )
            .expect("cache master");
        assert_eq!(cached["hit_count"], 0);
        assert_eq!(
            store
                .get_video_cache(&"a".repeat(64))
                .expect("read cache")
                .expect("cache hit")["hit_count"],
            1
        );
        store
            .upsert_video_stage(&json!({
                "project_id": project_id,
                "version_id": project["version"]["id"],
                "stage_key": "final-render",
                "scope_key": "master",
                "status": "running",
                "resource_class": "heavy",
                "attempt": 1,
                "progress": 0.42,
                "input_sha256": "b".repeat(64),
                "checkpoint": {"completed_scenes": ["scene-one"]}
            }))
            .expect("checkpoint running render");
        drop(store);

        let reopened =
            Store::open(root.join("data"), root.join("artifacts")).expect("recover video store");
        let stages = reopened
            .list_video_stages(project_id)
            .expect("list recovered stages");
        assert_eq!(stages[0]["status"], "interrupted");
        assert_eq!(stages[0]["checkpoint"]["completed_scenes"][0], "scene-one");
        assert_eq!(
            reopened
                .list_video_outputs(project_id)
                .expect("list outputs")[0]["artifact_path"],
            master.to_string_lossy().as_ref()
        );
        drop(reopened);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn schema_thirty_seven_adds_trusted_visual_source_receipts() {
        let root = std::env::temp_dir().join(format!("soundar-schema37-{}", Uuid::new_v4()));
        let data = root.join("data");
        let artifacts = root.join("artifacts");
        fs::create_dir_all(&data).expect("create schema 37 fixture");
        let database = data.join("soundar.sqlite3");
        let mut connection = rusqlite::Connection::open(&database).expect("open schema 37 fixture");
        super::migrate(&mut connection, 0).expect("create current fixture");
        connection
            .execute("DROP TABLE video_visual_source_receipts", [])
            .expect("remove schema 38 visual receipts");
        connection
            .pragma_update(None, "user_version", 37)
            .expect("mark schema 37");
        drop(connection);

        let store = Store::open(data, artifacts).expect("migrate schema 37");
        let connection = store.lock().expect("lock migrated fixture");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema version");
        let receipt_table: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'video_visual_source_receipts'",
                [],
                |row| row.get(0),
            )
            .expect("inspect visual receipt table");
        assert_eq!(version, super::SCHEMA_VERSION);
        assert_eq!(receipt_table, 1);
        drop(connection);
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn visual_source_receipts_are_version_bound_expiring_and_one_use() {
        let (store, root) = test_store();
        let project_id = "visual-receipt-project";
        let manifest = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Visual receipt",
            "revision": 1,
            "timeline_duration_us": 1_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let project = store
            .create_video_project("Visual receipt", &manifest, "test-suite")
            .expect("create visual receipt project");
        let version_id = project["version"]["id"]
            .as_str()
            .expect("current version")
            .to_string();
        let receipt = |id: &str, expires_at: &str| {
            json!({
                "id": id,
                "receipt_kind": "user_selected",
                "project_id": project_id,
                "expected_revision": 1,
                "expected_version_id": version_id,
                "source_path": "/tmp/exact-picked-image.png",
                "source_device": "7",
                "source_inode": "11",
                "size_bytes": 30,
                "modified_seconds": 1,
                "modified_nanoseconds": 2,
                "sha256": "a".repeat(64),
                "mime_type": "image/png",
                "width": 1,
                "height": 1,
                "has_alpha": true,
                "producer": "soundAr native file picker",
                "producer_version": "test",
                "generation_id": null,
                "trust_context": {"boundary": "native_file_picker"},
                "issued_at": "2026-01-01T00:00:00.000Z",
                "expires_at": expires_at,
            })
        };
        store
            .create_video_visual_source_receipt(&receipt(
                "visual-selection-live",
                "2999-01-01T00:00:00.000Z",
            ))
            .expect("create live receipt");
        store
            .create_video_visual_source_receipt(&receipt(
                "visual-selection-expired",
                "2026-01-02T00:00:00.000Z",
            ))
            .expect("create expired receipt");
        let request_one = json!({
            "project_id": project_id,
            "expected_revision": 1,
            "expected_version_id": version_id,
            "operation_id": "one",
            "origin": {"kind": "user_selected", "receipt_id": "visual-selection-live"}
        });
        let request_two = json!({
            "project_id": project_id,
            "expected_revision": 1,
            "expected_version_id": version_id,
            "operation_id": "two",
            "origin": {"kind": "user_selected", "receipt_id": "visual-selection-live"}
        });
        let request_expired = json!({
            "project_id": project_id,
            "expected_revision": 1,
            "expected_version_id": version_id,
            "operation_id": "expired",
            "origin": {"kind": "user_selected", "receipt_id": "visual-selection-expired"}
        });
        let (job_one, _) = store
            .create_idempotent_job(
                "video_add_visual_asset",
                "visual-receipt-job-one",
                &request_one,
            )
            .expect("create first receipt job")
            .expect("first receipt job identity");
        let (job_two, _) = store
            .create_idempotent_job(
                "video_add_visual_asset",
                "visual-receipt-job-two",
                &request_two,
            )
            .expect("create second receipt job")
            .expect("second receipt job identity");
        let (expired_job, _) = store
            .create_idempotent_job(
                "video_add_visual_asset",
                "visual-receipt-job-expired",
                &request_expired,
            )
            .expect("create expired receipt job")
            .expect("expired receipt job identity");
        let claimed = store
            .claim_video_visual_source_receipt(
                "visual-selection-live",
                "user_selected",
                project_id,
                1,
                &version_id,
                &job_one,
            )
            .expect("claim exact receipt");
        assert_eq!(claimed["sha256"], "a".repeat(64));
        store
            .claim_video_visual_source_receipt(
                "visual-selection-live",
                "user_selected",
                project_id,
                1,
                &version_id,
                &job_one,
            )
            .expect("same durable job may replay its receipt claim");
        store
            .connection
            .lock()
            .expect("lock receipt store")
            .execute(
                "UPDATE video_visual_source_receipts SET expires_at = '2026-01-02T00:00:00.000Z' WHERE id = 'visual-selection-live'",
                [],
            )
            .expect("expire already-claimed receipt");
        store
            .claim_video_visual_source_receipt(
                "visual-selection-live",
                "user_selected",
                project_id,
                1,
                &version_id,
                &job_one,
            )
            .expect("same durable job may replay after receipt expiry");
        assert!(store
            .claim_video_visual_source_receipt(
                "visual-selection-live",
                "user_selected",
                project_id,
                1,
                &version_id,
                &job_two,
            )
            .expect_err("another job cannot reuse the receipt")
            .contains("already used"));
        assert!(store
            .claim_video_visual_source_receipt(
                "visual-selection-expired",
                "user_selected",
                project_id,
                1,
                &version_id,
                &expired_job,
            )
            .expect_err("expired receipt fails closed")
            .contains("expired"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn video_audio_authorization_requires_an_integrity_bound_registration() {
        let (store, root) = test_store();
        let manifest = |project_id: &str| {
            json!({
                "schema_version": 1,
                "project_id": project_id,
                "name": project_id,
                "revision": 1,
                "timeline_duration_us": 1_000_000,
                "layout": {"canvas": {"width": 1080, "height": 1920}}
            })
        };
        for project_id in ["audio-owner", "audio-other"] {
            store
                .create_video_project(project_id, &manifest(project_id), "test-suite")
                .expect("create video project");
        }
        let registered_dir = root.join("artifacts/video/projects/audio-owner/sources");
        let other_dir = root.join("artifacts/video/projects/audio-other/sources");
        fs::create_dir_all(&registered_dir).expect("create registered project directory");
        fs::create_dir_all(&other_dir).expect("create other project directory");
        let registered = registered_dir.join("narration.wav");
        let unregistered = other_dir.join("unregistered.wav");
        fs::write(&registered, b"RIFF\x10\x00\x00\x00WAVEregistered-audio")
            .expect("write registered audio");
        fs::write(&unregistered, b"RIFF\x10\x00\x00\x00WAVEunregistered-audio")
            .expect("write unregistered audio");
        let size = fs::metadata(&registered)
            .expect("registered metadata")
            .len();
        let checksum = sha256_file(&registered).expect("registered checksum");
        store
            .upsert_video_asset(&json!({
                "id": "registered-audio",
                "project_id": "audio-owner",
                "kind": "source",
                "source_kind": "local",
                "local_path": registered,
                "mime_type": "audio/wav",
                "content_sha256": checksum,
                "size_bytes": size,
                "duration_us": 1_000_000,
                "status": "ready",
            }))
            .expect("register project audio");

        let authorized = store
            .get_registered_video_audio_by_path(registered.to_str().expect("registered path text"))
            .expect("authorize registered project audio")
            .expect("registered record");
        assert_eq!(authorized["project_id"], "audio-owner");
        assert!(store
            .get_registered_video_audio_by_path(
                unregistered.to_str().expect("unregistered path text"),
            )
            .expect("inspect unregistered cross-project audio")
            .is_none());

        fs::write(&registered, b"RIFF\x10\x00\x00\x00WAVEchanged-audio")
            .expect("tamper registered audio");
        assert!(store
            .get_registered_video_audio_by_path(registered.to_str().expect("registered path text"),)
            .expect_err("reject changed registered audio")
            .contains("changed on disk"));
        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn atomic_video_output_publication_enforces_live_lock_current_cas_and_batch_idempotency() {
        let (store, root) = test_store();
        let project_id = "atomic-output-project";
        let manifest = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Atomic output project",
            "revision": 1,
            "timeline_duration_us": 1_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        let project = store
            .create_video_project("Atomic output project", &manifest, "test-suite")
            .expect("create video project");
        let version_one = project["version"]["id"]
            .as_str()
            .expect("initial version")
            .to_string();
        let lease = store
            .acquire_video_project_lock(project_id, "atomic-output-test", 60)
            .expect("acquire output publication lease");
        let token = lease["token"].as_str().expect("lease token").to_string();
        let render_dir = root.join("artifacts/video/projects/atomic-output-project/renders");
        fs::create_dir_all(&render_dir).expect("create render fixture directory");
        let master_one = render_dir.join("master-v1.mp4");
        let variation_one = render_dir.join("variation-v1.mp4");
        fs::write(&master_one, b"atomic-master-version-one").expect("write first master");
        fs::write(&variation_one, b"atomic-variation-version-one").expect("write first variation");
        let first_batch = vec![
            json!({
                "id": "master-v1",
                "project_id": project_id,
                "version_id": version_one,
                "kind": "master",
                "label": "Master v1",
                "artifact_path": master_one,
                "mime_type": "video/mp4",
                "is_primary": true,
            }),
            json!({
                "id": "variation-v1",
                "project_id": project_id,
                "version_id": version_one,
                "kind": "variation",
                "label": "Variation v1",
                "artifact_path": variation_one,
                "mime_type": "video/mp4",
                "is_primary": false,
            }),
        ];
        let published = store
            .publish_video_outputs_current(&first_batch, 1, &version_one, &token)
            .expect("publish first atomic batch");
        assert_eq!(published.len(), 2);
        assert_eq!(published[0]["is_primary"], true);

        let replayed = store
            .publish_video_outputs_current(&first_batch, 1, &version_one, &token)
            .expect("adopt identical batch");
        assert_eq!(replayed[0]["id"], published[0]["id"]);
        assert_eq!(replayed[1]["id"], published[1]["id"]);
        assert_eq!(store.list_video_outputs(project_id).unwrap().len(), 2);

        // Adoption is semantic-id based, never content-only. If the old path disappeared, a
        // freshly validated replacement with the same stable id repairs the stored playable path.
        fs::remove_file(&variation_one).expect("remove adopted variation path");
        let repaired_variation = render_dir.join("variation-v1-repaired.mp4");
        fs::write(&repaired_variation, b"atomic-variation-version-one")
            .expect("write repaired variation");
        let mut repaired_batch = first_batch.clone();
        repaired_batch[1]["artifact_path"] = json!(repaired_variation);
        let repaired = store
            .publish_video_outputs_current(&repaired_batch, 1, &version_one, &token)
            .expect("repair adopted output path");
        assert_eq!(repaired[1]["id"], "variation-v1");
        assert_eq!(
            repaired[1]["artifact_path"],
            repaired_batch[1]["artifact_path"]
        );
        assert_eq!(store.list_video_outputs(project_id).unwrap().len(), 2);

        let mut revised_manifest = manifest.clone();
        revised_manifest["revision"] = json!(2);
        let revised = store
            .commit_video_manifest(
                project_id,
                1,
                &revised_manifest,
                "atomic-output-test",
                "Advance before stale output",
                &token,
                Some("ready"),
            )
            .expect("advance current version");
        let version_two = revised["version"]["id"]
            .as_str()
            .expect("second version")
            .to_string();
        let stale = json!({
            "id": "stale-variation",
            "project_id": project_id,
            "version_id": version_one,
            "kind": "variation",
            "label": "Stale variation",
            "artifact_path": repaired_variation,
            "mime_type": "video/mp4",
        });
        let stale_error = store
            .publish_video_output_current(&stale, 1, &version_one, &token)
            .expect_err("reject stale non-primary output");
        assert!(stale_error.starts_with("video.revision_conflict:"));
        assert_eq!(store.list_video_outputs(project_id).unwrap().len(), 2);

        let master_two = render_dir.join("master-v2.mp4");
        let preview_two = render_dir.join("preview-v2.mp4");
        fs::write(&master_two, b"atomic-master-version-two").expect("write second master");
        fs::write(&preview_two, b"atomic-preview-version-two").expect("write second preview");
        let invalid_batch = vec![
            json!({
                "id": "preview-v2",
                "project_id": project_id,
                "version_id": version_two,
                "kind": "preview",
                "label": "Preview v2",
                "artifact_path": preview_two,
            }),
            json!({
                "id": "master-v2-invalid",
                "project_id": project_id,
                "version_id": version_two,
                "kind": "master",
                "label": "Master v2",
                "artifact_path": master_two,
                "sha256": "0".repeat(64),
                "is_primary": true,
            }),
        ];
        assert!(store
            .publish_video_outputs_current(&invalid_batch, 2, &version_two, &token)
            .expect_err("reject invalid member before batch publication")
            .starts_with("video.integrity_failed:"));
        assert_eq!(store.list_video_outputs(project_id).unwrap().len(), 2);

        let two_primaries = vec![
            json!({
                "id": "master-v2-a", "project_id": project_id, "version_id": version_two,
                "kind": "master", "label": "Master A", "artifact_path": master_two,
                "is_primary": true,
            }),
            json!({
                "id": "master-v2-b", "project_id": project_id, "version_id": version_two,
                "kind": "master", "label": "Master B", "artifact_path": preview_two,
                "is_primary": true,
            }),
        ];
        assert!(store
            .publish_video_outputs_current(&two_primaries, 2, &version_two, &token)
            .expect_err("reject two requested primary masters")
            .starts_with("video.invalid_output:"));
        assert_eq!(
            store
                .list_video_outputs(project_id)
                .unwrap()
                .iter()
                .filter(|output| output["is_primary"] == true)
                .count(),
            1
        );

        let second_master = store
            .publish_video_output_current(
                &json!({
                    "id": "master-v2",
                    "project_id": project_id,
                    "version_id": version_two,
                    "kind": "master",
                    "label": "Master v2",
                    "artifact_path": master_two,
                    "is_primary": true,
                }),
                2,
                &version_two,
                &token,
            )
            .expect("publish new current master");
        assert_eq!(second_master["is_primary"], true);
        let all_outputs = store.list_video_outputs(project_id).unwrap();
        assert_eq!(
            all_outputs
                .iter()
                .filter(|output| output["is_primary"] == true)
                .count(),
            1
        );
        assert_eq!(
            all_outputs
                .iter()
                .find(|output| output["id"] == "master-v1")
                .expect("old master retained")["is_primary"],
            false
        );

        store
            .lock()
            .expect("database lock")
            .execute(
                "UPDATE video_project_locks SET lease_expires_at = '1970-01-01T00:00:00Z' WHERE token = ?1",
                [&token],
            )
            .expect("expire lease");
        assert!(store
            .publish_video_output_current(
                &json!({
                    "id": "preview-after-expiry",
                    "project_id": project_id,
                    "version_id": version_two,
                    "kind": "preview",
                    "label": "Expired preview",
                    "artifact_path": preview_two,
                }),
                2,
                &version_two,
                &token,
            )
            .expect_err("reject expired lease after hashing")
            .starts_with("video.lock_lost:"));

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manifest_and_variation_outputs_commit_or_rollback_as_one_current_version() {
        let (store, root) = test_store();
        let project_id = "atomic-render-commit";
        let manifest_one = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Atomic render",
            "revision": 1,
            "timeline_duration_us": 1_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        store
            .create_video_project("Atomic render", &manifest_one, "test-suite")
            .expect("create atomic render project");
        let lease = store
            .acquire_video_project_lock(project_id, "atomic-render-test", 60)
            .expect("acquire render lease");
        let token = lease["token"].as_str().expect("lease token");
        let render_dir = root.join("artifacts/video/projects/atomic-render-commit/renders");
        fs::create_dir_all(&render_dir).expect("create render directory");
        let master = render_dir.join("master.mp4");
        let variation = render_dir.join("variation.mp4");
        fs::write(&master, b"atomic-render-master").expect("write master");
        fs::write(&variation, b"atomic-render-variation").expect("write variation");
        let mut manifest_two = manifest_one.clone();
        manifest_two["revision"] = json!(2);
        let outputs = vec![
            json!({
                "id": "atomic-master",
                "project_id": project_id,
                "kind": "master",
                "label": "Canonical master",
                "artifact_path": master,
                "is_primary": true,
            }),
            json!({
                "id": "atomic-variation",
                "project_id": project_id,
                "version_id": null,
                "kind": "variation",
                "label": "Variation 2",
                "artifact_path": variation,
                "is_primary": false,
            }),
            json!({
                "id": "atomic-variation-alt",
                "project_id": project_id,
                "version_id": null,
                "kind": "variation",
                "label": "Variation 3 with identical encoded bytes",
                "artifact_path": variation,
                "is_primary": false,
                "provenance": {"variation": 3},
            }),
        ];
        let committed = store
            .commit_video_manifest_with_outputs(
                project_id,
                1,
                &manifest_two,
                "atomic-render-test",
                "Render two variations",
                token,
                Some("completed"),
                &outputs,
            )
            .expect("commit manifest and outputs atomically");
        assert_eq!(committed["revision"], 2);
        let current_version = committed["version"]["id"].as_str().expect("result version");
        let committed_outputs = committed["outputs"].as_array().expect("outputs");
        assert_eq!(committed_outputs.len(), 3);
        assert_eq!(
            committed_outputs
                .iter()
                .filter(|output| output["kind"] == "variation")
                .count(),
            2
        );
        assert!(committed_outputs
            .iter()
            .all(|output| output["version_id"] == current_version));
        assert_eq!(
            committed_outputs
                .iter()
                .filter(|output| output["is_primary"] == true)
                .count(),
            1
        );

        let rows_before = committed_outputs.len();
        let stale_error = store
            .commit_video_manifest_with_outputs(
                project_id,
                1,
                &manifest_two,
                "atomic-render-test",
                "Stale replay",
                token,
                Some("completed"),
                &outputs,
            )
            .expect_err("reject replay against consumed revision");
        assert!(stale_error.starts_with("video.revision_conflict:"));
        let after_stale = store
            .get_video_project(project_id)
            .unwrap()
            .expect("project after stale replay");
        assert_eq!(after_stale["revision"], 2);
        assert_eq!(
            after_stale["outputs"].as_array().unwrap().len(),
            rows_before
        );

        let mut manifest_three = manifest_two.clone();
        manifest_three["revision"] = json!(3);
        let invalid_outputs = vec![
            json!({
                "id": "primary-three-a", "project_id": project_id, "kind": "master",
                "label": "Primary A", "artifact_path": master, "is_primary": true,
            }),
            json!({
                "id": "primary-three-b", "project_id": project_id, "kind": "master",
                "label": "Primary B", "artifact_path": variation, "is_primary": true,
            }),
        ];
        assert!(store
            .commit_video_manifest_with_outputs(
                project_id,
                2,
                &manifest_three,
                "atomic-render-test",
                "Invalid primary batch",
                token,
                Some("completed"),
                &invalid_outputs,
            )
            .expect_err("rollback invalid output batch")
            .starts_with("video.invalid_output:"));
        let after_invalid = store
            .get_video_project(project_id)
            .unwrap()
            .expect("project after invalid batch");
        assert_eq!(after_invalid["revision"], 2);
        assert_eq!(after_invalid["version"]["id"], current_version);
        assert_eq!(
            after_invalid["outputs"].as_array().unwrap().len(),
            rows_before
        );

        // Force a failure after the transaction has advanced its in-memory version and inserted
        // the first output. Reusing an existing global id on the second member must roll back the
        // version, event, pointer, primary demotion, and first insert together.
        let late_failure_outputs = vec![
            json!({
                "id": "inserted-before-late-failure", "project_id": project_id,
                "kind": "variation", "label": "Must roll back", "artifact_path": master,
            }),
            json!({
                "id": "atomic-variation", "project_id": project_id,
                "kind": "variation", "label": "Conflicting global id", "artifact_path": variation,
            }),
        ];
        assert!(store
            .commit_video_manifest_with_outputs(
                project_id,
                2,
                &manifest_three,
                "atomic-render-test",
                "Injected late output identity failure",
                token,
                Some("completed"),
                &late_failure_outputs,
            )
            .expect_err("rollback a late output insertion failure")
            .starts_with("video.ownership_mismatch:"));
        let after_late_failure = store
            .get_video_project(project_id)
            .unwrap()
            .expect("project after late transactional failure");
        assert_eq!(after_late_failure["revision"], 2);
        assert_eq!(after_late_failure["version"]["id"], current_version);
        assert!(!after_late_failure["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|output| output["id"] == "inserted-before-late-failure"));

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn cancelling_atomic_output_hash_leaves_manifest_and_outputs_unchanged() {
        let (store, root) = test_store();
        let project_id = "cancelled-render-commit";
        let manifest_one = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Cancelled render",
            "revision": 1,
            "timeline_duration_us": 1_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        store
            .create_video_project("Cancelled render", &manifest_one, "test-suite")
            .expect("create cancelled render project");
        let lease = store
            .acquire_video_project_lock(project_id, "cancel-test", 60)
            .expect("acquire cancellation lease");
        let token = lease["token"].as_str().expect("lease token");
        let render_dir = root.join("artifacts/video/projects/cancelled-render-commit/renders");
        fs::create_dir_all(&render_dir).expect("create cancellation render directory");
        let large_render = render_dir.join("large-master.mp4");
        fs::File::create(&large_render)
            .and_then(|file| file.set_len(128 * 1024 * 1024))
            .expect("create sparse render large enough to cancel during hashing");
        let mut manifest_two = manifest_one.clone();
        manifest_two["revision"] = json!(2);
        let outputs = vec![json!({
            "id": "cancelled-stable-master",
            "project_id": project_id,
            "kind": "master",
            "label": "Cancelled master",
            "artifact_path": large_render,
            "is_primary": true,
        })];
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            signal.store(true, Ordering::Release);
        });
        let error = store
            .commit_video_manifest_with_outputs_cancellable(
                project_id,
                1,
                &manifest_two,
                "cancel-test",
                "Cancelled render commit",
                token,
                Some("completed"),
                &outputs,
                cancel.as_ref(),
            )
            .expect_err("cancel during output hashing");
        canceller.join().expect("join cancellation signal");
        assert!(error.starts_with("video.cancelled:"));
        let unchanged = store
            .get_video_project(project_id)
            .unwrap()
            .expect("reload unchanged project");
        assert_eq!(unchanged["revision"], 1);
        assert!(unchanged["outputs"].as_array().unwrap().is_empty());

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn project_job_recovery_filters_in_sql_and_survives_unrelated_noise() {
        let (store, root) = test_store();
        let project_id = "recovery-target";
        store
            .create_video_project(
                "Recovery target",
                &json!({
                    "schema_version": 1,
                    "project_id": project_id,
                    "name": "Recovery target",
                    "revision": 1,
                    "timeline_duration_us": 1_000_000,
                    "layout": {"canvas": {"width": 1080, "height": 1920}}
                }),
                "test-suite",
            )
            .expect("create recovery project");
        let target = store
            .create_job(
                "video_render_timeline_batch_preview",
                &json!({"base": {"project_id": project_id}, "variations": [1]}),
            )
            .expect("create nested recovery job");
        assert!(store.cancel_job(&target).expect("cancel recovery job"));

        // More than the old global scan limit of newer unrelated work must not shadow the exact
        // project job. Malformed JSON is ignored rather than breaking recovery for every project.
        {
            let mut connection = store.connection.lock().expect("database");
            let transaction = connection.transaction().expect("noise transaction");
            for index in 0..600 {
                transaction
                    .execute(
                        "INSERT INTO jobs
                         (id, kind, status, request_json, progress, attempt, priority, created_at, updated_at)
                         VALUES (?1, 'video_analyze', 'failed', ?2, 0.4, 1, 1,
                                 '2099-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
                        params![
                            format!("unrelated-video-job-{index:04}"),
                            if index == 599 {
                                "{malformed".to_string()
                            } else {
                                json!({"project_id": format!("other-project-{index}")})
                                    .to_string()
                            }
                        ],
                    )
                    .expect("insert unrelated job");
            }
            transaction.commit().expect("commit job noise");
        }

        let recovered = store
            .latest_video_project_job(project_id)
            .expect("query exact project recovery")
            .expect("recover target job");
        assert_eq!(recovered["id"], target);
        assert_eq!(recovered["status"], "cancelled");

        let composite_parent = store
            .create_job(
                "video_create_from_prompt",
                &json!({"project_id": project_id, "prompt": "Durable parent"}),
            )
            .expect("create composite recovery parent");
        let (internal_child, created) = store
            .create_idempotent_job(
                "video_import_local",
                "recovery-target-internal-import",
                &json!({
                    "project_id": project_id,
                    "parent_job_id": composite_parent.clone(),
                    "source_path": "/managed/history.wav",
                }),
            )
            .expect("create parent-bound recovery child")
            .expect("internal recovery identity");
        assert!(created);
        assert_eq!(
            store
                .latest_video_project_job(project_id)
                .expect("query composite recovery owner")
                .expect("recover composite parent")["id"],
            composite_parent
        );
        assert_ne!(internal_child, composite_parent);
        assert!(store
            .latest_video_project_job("other-project-without-job")
            .expect("query absent project")
            .is_none());

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn guarded_manifest_commit_checks_parent_activity_in_the_revision_transaction() {
        let (store, root) = test_store();
        let project_id = "parent-guarded-video";
        let manifest_one = json!({
            "schema_version": 1,
            "project_id": project_id,
            "name": "Parent guarded video",
            "revision": 1,
            "timeline_duration_us": 1_000_000,
            "layout": {"canvas": {"width": 1080, "height": 1920}}
        });
        store
            .create_video_project("Parent guarded video", &manifest_one, "test-suite")
            .expect("create guarded project");
        let lease = store
            .acquire_video_project_lock(project_id, "guarded-worker", 60)
            .expect("acquire guarded project lock");
        let token = lease["token"].as_str().expect("lock token");
        let active_parent = store
            .create_job(
                "video_regenerate_narration",
                &json!({"project_id": project_id, "title": "Active narration parent"}),
            )
            .expect("create active parent");
        let mut manifest_two = manifest_one.clone();
        manifest_two["revision"] = json!(2);
        let committed = store
            .commit_video_manifest_if_job_active(
                project_id,
                1,
                &manifest_two,
                "guarded-worker",
                "Commit while parent is active",
                token,
                Some("ready"),
                &active_parent,
            )
            .expect("commit under active parent");
        assert_eq!(committed["revision"], 2);

        let cancelled_parent = store
            .create_job(
                "video_regenerate_narration",
                &json!({"project_id": project_id, "title": "Cancelled narration parent"}),
            )
            .expect("create cancelled parent");
        assert!(store
            .cancel_job(&cancelled_parent)
            .expect("cancel durable parent"));
        let mut manifest_three = manifest_two.clone();
        manifest_three["revision"] = json!(3);
        let error = store
            .commit_video_manifest_if_job_active(
                project_id,
                2,
                &manifest_three,
                "guarded-worker",
                "Must not commit after cancellation",
                token,
                Some("ready"),
                &cancelled_parent,
            )
            .expect_err("reject cancelled parent atomically");
        assert!(
            error.starts_with("video.cancelled:"),
            "unexpected error: {error}"
        );
        let unchanged = store
            .get_video_project(project_id)
            .expect("reload guarded project")
            .expect("guarded project exists");
        assert_eq!(unchanged["revision"], 2);

        drop(store);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn assistant_video_link_survives_restart_and_never_restores_a_stale_file() {
        let (store, root) = test_store();
        let project_id = "assistant-restart-video";
        let project = store
            .create_video_project(
                "Saved assistant video",
                &json!({
                    "schema_version": 1,
                    "project_id": project_id,
                    "name": "Saved assistant video",
                    "revision": 1,
                    "timeline_duration_us": 1_000_000,
                    "layout": {"canvas": {"width": 1080, "height": 1920}}
                }),
                "test-suite",
            )
            .expect("create assistant video project");
        let render_dir = root.join("artifacts/video/projects/assistant-restart-video/renders");
        fs::create_dir_all(&render_dir).expect("create render directory");
        let master = render_dir.join("master.mp4");
        let master_bytes = b"assistant-video-master";
        fs::write(&master, master_bytes).expect("write assistant master");
        let output = store
            .publish_video_output(&json!({
                "project_id": project_id,
                "version_id": project["version"]["id"],
                "kind": "master",
                "label": "Final master",
                "artifact_path": master,
                "mime_type": "video/mp4",
                "is_primary": true,
            }))
            .expect("publish assistant master");
        let link = json!({
            "thread_id": "saved-thread",
            "turn_id": "saved-turn",
            "item_id": "saved-call",
            "project_id": project_id,
            "output_id": output["id"],
            "relationship": "master",
        });
        let first = store
            .link_assistant_video_artifact(&link)
            .expect("persist assistant link");
        let replay = store
            .link_assistant_video_artifact(&link)
            .expect("replay exact tool call idempotently");
        assert_eq!(first["id"], replay["id"]);
        assert!(store
            .latest_assistant_video_artifact("another-thread")
            .expect("enforce exact thread lookup")
            .is_none());
        drop(store);

        let reopened =
            Store::open(root.join("data"), root.join("artifacts")).expect("reopen store");
        let restored = reopened
            .latest_assistant_video_artifact("saved-thread")
            .expect("load saved assistant relationship")
            .expect("saved relationship");
        assert_eq!(restored["project_id"], project_id);
        assert_eq!(restored["output_id"], output["id"]);
        assert_eq!(restored["relationship"], "master");

        fs::write(&master, vec![b'x'; master_bytes.len()])
            .expect("tamper saved master without changing its size");
        let tampered_fallback = reopened
            .latest_assistant_video_artifact("saved-thread")
            .expect("recheck tampered assistant output")
            .expect("project fallback for tampered output");
        assert!(tampered_fallback["output_id"].is_null());
        assert_eq!(tampered_fallback["relationship"], "project");

        fs::write(&master, master_bytes).expect("restore saved master");
        let restored_again = reopened
            .latest_assistant_video_artifact("saved-thread")
            .expect("recheck restored assistant output")
            .expect("restored output relationship");
        assert_eq!(restored_again["output_id"], output["id"]);
        fs::remove_file(&master).expect("simulate missing saved master");
        let safe_fallback = reopened
            .latest_assistant_video_artifact("saved-thread")
            .expect("recheck missing assistant output")
            .expect("project fallback");
        assert!(safe_fallback["output_id"].is_null());
        assert_eq!(safe_fallback["relationship"], "project");

        drop(reopened);
        fs::remove_dir_all(root).ok();
    }
}
