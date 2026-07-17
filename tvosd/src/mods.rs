//! Native, profile-based game mod management.
//!
//! Archives are immutable in a content-addressed vault. Profiles reference
//! exact artifacts; deployment is a reversible materialized transaction with
//! an ownership manifest. Provider discovery is deliberately separate from
//! deployment so Nexus/Workshop/local files all obey the same safety rules.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::config_dir;
use crate::sources::steam;

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_FILES: usize = 25_000;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub id: &'static str,
    pub name: &'static str,
    pub connected: bool,
    pub available: bool,
    pub mode: &'static str,
    pub detail: String,
    pub browse_url: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModRequirement {
    pub id: String,
    #[serde(default = "required_true")]
    pub required: bool,
}

fn required_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledMod {
    pub id: String,
    pub game_id: String,
    pub profile_id: String,
    pub provider: String,
    pub title: String,
    pub version: String,
    pub artifact_hash: String,
    pub enabled: bool,
    pub priority: i64,
    pub file_count: usize,
    pub requirements: Vec<ModRequirement>,
    pub security: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModProfile {
    pub id: String,
    pub game_id: String,
    pub name: String,
    pub active: bool,
    pub locked: bool,
    pub revision: i64,
    pub mod_count: usize,
    pub health: String,
    pub issues: Vec<ProfileIssue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mod_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModJob {
    pub id: String,
    pub game_id: String,
    pub title: String,
    pub phase: String,
    pub status: String,
    pub progress: f64,
    pub detail: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GameModsOverview {
    pub game_id: String,
    pub support_level: String,
    pub support_detail: String,
    pub providers: Vec<ProviderStatus>,
    pub profiles: Vec<ModProfile>,
    pub installed: Vec<InstalledMod>,
    pub jobs: Vec<ModJob>,
}

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub game_id: String,
    pub profile_id: String,
    pub title: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Absolute local archive/folder, file:// URL, or HTTPS archive.
    pub source: String,
    /// Optional relative prefix inside the game directory.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub requirements: Vec<ModRequirement>,
}

fn default_version() -> String {
    "1.0.0".into()
}
fn default_provider() -> String {
    "local".into()
}

#[derive(Debug, Deserialize)]
pub struct ProfileCreateRequest {
    pub game_id: String,
    pub name: String,
    pub clone_from: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProfileActionRequest {
    pub game_id: String,
    pub profile_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ModActionRequest {
    pub game_id: String,
    pub profile_id: String,
    pub mod_id: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeploymentEntry {
    destination: String,
    backup: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeploymentManifest {
    game_id: String,
    profile_id: String,
    game_root: String,
    entries: Vec<DeploymentEntry>,
}

pub struct ModManager {
    conn: Mutex<Connection>,
    root: PathBuf,
}

impl ModManager {
    pub fn open() -> Self {
        let root = config_dir().join("mods");
        let _ = fs::create_dir_all(root.join("vault"));
        let _ = fs::create_dir_all(root.join("staging"));
        let conn = Connection::open(root.join("mods.sqlite3"))
            .unwrap_or_else(|_| Connection::open_in_memory().expect("in-memory mod database"));
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS mod_artifacts(
               hash TEXT PRIMARY KEY, provider TEXT NOT NULL, title TEXT NOT NULL,
               version TEXT NOT NULL, path TEXT NOT NULL, files_json TEXT NOT NULL,
               requirements_json TEXT NOT NULL, security TEXT NOT NULL,
               created_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mod_profiles(
               id TEXT PRIMARY KEY, game_id TEXT NOT NULL, name TEXT NOT NULL,
               active INTEGER NOT NULL DEFAULT 0, locked INTEGER NOT NULL DEFAULT 0,
               revision INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL,
               UNIQUE(game_id,name)
             );
             CREATE TABLE IF NOT EXISTS profile_mods(
               profile_id TEXT NOT NULL, mod_id TEXT NOT NULL, artifact_hash TEXT NOT NULL,
               title TEXT NOT NULL, provider TEXT NOT NULL, version TEXT NOT NULL,
               enabled INTEGER NOT NULL DEFAULT 1, priority INTEGER NOT NULL DEFAULT 0,
               target TEXT NOT NULL DEFAULT '',
               PRIMARY KEY(profile_id,mod_id),
               FOREIGN KEY(profile_id) REFERENCES mod_profiles(id) ON DELETE CASCADE,
               FOREIGN KEY(artifact_hash) REFERENCES mod_artifacts(hash)
             );
             CREATE TABLE IF NOT EXISTS mod_jobs(
               id TEXT PRIMARY KEY, game_id TEXT NOT NULL, title TEXT NOT NULL,
               phase TEXT NOT NULL, status TEXT NOT NULL, progress REAL NOT NULL,
               detail TEXT NOT NULL, updated_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS deployments(
               game_id TEXT PRIMARY KEY, profile_id TEXT NOT NULL,
               manifest_path TEXT NOT NULL, deployed_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS mod_launch_sessions(
               id INTEGER PRIMARY KEY, game_id TEXT NOT NULL, profile_id TEXT NOT NULL,
               revision INTEGER NOT NULL, state TEXT NOT NULL, detail TEXT,
               started_at INTEGER NOT NULL
             );",
        )
        .expect("mod schema");
        Self {
            conn: Mutex::new(conn),
            root,
        }
    }

    pub fn overview(&self, game_id: &str) -> Result<GameModsOverview, String> {
        self.ensure_profiles(game_id)?;
        Ok(GameModsOverview {
            game_id: game_id.to_string(),
            support_level: support_level(game_id).0.into(),
            support_detail: support_level(game_id).1.into(),
            providers: provider_statuses(),
            profiles: self.profiles(game_id)?,
            installed: self.installed(game_id, None)?,
            jobs: self.jobs(Some(game_id))?,
        })
    }

    pub fn search_installed(
        &self,
        game_id: &str,
        q: Option<&str>,
    ) -> Result<Vec<InstalledMod>, String> {
        let query = q.unwrap_or_default().trim().to_lowercase();
        let mut mods = self.installed(game_id, None)?;
        if !query.is_empty() {
            mods.retain(|m| m.title.to_lowercase().contains(&query) || m.provider.contains(&query));
        }
        Ok(mods)
    }

    pub fn profiles(&self, game_id: &str) -> Result<Vec<ModProfile>, String> {
        self.ensure_profiles(game_id)?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id,name,active,locked,revision FROM mod_profiles WHERE game_id=?1 ORDER BY active DESC,name"
        ).map_err(err)?;
        let base: Vec<(String, String, bool, bool, i64)> = stmt
            .query_map([game_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, i64>(3)? != 0,
                    r.get(4)?,
                ))
            })
            .map_err(err)?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        drop(conn);
        base.into_iter()
            .map(|(id, name, active, locked, revision)| {
                let validation = self.validate(&id)?;
                let mod_count = self
                    .installed(game_id, Some(&id))?
                    .iter()
                    .filter(|m| m.enabled)
                    .count();
                Ok(ModProfile {
                    id,
                    game_id: game_id.into(),
                    name,
                    active,
                    locked,
                    revision,
                    mod_count,
                    health: validation.0,
                    issues: validation.1,
                })
            })
            .collect()
    }

    pub fn create_profile(&self, req: &ProfileCreateRequest) -> Result<ModProfile, String> {
        validate_label(&req.game_id, "game_id")?;
        validate_label(&req.name, "profile name")?;
        self.ensure_profiles(&req.game_id)?;
        let id = stable_id(&format!("{}:{}:{}", req.game_id, req.name, now()));
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO mod_profiles(id,game_id,name,created_at) VALUES(?1,?2,?3,?4)",
            params![id, req.game_id, req.name.trim(), now()],
        )
        .map_err(err)?;
        if let Some(source) = req.clone_from.as_deref() {
            let source_game: Option<String> = conn
                .query_row(
                    "SELECT game_id FROM mod_profiles WHERE id=?1",
                    [source],
                    |r| r.get(0),
                )
                .optional()
                .map_err(err)?;
            if source_game.as_deref() != Some(req.game_id.as_str()) {
                conn.execute("DELETE FROM mod_profiles WHERE id=?1", [&id])
                    .map_err(err)?;
                return Err("source profile does not belong to this game".into());
            }
            conn.execute(
                "INSERT INTO profile_mods(profile_id,mod_id,artifact_hash,title,provider,version,enabled,priority,target)
                 SELECT ?1,mod_id,artifact_hash,title,provider,version,enabled,priority,target
                 FROM profile_mods WHERE profile_id=?2", params![id, source]
            ).map_err(err)?;
        }
        drop(conn);
        self.profiles(&req.game_id)?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or("profile was not created".into())
    }

    pub fn delete_profile(&self, req: &ProfileActionRequest) -> Result<(), String> {
        let profiles = self.profiles(&req.game_id)?;
        let profile = profiles
            .iter()
            .find(|p| p.id == req.profile_id)
            .ok_or("profile not found")?;
        if profile.name == "Vanilla" {
            return Err("Vanilla cannot be deleted".into());
        }
        if profile.active {
            return Err("activate another profile before deleting this one".into());
        }
        self.conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute(
                "DELETE FROM mod_profiles WHERE id=?1 AND game_id=?2",
                params![req.profile_id, req.game_id],
            )
            .map_err(err)?;
        Ok(())
    }

    pub fn activate(&self, req: &ProfileActionRequest) -> Result<ModProfile, String> {
        let validation = self.validate(&req.profile_id)?;
        if validation.0 == "blocked" {
            return Err(validation
                .1
                .first()
                .map(|i| i.message.clone())
                .unwrap_or("profile is blocked".into()));
        }
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction().map_err(err)?;
        tx.execute(
            "UPDATE mod_profiles SET active=0 WHERE game_id=?1",
            [&req.game_id],
        )
        .map_err(err)?;
        let changed = tx
            .execute(
                "UPDATE mod_profiles SET active=1 WHERE id=?1 AND game_id=?2",
                params![req.profile_id, req.game_id],
            )
            .map_err(err)?;
        if changed == 0 {
            return Err("profile not found".into());
        }
        tx.commit().map_err(err)?;
        drop(conn);
        self.profiles(&req.game_id)?
            .into_iter()
            .find(|p| p.id == req.profile_id)
            .ok_or("profile not found".into())
    }

    pub fn import(&self, req: ImportRequest) -> Result<InstalledMod, String> {
        validate_label(&req.game_id, "game_id")?;
        validate_label(&req.title, "mod title")?;
        let target = safe_relative(&req.target)?;
        self.ensure_profiles(&req.game_id)?;
        self.assert_profile(&req.game_id, &req.profile_id)?;
        let job_id = stable_id(&format!("job:{}:{}", req.game_id, now_nanos()));
        self.set_job(
            &job_id,
            &req.game_id,
            &req.title,
            "inspect",
            "running",
            5.0,
            "Inspecting source",
        )?;
        let result = self.import_inner(&job_id, &req, target);
        match &result {
            Ok(_) => self.set_job(
                &job_id,
                &req.game_id,
                &req.title,
                "complete",
                "done",
                100.0,
                "Installed into profile",
            )?,
            Err(e) => self.set_job(
                &job_id,
                &req.game_id,
                &req.title,
                "failed",
                "failed",
                100.0,
                e,
            )?,
        }
        result
    }

    fn import_inner(
        &self,
        job_id: &str,
        req: &ImportRequest,
        target: PathBuf,
    ) -> Result<InstalledMod, String> {
        let stage = self.root.join("staging").join(job_id);
        fs::create_dir_all(&stage).map_err(err)?;
        self.set_job(
            job_id,
            &req.game_id,
            &req.title,
            "acquire",
            "running",
            15.0,
            "Acquiring artifact",
        )?;
        let source = acquire(&req.source, &stage)?;
        self.set_job(
            job_id,
            &req.game_id,
            &req.title,
            "verify",
            "running",
            35.0,
            "Hashing and inspecting",
        )?;
        let hash = hash_source(&source)?;
        let vault = self.root.join("vault").join(&hash);
        let files_root = vault.join("files");
        if !files_root.is_dir() {
            fs::create_dir_all(&files_root).map_err(err)?;
            self.set_job(
                job_id,
                &req.game_id,
                &req.title,
                "extract",
                "running",
                55.0,
                "Safely extracting",
            )?;
            if source.is_dir() {
                copy_tree(&source, &files_root)?;
            } else {
                extract_zip(&source, &files_root)?;
            }
        }
        let mut files = list_relative_files(&files_root)?;
        files.retain(|p| !is_package_metadata(p));
        if files.is_empty() {
            return Err("the package contains no deployable files".into());
        }
        let manifest = read_package_manifest(&files_root);
        let title = manifest
            .as_ref()
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or(&req.title)
            .to_string();
        let version = manifest
            .as_ref()
            .and_then(|v| v.get("version_number").or_else(|| v.get("version")))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or(&req.version)
            .to_string();
        let mut requirements = req.requirements.clone();
        if let Some(extra) = manifest
            .as_ref()
            .and_then(|v| v.get("dependencies"))
            .and_then(|v| v.as_array())
        {
            for dep in extra.iter().filter_map(|v| v.as_str()) {
                if !requirements.iter().any(|r| r.id == dep) {
                    requirements.push(ModRequirement {
                        id: dep.into(),
                        required: true,
                    });
                }
            }
        }
        let security = inspect_security(&files);
        self.set_job(
            job_id,
            &req.game_id,
            &title,
            "register",
            "running",
            80.0,
            "Creating immutable profile revision",
        )?;
        let mod_id = format!("{}:{}", req.provider, &hash[..16]);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.unchecked_transaction().map_err(err)?;
        tx.execute(
            "INSERT OR IGNORE INTO mod_artifacts(hash,provider,title,version,path,files_json,requirements_json,security,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![hash, req.provider, title, version, vault.to_string_lossy(), serde_json::to_string(&files).map_err(err)?, serde_json::to_string(&requirements).map_err(err)?, security, now()]
        ).map_err(err)?;
        let priority: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(priority),-1)+1 FROM profile_mods WHERE profile_id=?1",
                [&req.profile_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        tx.execute(
            "INSERT OR REPLACE INTO profile_mods(profile_id,mod_id,artifact_hash,title,provider,version,enabled,priority,target)
             VALUES(?1,?2,?3,?4,?5,?6,1,?7,?8)",
            params![req.profile_id, mod_id, hash, title, req.provider, version, priority, target.to_string_lossy()]
        ).map_err(err)?;
        tx.execute(
            "UPDATE mod_profiles SET revision=revision+1 WHERE id=?1",
            [&req.profile_id],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        drop(conn);
        let _ = fs::remove_dir_all(stage);
        self.installed(&req.game_id, Some(&req.profile_id))?
            .into_iter()
            .find(|m| m.id == mod_id)
            .ok_or("mod registration failed".into())
    }

    pub fn set_enabled(&self, req: &ModActionRequest) -> Result<(), String> {
        self.assert_profile(&req.game_id, &req.profile_id)?;
        let enabled = req.enabled.unwrap_or(true);
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn
            .execute(
                "UPDATE profile_mods SET enabled=?1 WHERE profile_id=?2 AND mod_id=?3",
                params![enabled as i64, req.profile_id, req.mod_id],
            )
            .map_err(err)?;
        if changed == 0 {
            return Err("mod not found in profile".into());
        }
        conn.execute(
            "UPDATE mod_profiles SET revision=revision+1 WHERE id=?1",
            [&req.profile_id],
        )
        .map_err(err)?;
        Ok(())
    }

    pub fn remove_mod(&self, req: &ModActionRequest) -> Result<(), String> {
        self.assert_profile(&req.game_id, &req.profile_id)?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let changed = conn
            .execute(
                "DELETE FROM profile_mods WHERE profile_id=?1 AND mod_id=?2",
                params![req.profile_id, req.mod_id],
            )
            .map_err(err)?;
        if changed == 0 {
            return Err("mod not found in profile".into());
        }
        conn.execute(
            "UPDATE mod_profiles SET revision=revision+1 WHERE id=?1",
            [&req.profile_id],
        )
        .map_err(err)?;
        Ok(())
    }

    pub fn validate_profile(&self, req: &ProfileActionRequest) -> Result<ModProfile, String> {
        self.profiles(&req.game_id)?
            .into_iter()
            .find(|p| p.id == req.profile_id)
            .ok_or("profile not found".into())
    }

    pub fn deploy(&self, req: &ProfileActionRequest) -> Result<ModProfile, String> {
        self.assert_profile(&req.game_id, &req.profile_id)?;
        let profile = self
            .profiles(&req.game_id)?
            .into_iter()
            .find(|p| p.id == req.profile_id)
            .ok_or("profile not found")?;
        if profile.health == "blocked" {
            return Err(profile
                .issues
                .first()
                .map(|i| i.message.clone())
                .unwrap_or("profile is blocked".into()));
        }
        self.rollback_game(&req.game_id)?;
        if profile.name == "Vanilla" {
            return Ok(profile);
        }
        let root = game_root(&req.game_id).ok_or_else(|| "game install directory was not found; set its game-path adapter before deploying mods".to_string())?;
        let root = fs::canonicalize(root)
            .map_err(|e| format!("game install directory is unavailable: {e}"))?;
        let deployment_id = stable_id(&format!(
            "deploy:{}:{}:{}",
            req.game_id,
            req.profile_id,
            now_nanos()
        ));
        let deploy_root = self.root.join("deployments").join(&deployment_id);
        let backup_root = deploy_root.join("backup");
        fs::create_dir_all(&backup_root).map_err(err)?;
        let winners = self.effective_files(&req.profile_id)?;
        let mut entries = Vec::new();
        for (relative, source) in winners {
            reject_symlink_ancestors(&root, &relative)?;
            let destination = safe_join(&root, &relative)?;
            let backup = if destination.is_file() {
                let b = safe_join(&backup_root, &relative)?;
                if let Some(parent) = b.parent() {
                    fs::create_dir_all(parent).map_err(err)?;
                }
                fs::copy(&destination, &b).map_err(err)?;
                Some(b.to_string_lossy().to_string())
            } else {
                None
            };
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(err)?;
            }
            if let Err(e) = fs::copy(&source, &destination) {
                let manifest = DeploymentManifest {
                    game_id: req.game_id.clone(),
                    profile_id: req.profile_id.clone(),
                    game_root: root.to_string_lossy().into(),
                    entries,
                };
                let _ = restore_manifest(&manifest);
                return Err(format!("deployment failed at {}: {e}", relative.display()));
            }
            entries.push(DeploymentEntry {
                destination: destination.to_string_lossy().into(),
                backup,
            });
        }
        let manifest = DeploymentManifest {
            game_id: req.game_id.clone(),
            profile_id: req.profile_id.clone(),
            game_root: root.to_string_lossy().into(),
            entries,
        };
        let manifest_path = deploy_root.join("manifest.json");
        fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).map_err(err)?,
        )
        .map_err(err)?;
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
            "INSERT OR REPLACE INTO deployments(game_id,profile_id,manifest_path,deployed_at) VALUES(?1,?2,?3,?4)",
            params![req.game_id, req.profile_id, manifest_path.to_string_lossy(), now()]
        ).map_err(err)?;
        Ok(profile)
    }

    pub fn rollback(&self, req: &ProfileActionRequest) -> Result<(), String> {
        self.assert_profile(&req.game_id, &req.profile_id)?;
        self.rollback_game(&req.game_id)
    }

    fn rollback_game(&self, game_id: &str) -> Result<(), String> {
        let path: Option<String> = self
            .conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .query_row(
                "SELECT manifest_path FROM deployments WHERE game_id=?1",
                [game_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        let Some(path) = path else {
            return Ok(());
        };
        let manifest: DeploymentManifest =
            serde_json::from_slice(&fs::read(&path).map_err(err)?).map_err(err)?;
        restore_manifest(&manifest)?;
        self.conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execute("DELETE FROM deployments WHERE game_id=?1", [game_id])
            .map_err(err)?;
        Ok(())
    }

    /// Validate and deploy the active/selected profile immediately before a
    /// game launch. Vanilla always restores the original installation first.
    pub fn prepare_launch(&self, game_id: &str, requested: Option<&str>) -> Result<String, String> {
        self.ensure_profiles(game_id)?;
        let profile_id = if let Some(id) = requested {
            id.to_string()
        } else {
            self.conn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .query_row(
                    "SELECT id FROM mod_profiles WHERE game_id=?1 AND active=1 LIMIT 1",
                    [game_id],
                    |r| r.get(0),
                )
                .map_err(err)?
        };
        let req = ProfileActionRequest {
            game_id: game_id.into(),
            profile_id: profile_id.clone(),
        };
        let profile = self.deploy(&req)?;
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
            "INSERT INTO mod_launch_sessions(game_id,profile_id,revision,state,started_at) VALUES(?1,?2,?3,'starting',?4)",
            params![game_id, profile_id, profile.revision, now()]
        ).map_err(err)?;
        Ok(profile.name)
    }

    pub fn diagnostics(&self, game_id: &str) -> Result<serde_json::Value, String> {
        let profiles = self.profiles(game_id)?;
        let active = profiles.iter().find(|p| p.active);
        let deployment: Option<(String, String)> = self
            .conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .query_row(
                "SELECT profile_id,manifest_path FROM deployments WHERE game_id=?1",
                [game_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        Ok(serde_json::json!({
            "game_id": game_id,
            "active_profile": active,
            "deployment": deployment.map(|(profile_id, manifest)| serde_json::json!({"profile_id":profile_id,"manifest":manifest})),
            "game_root": game_root(game_id),
            "last_known_good": active.filter(|p| p.health == "ready" || p.health == "warnings"),
            "diagnosis": "No reproducible mod failure has been recorded.",
        }))
    }

    pub fn jobs(&self, game_id: Option<&str>) -> Result<Vec<ModJob>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = if game_id.is_some() {
            "SELECT id,game_id,title,phase,status,progress,detail,updated_at FROM mod_jobs WHERE game_id=?1 ORDER BY updated_at DESC LIMIT 100"
        } else {
            "SELECT id,game_id,title,phase,status,progress,detail,updated_at FROM mod_jobs ORDER BY updated_at DESC LIMIT 100"
        };
        let mut stmt = conn.prepare(sql).map_err(err)?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(ModJob {
                id: r.get(0)?,
                game_id: r.get(1)?,
                title: r.get(2)?,
                phase: r.get(3)?,
                status: r.get(4)?,
                progress: r.get(5)?,
                detail: r.get(6)?,
                updated_at: r.get(7)?,
            })
        };
        let rows = if let Some(game) = game_id {
            stmt.query_map([game], map)
                .map_err(err)?
                .filter_map(Result::ok)
                .collect()
        } else {
            stmt.query_map([], map)
                .map_err(err)?
                .filter_map(Result::ok)
                .collect()
        };
        Ok(rows)
    }

    fn installed(
        &self,
        game_id: &str,
        profile_id: Option<&str>,
    ) -> Result<Vec<InstalledMod>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = "SELECT pm.mod_id,pm.profile_id,pm.title,pm.provider,pm.version,pm.artifact_hash,pm.enabled,pm.priority,a.files_json,a.requirements_json,a.security
                   FROM profile_mods pm JOIN mod_profiles p ON p.id=pm.profile_id JOIN mod_artifacts a ON a.hash=pm.artifact_hash
                   WHERE p.game_id=?1 AND (?2='' OR pm.profile_id=?2) ORDER BY pm.priority,pm.title";
        let mut stmt = conn.prepare(sql).map_err(err)?;
        let profile = profile_id.unwrap_or("");
        let rows = stmt
            .query_map(params![game_id, profile], |r| {
                let files: Vec<String> =
                    serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default();
                let requirements: Vec<ModRequirement> =
                    serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default();
                Ok(InstalledMod {
                    id: r.get(0)?,
                    game_id: game_id.into(),
                    profile_id: r.get(1)?,
                    title: r.get(2)?,
                    provider: r.get(3)?,
                    version: r.get(4)?,
                    artifact_hash: r.get(5)?,
                    enabled: r.get::<_, i64>(6)? != 0,
                    priority: r.get(7)?,
                    file_count: files.len(),
                    requirements,
                    security: r.get(10)?,
                })
            })
            .map_err(err)?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    fn validate(&self, profile_id: &str) -> Result<(String, Vec<ProfileIssue>), String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let game_id: String = conn
            .query_row(
                "SELECT game_id FROM mod_profiles WHERE id=?1",
                [profile_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        drop(conn);
        let mods = self.installed(&game_id, Some(profile_id))?;
        let enabled: Vec<_> = mods.iter().filter(|m| m.enabled).collect();
        let mut issues = Vec::new();
        for m in &enabled {
            for requirement in m.requirements.iter().filter(|r| r.required) {
                let needle = normalize(&requirement.id);
                let found = enabled.iter().any(|candidate| {
                    let name = normalize(&candidate.title);
                    let id = normalize(&candidate.id);
                    needle.contains(&name) || name.contains(&needle) || needle == id
                });
                if !found {
                    issues.push(ProfileIssue {
                        severity: "error".into(),
                        code: "missing_dependency".into(),
                        message: format!("{} requires {}", m.title, requirement.id),
                        mod_id: Some(m.id.clone()),
                    });
                }
            }
            if m.security != "data-only" {
                issues.push(ProfileIssue {
                    severity: "warning".into(),
                    code: "executable_code".into(),
                    message: format!("{} contains {}", m.title, m.security),
                    mod_id: Some(m.id.clone()),
                });
            }
        }
        let files = self.profile_files(profile_id)?;
        for (path, owners) in files {
            if owners.len() > 1 {
                issues.push(ProfileIssue {
                    severity: "warning".into(),
                    code: "file_conflict".into(),
                    message: format!(
                        "{} is supplied by {} mods; the later mod wins",
                        path.display(),
                        owners.len()
                    ),
                    mod_id: owners.last().cloned(),
                });
            }
        }
        let health = if issues.iter().any(|i| i.severity == "error") {
            "blocked"
        } else if issues.is_empty() {
            "ready"
        } else {
            "warnings"
        };
        Ok((health.into(), issues))
    }

    fn profile_files(&self, profile_id: &str) -> Result<BTreeMap<PathBuf, Vec<String>>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT pm.mod_id,pm.target,a.files_json FROM profile_mods pm JOIN mod_artifacts a ON a.hash=pm.artifact_hash WHERE pm.profile_id=?1 AND pm.enabled=1 ORDER BY pm.priority").map_err(err)?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([profile_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(err)?
            .filter_map(Result::ok)
            .collect();
        let mut out: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        for (mod_id, target, json) in rows {
            let target = safe_relative(&target)?;
            for file in serde_json::from_str::<Vec<String>>(&json).unwrap_or_default() {
                let rel = target.join(safe_relative(&file)?);
                out.entry(rel).or_default().push(mod_id.clone());
            }
        }
        Ok(out)
    }

    fn effective_files(&self, profile_id: &str) -> Result<BTreeMap<PathBuf, PathBuf>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare("SELECT pm.target,a.path,a.files_json FROM profile_mods pm JOIN mod_artifacts a ON a.hash=pm.artifact_hash WHERE pm.profile_id=?1 AND pm.enabled=1 ORDER BY pm.priority").map_err(err)?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([profile_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(err)?
            .filter_map(Result::ok)
            .collect();
        let mut out = BTreeMap::new();
        for (target, vault, json) in rows {
            let target = safe_relative(&target)?;
            for file in serde_json::from_str::<Vec<String>>(&json).unwrap_or_default() {
                let file = safe_relative(&file)?;
                out.insert(
                    target.join(&file),
                    PathBuf::from(&vault).join("files").join(file),
                );
            }
        }
        Ok(out)
    }

    fn ensure_profiles(&self, game_id: &str) -> Result<(), String> {
        validate_label(game_id, "game_id")?;
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mod_profiles WHERE game_id=?1",
                [game_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        if count == 0 {
            conn.execute("INSERT INTO mod_profiles(id,game_id,name,active,locked,created_at) VALUES(?1,?2,'Vanilla',1,1,?3)", params![stable_id(&format!("{game_id}:vanilla")),game_id,now()]).map_err(err)?;
            conn.execute("INSERT INTO mod_profiles(id,game_id,name,active,locked,created_at) VALUES(?1,?2,'Default Modded',0,0,?3)", params![stable_id(&format!("{game_id}:default")),game_id,now()]).map_err(err)?;
        }
        Ok(())
    }

    fn assert_profile(&self, game_id: &str, profile_id: &str) -> Result<(), String> {
        let exists: i64 = self
            .conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mod_profiles WHERE id=?1 AND game_id=?2)",
                params![profile_id, game_id],
                |r| r.get(0),
            )
            .map_err(err)?;
        if exists == 0 {
            Err("profile not found".into())
        } else {
            Ok(())
        }
    }

    fn set_job(
        &self,
        id: &str,
        game: &str,
        title: &str,
        phase: &str,
        status: &str,
        progress: f64,
        detail: &str,
    ) -> Result<(), String> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner()).execute(
            "INSERT OR REPLACE INTO mod_jobs(id,game_id,title,phase,status,progress,detail,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id,game,title,phase,status,progress,detail,now()]
        ).map_err(err)?;
        Ok(())
    }
}

