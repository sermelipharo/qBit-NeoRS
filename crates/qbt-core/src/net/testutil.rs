//! Test helpers: a `CoreContext` backed by a temporary profile folder.

use std::path::Path;
use std::sync::Arc;

use crate::app::{AppConfig, Profile};
use crate::context::CoreContext;
use crate::events::EventBus;
use crate::logger::Logger;
use crate::prefs::{Preferences, PreferencesStore};

pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

pub(crate) fn context(rt: &tokio::runtime::Runtime, dir: &Path) -> CoreContext {
    context_with(rt, dir, Preferences::default())
}

pub(crate) fn context_with(rt: &tokio::runtime::Runtime, dir: &Path, prefs: Preferences) -> CoreContext {
    let config = AppConfig { profile_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
    let profile = Arc::new(Profile::resolve(&config));
    for d in [&profile.config_dir, &profile.data_dir, &profile.cache_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let prefs = Arc::new(PreferencesStore::in_memory(prefs));
    let events = EventBus::default();
    let logger = Arc::new(Logger::new(&profile, &prefs, events.clone(), false));
    CoreContext { rt: rt.handle().clone(), profile, prefs, events, logger, config: Arc::new(config) }
}
