//! Links `qbt-core` (with its UniFFI scaffolding) into a static library for the
//! macOS app. Everything exported lives in `qbt-core`; this crate only exists
//! so the app links one archive.
pub use qbt_core;