fn provider_statuses() -> Vec<ProviderStatus> {
    // Provider status is rendered on a latency-sensitive game page. Checking
    // PATH is deterministic and avoids starting Steam/Flatpak helper processes.
    let executable = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
    };
    let nexus = std::env::var("TVOS_NEXUS_API_KEY").is_ok();
    let modio = std::env::var("TVOS_MODIO_TOKEN").is_ok();
    let steam_available = executable("steam") || executable("flatpak");
    vec![
        ProviderStatus{id:"nexus",name:"Nexus Mods",connected:nexus,available:true,mode:"oauth",detail:if nexus{"Connected".into()}else{"Connect a registered TV OS Nexus application to browse and download.".into()},browse_url:Some("https://www.nexusmods.com/games")},
        ProviderStatus{id:"workshop",name:"Steam Workshop",connected:steam_available,available:steam_available,mode:"steam_managed",detail:"Steam owns Workshop subscriptions, transfers, and updates; TV OS validates them in profiles.".into(),browse_url:Some("https://steamcommunity.com/workshop/")},
        ProviderStatus{id:"modio",name:"mod.io",connected:modio,available:true,mode:"oauth",detail:if modio{"Connected".into()}else{"Device login is required for subscriptions.".into()},browse_url:Some("https://mod.io/g")},
        ProviderStatus{id:"thunderstore",name:"Thunderstore",connected:true,available:true,mode:"public_api",detail:"Public packages and dependency manifests are supported.".into(),browse_url:Some("https://thunderstore.io/")},
        ProviderStatus{id:"modrinth",name:"Modrinth",connected:true,available:true,mode:"public_api",detail:"Loader-aware Minecraft packages.".into(),browse_url:Some("https://modrinth.com/mods")},
        ProviderStatus{id:"curseforge",name:"CurseForge",connected:false,available:true,mode:"api_key",detail:"A CurseForge API application key is required.".into(),browse_url:Some("https://www.curseforge.com/minecraft")},
        ProviderStatus{id:"github",name:"GitHub releases",connected:true,available:true,mode:"public_api",detail:"Release assets from explicitly selected repositories.".into(),browse_url:Some("https://github.com/")},
        ProviderStatus{id:"local",name:"Local package",connected:true,available:true,mode:"native",detail:"ZIP archives and folders are inspected and stored immutably.".into(),browse_url:None},
    ]
}

