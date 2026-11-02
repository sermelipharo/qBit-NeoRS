//! Lua plugins (qBittorrent 5.3 Plugins dialog and menu). No WebAPI:
//! qBittorrent 5.3 has no plugin endpoints.

use super::App;
use crate::error::QbtResult;
use crate::plugins::{PluginInstallResult, PluginsStatus};

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// Installed plugins and the engine state (empty while plugins are off).
    pub fn plugins_status(&self) -> PluginsStatus {
        self.plugins.status()
    }

    /// Installs or updates a plugin from a local `.lua` file (absolute
    /// path). An update must have a higher `VERSION`; failures of an update
    /// are `Conflict` errors.
    pub async fn install_plugin(&self, path: String) -> QbtResult<PluginInstallResult> {
        let plugins = self.plugins.clone();
        self.spawn_on_rt(async move { plugins.install(path).await }).await
    }

    pub async fn uninstall_plugin(&self, id: String) -> QbtResult<()> {
        let plugins = self.plugins.clone();
        self.spawn_on_rt(async move { plugins.uninstall(id).await }).await
    }

    pub async fn set_plugin_enabled(&self, id: String, enabled: bool) -> QbtResult<()> {
        let plugins = self.plugins.clone();
        self.spawn_on_rt(async move { plugins.set_enabled(id, enabled).await }).await
    }

    /// Calls an enabled plugin's `invoke` function (Plugins menu).
    pub async fn invoke_plugin(&self, id: String) -> QbtResult<()> {
        let plugins = self.plugins.clone();
        self.spawn_on_rt(async move { plugins.invoke(id).await }).await
    }
}
