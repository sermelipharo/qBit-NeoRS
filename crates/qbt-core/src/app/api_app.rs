//! Application-level calls: preferences, build info, network interfaces,
//! cookies, test e-mail. Mirrors `/api/v2/app/*`.

use super::App;
use crate::error::QbtResult;
use crate::model::*;
use crate::prefs::Preferences;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    pub fn version(&self) -> String {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    }

    pub fn build_info(&self) -> BuildInfo {
        BuildInfo {
            app: env!("CARGO_PKG_VERSION").to_string(),
            qbittorrent_compat: QBT_COMPAT_VERSION.to_string(),
            webapi: WEBAPI_VERSION.to_string(),
            engine: "librqbit 9.0.1 (qBit NeoRS fork)".to_string(),
            rustc: option_env!("QBT_RUSTC_VERSION").unwrap_or("rustc").to_string(),
            platform: std::env::consts::OS.to_string(),
            bitness: (std::mem::size_of::<usize>() * 8) as i32,
        }
    }

    pub fn preferences(&self) -> Preferences {
        self.ctx.prefs.get()
    }

    /// Replaces all preferences; subsystems apply the differences live.
    pub fn set_preferences(&self, prefs: Preferences) -> QbtResult<()> {
        self.ctx.prefs.set(prefs)
    }

    /// Partial update with a WebAPI `setPreferences` JSON object.
    pub fn set_preferences_json(&self, json: String) -> QbtResult<()> {
        self.ctx.prefs.apply_json(&json)
    }

    /// All preferences as the WebAPI `app/preferences` JSON object.
    pub fn preferences_json(&self) -> String {
        self.ctx.prefs.to_api_json()
    }

    pub fn default_save_path(&self) -> String {
        self.ctx.prefs.get().downloads.save_path
    }

    pub fn network_interfaces(&self) -> Vec<NetworkInterface> {
        crate::net::interfaces::list()
    }

    /// Addresses of one interface ("" = all interfaces).
    pub fn network_interface_addresses(&self, interface: String) -> Vec<String> {
        crate::net::interfaces::addresses(&interface)
    }

    pub fn cookies(&self) -> Vec<Cookie> {
        crate::net::http::cookies(&self.ctx)
    }

    pub fn set_cookies(&self, cookies: Vec<Cookie>) -> QbtResult<()> {
        crate::net::http::set_cookies(&self.ctx, cookies)
    }

    pub async fn send_test_email(&self) -> QbtResult<()> {
        let ctx = self.ctx.clone();
        self.spawn_on_rt(async move { crate::net::smtp::send_test_email(&ctx).await }).await
    }

    /// "Test Port": asks portcheck.transmissionbt.com whether `port` is
    /// reachable from the internet (`true` = open). Sends the public IP
    /// address and the port to that service; call only on a user's request.
    pub async fn check_port(&self, port: i64) -> QbtResult<bool> {
        let ctx = self.ctx.clone();
        self.spawn_on_rt(async move { crate::net::portcheck::check_port(&ctx, port).await }).await
    }

    /// Absolute free space (bytes) of the volume containing `path`.
    pub fn free_disk_space(&self, path: String) -> i64 {
        crate::utils::fs::free_space(&path).unwrap_or(-1)
    }

    /// Signals the daemon/front end that the user asked to quit (WebAPI `app/shutdown`).
    pub fn request_shutdown(&self) {
        self.ctx.events.request_shutdown();
    }

    /// WebUI server state: URL, errors and the temporary password generated
    /// when no WebUI password is configured.
    pub fn webui_status(&self) -> crate::webui::WebUiStatus {
        self.webui.status()
    }
}