fn support_level(game_id: &str) -> (&'static str, &'static str) {
    if game_id.starts_with("steam:") {
        ("adapter_managed","Steam installation detected; generic transactional deployment and Workshop monitoring are available.")
    } else {
        ("generic_managed","Profiles, dependency checks, archive inspection, and rollback are available; game-specific semantic compatibility is unverified.")
    }
}

fn game_root(game_id: &str) -> Option<PathBuf> {
    let env_key = format!(
        "TVOS_GAME_ROOT_{}",
        game_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    );
    std::env::var(env_key)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| steam::install_dir(game_id))
}

fn acquire(source: &str, stage: &Path) -> Result<PathBuf, String> {
    if source.starts_with("https://") {
        let dest = stage.join("download.zip");
        let mut response = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(1800))
            .build()
            .map_err(err)?
            .get(source)
            .send()
            .map_err(err)?;
        if !response.status().is_success() {
            return Err(format!("download failed with {}", response.status()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_ARCHIVE_BYTES)
        {
            return Err("archive exceeds 8 GiB safety limit".into());
        }
        let mut file = File::create(&dest).map_err(err)?;
        let mut total = 0u64;
        let mut buf = [0u8; 128 * 1024];
        loop {
            let n = response.read(&mut buf).map_err(err)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_ARCHIVE_BYTES {
                return Err("archive exceeds 8 GiB safety limit".into());
            }
            file.write_all(&buf[..n]).map_err(err)?;
        }
        Ok(dest)
    } else {
        let raw = source.strip_prefix("file://").unwrap_or(source);
        let path = fs::canonicalize(raw).map_err(|e| format!("source is unavailable: {e}"))?;
        if !path.is_file() && !path.is_dir() {
            return Err("source must be a ZIP file or directory".into());
        }
        Ok(path)
    }
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(archive).map_err(err)?;
    let mut zip = zip::ZipArchive::new(file).map_err(err)?;
    if zip.len() > MAX_FILES {
        return Err("archive has too many files".into());
    }
    let mut expanded = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(err)?;
        let Some(name) = entry.enclosed_name() else {
            return Err("archive contains an unsafe path".into());
        };
        if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("archive symlinks are not allowed".into());
        }
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAX_EXPANDED_BYTES {
            return Err("expanded archive exceeds 16 GiB safety limit".into());
        }
        let out = safe_join(dest, &name)?;
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(err)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p).map_err(err)?;
            }
            let mut f = File::create(out).map_err(err)?;
            std::io::copy(&mut entry, &mut f).map_err(err)?;
        }
    }
    Ok(())
}

