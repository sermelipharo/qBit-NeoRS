use std::sync::Arc;

use crate::app::{AppConfig, Profile};
use crate::events::EventBus;
use crate::logger::Logger;
use crate::prefs::PreferencesStore;

/// Shared services handed to every subsystem at start-up.
#[derive(Clone)]
pub struct CoreContext {
    pub rt: tokio::runtime::Handle,
    pub profile: Arc<Profile>,
    pub prefs: Arc<PreferencesStore>,
    pub events: EventBus,
    pub logger: Arc<Logger>,
    pub config: Arc<AppConfig>,
}
