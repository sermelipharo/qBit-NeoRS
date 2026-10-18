//! Persistent key/value store shared by all WebUI clients
//! (qBittorrent `ClientDataStorage`, `clientdata/load|store`), saved to
//! `config/webui_clientdata.json`, at most 1 MiB of compact JSON.

use std::path::PathBuf;

use parking_lot::Mutex;
use serde_json::{Map, Value};

pub const MAX_SIZE: usize = 1024 * 1024;

pub struct ClientDataStorage {
    path: PathBuf,
    data: Mutex<Map<String, Value>>,
}

impl ClientDataStorage {
    /// Loads the file; a missing, oversized or malformed file yields an empty
    /// store (and a warning through `warn`).
    pub fn load(path: PathBuf, warn: impl Fn(String)) -> Self {
        let data = match std::fs::metadata(&path) {
            Err(_) => Map::new(),
            Ok(m) if m.len() > MAX_SIZE as u64 => {
                warn(format!("Failed to load web client data. File: \"{}\" is too large", path.display()));
                Map::new()
            }
            Ok(_) => match std::fs::read(&path).map(|b| serde_json::from_slice::<Value>(&b)) {
                Ok(Ok(Value::Object(m))) => m,
                Ok(Ok(_)) => {
                    warn(format!(
                        "Failed to load web client data. File: \"{}\". Error: \"Invalid data format\"",
                        path.display()
                    ));
                    Map::new()
                }
                Ok(Err(e)) => {
                    warn(format!("Failed to parse web client data. File: \"{}\". Error: \"{e}\"", path.display()));
                    Map::new()
                }
                Err(e) => {
                    warn(format!("Failed to load web client data. {e}"));
                    Map::new()
                }
            },
        };
        Self { path, data: Mutex::new(data) }
    }

    pub fn load_all(&self) -> Map<String, Value> {
        self.data.lock().clone()
    }

    pub fn load_keys(&self, keys: &[String]) -> Map<String, Value> {
        let d = self.data.lock();
        keys.iter().filter_map(|k| d.get(k).map(|v| (k.clone(), v.clone()))).collect()
    }

    /// Merges `obj` (`null` deletes a key). The file is rewritten only when
    /// something changed.
    pub fn store(&self, obj: Map<String, Value>) -> Result<(), String> {
        let mut d = self.data.lock();
        let mut next = d.clone();
        let mut modified = false;
        for (k, v) in obj {
            if v.is_null() {
                modified |= next.shift_remove(&k).is_some();
            } else if next.get(&k) != Some(&v) {
                next.insert(k, v);
                modified = true;
            }
        }
        if !modified {
            return Ok(());
        }
        let json = serde_json::to_vec(&next).map_err(|e| e.to_string())?;
        if json.len() > MAX_SIZE {
            return Err(format!("Total web client data must not be larger than {MAX_SIZE} bytes"));
        }
        crate::utils::fs::write_atomic(&self.path, &json)
            .map_err(|e| format!("Failed to save web client data. Error: \"{e}\""))?;
        *d = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn store_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webui_clientdata.json");
        let s = ClientDataStorage::load(path.clone(), |_| {});
        s.store(json!({"a": 1, "b": {"c": true}}).as_object().unwrap().clone()).unwrap();
        s.store(json!({"a": null, "d": "x"}).as_object().unwrap().clone()).unwrap();
        assert_eq!(Value::Object(s.load_all()), json!({"b": {"c": true}, "d": "x"}));
        assert_eq!(Value::Object(s.load_keys(&["d".into(), "zz".into()])), json!({"d": "x"}));
        let s2 = ClientDataStorage::load(path, |_| {});
        assert_eq!(Value::Object(s2.load_all()), json!({"b": {"c": true}, "d": "x"}));
    }

    #[test]
    fn size_limit_and_bad_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        std::fs::write(&path, b"[1,2]").unwrap();
        let warned = std::sync::atomic::AtomicBool::new(false);
        let s = ClientDataStorage::load(path, |_| warned.store(true, std::sync::atomic::Ordering::Relaxed));
        assert!(warned.into_inner());
        assert!(s.load_all().is_empty());
        let big = "x".repeat(MAX_SIZE);
        let e = s.store(json!({"k": big}).as_object().unwrap().clone()).unwrap_err();
        assert_eq!(e, "Total web client data must not be larger than 1048576 bytes");
        assert!(s.load_all().is_empty());
    }
}
