//! Preferences: model, persistence and change notification.

mod api_json;
mod enums;
mod model;

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::watch;

pub use enums::*;
pub use model::*;

use crate::app::Profile;
use crate::error::{QbtError, QbtResult};

/// Owns the current [`Preferences`], persists them to
/// `config/preferences.json` and notifies subscribers of changes.
pub struct PreferencesStore {
    path: PathBuf,
    /// Serializes writers so concurrent `set` calls cannot interleave.
    write_lock: Mutex<()>,
    tx: watch::Sender<Arc<Preferences>>,
}

impl PreferencesStore {
    pub fn load(profile: &Profile) -> QbtResult<Self> {
        let path = profile.preferences_file();
        let mut prefs: Preferences = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes).and_then(|mut v| {
                migrate_stored(&mut v);
                serde_json::from_value(v)
            }).unwrap_or_else(|e| {
                // A corrupt file must never prevent start-up; keep a copy for the user.
                let _ = std::fs::copy(&path, path.with_extension("json.corrupt"));
                eprintln!("preferences.json is invalid ({e}); using defaults");
                Preferences::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
            Err(e) => return Err(e.into()),
        };
        let changed = resolve_dynamic_defaults(&mut prefs, profile);
        let (tx, _) = watch::channel(Arc::new(prefs));
        let store = Self { path, write_lock: Mutex::new(()), tx };
        if changed {
            store.persist(&store.get())?;
        }
        Ok(store)
    }

    /// In-memory store for tests.
    pub fn in_memory(prefs: Preferences) -> Self {
        let (tx, _) = watch::channel(Arc::new(prefs));
        Self { path: PathBuf::new(), write_lock: Mutex::new(()), tx }
    }

    /// Snapshot of the current preferences.
    pub fn get(&self) -> Preferences {
        (**self.tx.borrow()).clone()
    }

    /// Cheap shared snapshot.
    pub fn current(&self) -> Arc<Preferences> {
        self.tx.borrow().clone()
    }

    /// Change notifications (the receiver sees the latest value).
    pub fn subscribe(&self) -> watch::Receiver<Arc<Preferences>> {
        self.tx.subscribe()
    }

    /// Replaces all preferences after validation, persists and notifies.
    pub fn set(&self, prefs: Preferences) -> QbtResult<()> {
        let _g = self.write_lock.lock();
        self.store(prefs)
    }

    /// Validates, persists and publishes; the caller holds `write_lock`.
    fn store(&self, prefs: Preferences) -> QbtResult<()> {
        validate(&prefs)?;
        self.persist(&prefs)?;
        self.tx.send_replace(Arc::new(prefs));
        Ok(())
    }

    /// Applies `f` to a copy of the current preferences and stores the result.
    /// The read-modify-write is atomic: concurrent updates never overwrite
    /// each other's changes.
    pub fn update(&self, f: impl FnOnce(&mut Preferences)) -> QbtResult<()> {
        let _g = self.write_lock.lock();
        let mut p = self.get();
        f(&mut p);
        self.store(p)
    }

    /// WebAPI `setPreferences`: partial update from a JSON object.
    pub fn apply_json(&self, json: &str) -> QbtResult<()> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let obj = value.as_object().ok_or_else(|| QbtError::invalid("preferences must be a JSON object"))?;
        let _g = self.write_lock.lock();
        let mut p = self.get();
        api_json::apply(&mut p, obj)?;
        self.store(p)
    }

    /// WebAPI `app/preferences` JSON object.
    pub fn to_api_json(&self) -> String {
        api_json::to_json(&self.current()).to_string()
    }

    /// WebAPI `app/preferences` JSON value.
    pub fn to_api_value(&self) -> serde_json::Value {
        api_json::to_json(&self.current())
    }

    fn persist(&self, prefs: &Preferences) -> QbtResult<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let data = serde_json::to_vec_pretty(prefs).map_err(|e| QbtError::other(e.to_string()))?;
        crate::utils::fs::write_atomic(&self.path, &data)
    }
}

fn validate(p: &Preferences) -> QbtResult<()> {
    let user = &p.webui.web_ui_username;
    if user.chars().count() < 3 {
        return Err(QbtError::invalid("WebUI username must be at least 3 characters long"));
    }
    if user.contains(':') {
        return Err(QbtError::invalid("WebUI username cannot contain a colon"));
    }
    Ok(())
}

