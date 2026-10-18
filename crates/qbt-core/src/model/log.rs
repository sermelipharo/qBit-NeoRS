use serde::Serialize;

/// `Log::MsgType` (bit flags in the WebAPI filter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum LogLevel {
    #[default]
    Normal,
    Info,
    Warning,
    Critical,
}

impl LogLevel {
    pub fn as_flag(self) -> i32 {
        match self {
            LogLevel::Normal => 1,
            LogLevel::Info => 2,
            LogLevel::Warning => 4,
            LogLevel::Critical => 8,
        }
    }
}

impl Serialize for LogLevel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(self.as_flag())
    }
}

/// Execution log "General" entry / `/api/v2/log/main` item.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct LogMessage {
    pub id: i64,
    /// Unix milliseconds.
    pub timestamp: i64,
    #[serde(rename = "type")]
    pub level: LogLevel,
    pub message: String,
}

/// Execution log "Blocked IPs" entry / `/api/v2/log/peers` item.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PeerLogEntry {
    pub id: i64,
    pub ip: String,
    pub timestamp: i64,
    pub blocked: bool,
    pub reason: String,
}
