//! Helpers for native front ends: UI-lock password hashing and tracker
//! favicons. Neither touches the WebAPI preferences.

use super::App;
use crate::error::{QbtError, QbtResult};

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// PBKDF2-HMAC-SHA512 hash (`base64(salt):base64(hash)`) for the UI-lock
    /// password; the front end stores it, never the password.
    pub async fn hash_ui_lock_password(&self, password: String) -> QbtResult<String> {
        self.spawn_on_rt(async move {
            tokio::task::spawn_blocking(move || crate::webui::password::hash_password(&password))
                .await
                .map_err(|e| QbtError::other(format!("task failed: {e}")))
        })
        .await
    }

    /// Checks `password` against a hash made by `hash_ui_lock_password`
    /// (constant-time comparison of the derived key).
    pub async fn verify_ui_lock_password(&self, stored: String, password: String) -> QbtResult<bool> {
        self.spawn_on_rt(async move {
            tokio::task::spawn_blocking(move || crate::webui::password::verify_password(&stored, &password))
                .await
                .map_err(|e| QbtError::other(format!("task failed: {e}")))
        })
        .await
    }

    /// Raw favicon bytes (at most 64 KiB, `.ico` then `.png`) for a tracker
    /// URL, fetched through the general-purpose proxy. Errors when none is
    /// available; the front end decodes the image.
    pub async fn fetch_tracker_favicon(&self, tracker_url: String) -> QbtResult<Vec<u8>> {
        let ctx = self.ctx.clone();
        self.spawn_on_rt(async move { crate::net::favicon::fetch_favicon(&ctx, &tracker_url).await }).await
    }
}