/// Fills values whose defaults depend on the machine (save path, random
/// ports). Returns true when something was filled in and must be persisted.
/// Stored keys renamed since: `mail_notification_ssl_enabled` became
/// `mail_notification_encryption_type` (qBittorrent 5.3's
/// `migrateSMTPEncryptionSetting`: SSL on = SMTPS, off = None);
/// `export_dir` / `export_dir_fin` became the .torrent backup folders
/// (`migrateTorrentExportFolderSettings`: the folder is kept and enabled
/// when it was set).
fn migrate_stored(v: &mut serde_json::Value) {
    let Some(d) = v.get_mut("downloads").and_then(serde_json::Value::as_object_mut) else { return };
    if let Some(ssl) = d.remove("mail_notification_ssl_enabled")
        && !d.contains_key("mail_notification_encryption_type")
    {
        let t = if ssl.as_bool().unwrap_or(false) { SmtpEncryptionType::Smtps } else { SmtpEncryptionType::None };
        d.insert("mail_notification_encryption_type".into(), serde_json::to_value(t).unwrap_or_default());
    }
    for (old, dir, enabled) in [
        ("export_dir", "torrent_files_backup_dir", "torrent_files_backup_enabled"),
        ("export_dir_fin", "torrent_files_finished_backup_dir", "torrent_files_finished_backup_dir_enabled"),
    ] {
        if let Some(old) = d.remove(old)
            && !d.contains_key(dir)
        {
            let path = old.as_str().unwrap_or_default().trim().to_owned();
            d.insert(enabled.into(), serde_json::Value::Bool(!path.is_empty()));
            let path = if path.is_empty() { DEFAULT_TORRENT_BACKUP_DIR.to_owned() } else { path };
            d.insert(dir.into(), serde_json::Value::String(path));
        }
    }
}

fn resolve_dynamic_defaults(p: &mut Preferences, _profile: &Profile) -> bool {
    let mut changed = false;
    if p.downloads.save_path.is_empty() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        p.downloads.save_path = home.join("Downloads").to_string_lossy().into_owned();
        changed = true;
    }
    if p.downloads.temp_path.is_empty() {
        p.downloads.temp_path = PathBuf::from(&p.downloads.save_path).join("temp").to_string_lossy().into_owned();
        changed = true;
    }
    if p.connection.listen_port < 0 {
        p.connection.listen_port = random_port(None);
        changed = true;
    }
    if p.connection.ssl_listen_port < 0 {
        p.connection.ssl_listen_port = random_port(Some(p.connection.listen_port));
        changed = true;
    }
    if p.advanced.dht_bootstrap_nodes.trim().is_empty() {
        p.advanced.dht_bootstrap_nodes = DEFAULT_DHT_BOOTSTRAP_NODES.into();
        changed = true;
    }
    changed
}

fn random_port(avoid: Option<i64>) -> i64 {
    loop {
        let p = 1024 + (rand::random::<u32>() % (65535 - 1024 + 1)) as i64;
        if Some(p) != avoid {
            return p;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_the_old_export_folders() {
        let load = |json: &str| {
            let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
            migrate_stored(&mut v);
            serde_json::from_value::<Preferences>(v).unwrap().downloads
        };
        let d = load(r#"{"downloads": {"export_dir": "/t/all", "export_dir_fin": ""}}"#);
        assert!(d.torrent_files_backup_enabled);
        assert_eq!(d.torrent_files_backup_dir, "/t/all");
        assert!(!d.torrent_files_finished_backup_dir_enabled);
        assert_eq!(d.torrent_files_finished_backup_dir, "backup");
        let d = load(r#"{"downloads": {"export_dir_fin": " /t/done "}}"#);
        assert!(!d.torrent_files_backup_enabled);
        assert!(d.torrent_files_finished_backup_dir_enabled);
        assert_eq!(d.torrent_files_finished_backup_dir, "/t/done");
        // Already migrated: the new keys win.
        let d = load(r#"{"downloads": {"export_dir": "/old", "torrent_files_backup_dir": "/new"}}"#);
        assert_eq!((d.torrent_files_backup_enabled, d.torrent_files_backup_dir.as_str()), (false, "/new"));
        assert_eq!(load("{}"), DownloadsPrefs::default());
    }

    #[test]
    fn migrates_the_old_smtp_ssl_switch() {
        let load = |json: &str| {
            let mut v: serde_json::Value = serde_json::from_str(json).unwrap();
            migrate_stored(&mut v);
            serde_json::from_value::<Preferences>(v).unwrap().downloads.mail_notification_encryption_type
        };
        assert_eq!(load(r#"{"downloads": {"mail_notification_ssl_enabled": true}}"#), SmtpEncryptionType::Smtps);
        assert_eq!(load(r#"{"downloads": {"mail_notification_ssl_enabled": false}}"#), SmtpEncryptionType::None);
        assert_eq!(
            load(r#"{"downloads": {"mail_notification_ssl_enabled": false, "mail_notification_encryption_type": "Starttls"}}"#),
            SmtpEncryptionType::Starttls
        );
        assert_eq!(load(r#"{"downloads": {}}"#), SmtpEncryptionType::Smtps);
        assert_eq!(load("{}"), SmtpEncryptionType::Smtps);
    }
}