fn hash_source(path: &Path) -> Result<String, String> {
    let mut h = Sha256::new();
    if path.is_file() {
        let mut f = File::open(path).map_err(err)?;
        let mut b = [0u8; 128 * 1024];
        loop {
            let n = f.read(&mut b).map_err(err)?;
            if n == 0 {
                break;
            }
            h.update(&b[..n]);
        }
    } else {
        for rel in list_relative_files(path)? {
            h.update(rel.as_bytes());
            let mut f = File::open(path.join(&rel)).map_err(err)?;
            let mut b = [0u8; 64 * 1024];
            loop {
                let n = f.read(&mut b).map_err(err)?;
                if n == 0 {
                    break;
                }
                h.update(&b[..n]);
            }
        }
    }
    Ok(format!("{:x}", h.finalize()))
}
fn copy_tree(src: &Path, dest: &Path) -> Result<(), String> {
    for rel in list_relative_files(src)? {
        let to = safe_join(dest, Path::new(&rel))?;
        if let Some(p) = to.parent() {
            fs::create_dir_all(p).map_err(err)?;
        }
        fs::copy(src.join(&rel), to).map_err(err)?;
    }
    Ok(())
}
fn list_relative_files(root: &Path) -> Result<Vec<String>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>, size: &mut u64) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(err)? {
            let entry = entry.map_err(err)?;
            let ty = entry.file_type().map_err(err)?;
            if ty.is_symlink() {
                return Err("symlinks are not allowed in mod packages".into());
            }
            if ty.is_dir() {
                walk(root, &entry.path(), out, size)?;
            } else if ty.is_file() {
                if out.len() >= MAX_FILES {
                    return Err("package has too many files".into());
                }
                *size = size.saturating_add(entry.metadata().map_err(err)?.len());
                if *size > MAX_EXPANDED_BYTES {
                    return Err("package exceeds the 16 GiB expanded-size limit".into());
                }
                out.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .map_err(err)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    let mut size = 0;
    walk(root, root, &mut out, &mut size)?;
    out.sort();
    Ok(out)
}
fn read_package_manifest(root: &Path) -> Option<serde_json::Value> {
    ["manifest.json", "mod.json"].iter().find_map(|p| {
        fs::read(root.join(p))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    })
}
fn is_package_metadata(path: &str) -> bool {
    matches!(
        path.to_ascii_lowercase().as_str(),
        "manifest.json" | "mod.json" | "readme.md" | "changelog.md" | "icon.png"
    )
}
fn inspect_security(files: &[String]) -> String {
    let mut kinds = BTreeSet::new();
    for f in files {
        match Path::new(f)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "dll" => {
                kinds.insert("native or managed DLL");
            }
            "exe" | "msi" => {
                kinds.insert("executable installer");
            }
            "sh" | "bat" | "cmd" | "ps1" => {
                kinds.insert("script");
            }
            _ => {}
        }
    }
    if kinds.is_empty() {
        "data-only".into()
    } else {
        kinds.into_iter().collect::<Vec<_>>().join(", ")
    }
}
fn restore_manifest(manifest: &DeploymentManifest) -> Result<(), String> {
    for entry in manifest.entries.iter().rev() {
        let dest = PathBuf::from(&entry.destination);
        if let Some(backup) = &entry.backup {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p).map_err(err)?;
            }
            fs::copy(backup, &dest).map_err(err)?;
        } else if dest.exists() {
            fs::remove_file(&dest).map_err(err)?;
        }
    }
    Ok(())
}
fn safe_relative(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err("target path must be relative".into());
    }
    for c in path.components() {
        if !matches!(c, Component::Normal(_) | Component::CurDir) {
            return Err("path traversal is not allowed".into());
        }
    }
    Ok(path
        .components()
        .filter_map(|c| {
            if let Component::Normal(v) = c {
                Some(v)
            } else {
                None
            }
        })
        .collect())
}
fn safe_join(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    let rel = safe_relative(&rel.to_string_lossy())?;
    Ok(root.join(rel))
}
fn reject_symlink_ancestors(root: &Path, rel: &Path) -> Result<(), String> {
    let rel = safe_relative(&rel.to_string_lossy())?;
    let mut current = root.to_path_buf();
    for component in rel.components() {
        if let Component::Normal(part) = component {
            current.push(part);
            if let Ok(meta) = fs::symlink_metadata(&current) {
                if meta.file_type().is_symlink() {
                    return Err(format!(
                        "deployment path crosses a symlink: {}",
                        current.display()
                    ));
                }
            }
        }
    }
    Ok(())
}
fn validate_label(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 180
        || value.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
    {
        Err(format!("invalid {label}"))
    } else {
        Ok(())
    }
}
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn stable_id(seed: &str) -> String {
    let mut h = Sha256::new();
    h.update(seed.as_bytes());
    format!("{:x}", h.finalize())[..24].to_string()
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traversal_is_rejected() {
        assert!(safe_relative("../game.exe").is_err());
        assert!(safe_relative("mods/ok.dll").is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn deployment_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("tvos-mod-symlink-{}", now_nanos()));
        let outside = root.with_extension("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("Mods")).unwrap();
        assert!(reject_symlink_ancestors(&root, Path::new("Mods/escape.dll")).is_err());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
    #[test]
    fn security_classifies_code() {
        assert_eq!(inspect_security(&["textures/a.dds".into()]), "data-only");
        assert!(inspect_security(&["BepInEx/x.dll".into()]).contains("DLL"));
    }
    #[test]
    fn ids_are_stable() {
        assert_eq!(stable_id("same"), stable_id("same"));
        assert_ne!(stable_id("same"), stable_id("other"));
    }
}
