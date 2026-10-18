//! Enumerations used by preferences. Integer/string mappings follow the
//! qBittorrent WebAPI exactly (see `as_i32`/`from_i32`, `as_api_str`).

use serde::{Deserialize, Serialize};

macro_rules! int_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $variant:ident = $val:expr),+ $(,)? } default $def:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
        pub enum $name { $($(#[$vm])* $variant),+ }

        impl Default for $name {
            fn default() -> Self { $name::$def }
        }

        impl $name {
            pub fn as_i32(self) -> i32 {
                match self { $($name::$variant => $val),+ }
            }
            /// Out-of-range values map to the default, like qBittorrent's clamping.
            pub fn from_i32(v: i32) -> Self {
                match v { $(x if x == $val => $name::$variant,)+ _ => $name::$def }
            }
        }
    };
}

int_enum!(
    /// `auto_delete_mode` (TorrentFileGuard::AutoDeleteMode).
    AutoDeleteMode { Never = 0, IfAdded = 1, Always = 2 } default Never
);

int_enum!(
    /// `bittorrent_protocol`.
    BtProtocol { Both = 0, Tcp = 1, Utp = 2 } default Both
);

int_enum!(
    /// `encryption`: 0 allow (prefer), 1 require, 2 disable.
    EncryptionMode { Allow = 0, Require = 1, Disable = 2 } default Allow
);

int_enum!(
    /// `scheduler_days`.
    SchedulerDays {
        EveryDay = 0, Weekday = 1, Weekend = 2,
        Monday = 3, Tuesday = 4, Wednesday = 5, Thursday = 6, Friday = 7, Saturday = 8, Sunday = 9
    } default EveryDay
);

int_enum!(
    /// `dyndns_service`.
    DynDnsService { DynDns = 0, NoIp = 1 } default DynDns
);

int_enum!(
    /// `file_log_age_type`.
    FileLogAgeType { Days = 0, Months = 1, Years = 2 } default Months
);

int_enum!(
    /// `disk_io_type` (only `Default` and `Posix` are meaningful in this engine).
    DiskIoType { Default = 0, MMap = 1, Posix = 2, SimplePreadPwrite = 3 } default Default
);

int_enum!(
    /// `disk_io_read_mode`.
    DiskIoReadMode { DisableOsCache = 0, EnableOsCache = 1 } default EnableOsCache
);

int_enum!(
    /// `disk_io_write_mode`.
    DiskIoWriteMode { DisableOsCache = 0, EnableOsCache = 1, WriteThrough = 2 } default EnableOsCache
);

int_enum!(
    /// `utp_tcp_mixed_mode`.
    UtpMixedMode { PreferTcp = 0, PeerProportional = 1 } default PreferTcp
);

int_enum!(
    /// `upload_slots_behavior` (choking algorithm).
    UploadSlotsBehavior { FixedSlots = 0, RateBased = 1 } default FixedSlots
);

int_enum!(
    /// `upload_choking_algorithm` (seed choking algorithm).
    UploadChokingAlgorithm { RoundRobin = 0, FastestUpload = 1, AntiLeech = 2 } default FastestUpload
);

/// `proxy_type`; WebAPI string names, stored ints None=0 HTTP=1 SOCKS5=2 SOCKS4=5.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ProxyType {
    #[default]
    None,
    Http,
    Socks5,
    Socks4,
}

impl ProxyType {
    pub fn as_api_str(self) -> &'static str {
        match self {
            ProxyType::None => "None",
            ProxyType::Http => "HTTP",
            ProxyType::Socks5 => "SOCKS5",
            ProxyType::Socks4 => "SOCKS4",
        }
    }

    pub fn from_api(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::String(s) => match s.as_str() {
                "HTTP" => ProxyType::Http,
                "SOCKS5" => ProxyType::Socks5,
                "SOCKS4" => ProxyType::Socks4,
                _ => ProxyType::None,
            },
            // Legacy integer form.
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(1) | Some(3) => ProxyType::Http,
                Some(2) | Some(4) => ProxyType::Socks5,
                Some(5) => ProxyType::Socks4,
                _ => ProxyType::None,
            },
            _ => ProxyType::None,
        }
    }
}

/// `resume_data_storage_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ResumeDataStorageType {
    /// "Fastresume files" (one file per torrent in BT_backup).
    #[default]
    Legacy,
    /// "SQLite database (experimental)".
    SQLite,
}

/// `torrent_content_remove_option`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ContentRemoveOption {
    #[default]
    Delete,
    MoveToTrash,
}
