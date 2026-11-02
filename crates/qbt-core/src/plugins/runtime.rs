//! A sandboxed Lua state per plugin, with an instruction/time budget per
//! callback and a memory cap.
//!
//! Like qBittorrent, only the base, string, table, math, utf8 and coroutine
//! libraries are opened, and `collectgarbage`, `dofile`, `getmetatable`,
//! `setmetatable`, `load`, `loadfile`, `raw*`, `warn` and `string.dump` are
//! removed (so no `io`, `os`, `debug`, `package`/`require` or bytecode).
//! Plugins load as text only.
//!
//! `pcall`/`xpcall` are replaced by versions that stop catching errors once
//! the callback is over its budget, so a plugin cannot swallow its own
//! timeout and keep running.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{ChunkMode, Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Value, VmState};

use super::version::PluginVersion;

/// Budgets of one plugin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Wall time per callback (qBittorrent's `LUA_TIMEOUT`: 1 s).
    pub timeout: Duration,
    /// VM instructions per callback.
    pub max_instructions: u64,
    /// Bytes the Lua state may allocate.
    pub memory: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(1), max_instructions: 200_000_000, memory: 16 * 1024 * 1024 }
    }
}

/// The hook runs every this many VM instructions.
const HOOK_INTERVAL: u32 = 1_000;

/// Per-callback counters shared by the hook and the API functions.
#[derive(Default, Debug)]
pub(crate) struct Budget {
    deadline: Cell<Option<Instant>>,
    max_instructions: Cell<u64>,
    instructions: Cell<u64>,
    exhausted: Cell<bool>,
    pub log_lines: Cell<u32>,
    pub log_overflow_reported: Cell<bool>,
    pub execs: Cell<u32>,
    pub mails: Cell<u32>,
}

impl Budget {
    pub fn start(&self, limits: &Limits) {
        self.deadline.set(Some(Instant::now() + limits.timeout));
        self.max_instructions.set(limits.max_instructions);
        self.instructions.set(0);
        self.exhausted.set(false);
        self.log_lines.set(0);
        self.log_overflow_reported.set(false);
        self.execs.set(0);
        self.mails.set(0);
    }

    /// The callback ran out of time or instructions.
    pub fn exhausted(&self) -> bool {
        self.exhausted.get()
    }

    fn tick(&self) -> bool {
        let n = self.instructions.get() + u64::from(HOOK_INTERVAL);
        self.instructions.set(n);
        let over = n > self.max_instructions.get() || self.deadline.get().is_some_and(|d| Instant::now() >= d);
        if over {
            self.exhausted.set(true);
        }
        over
    }
}

/// Message of a Lua error without mlua's decorations and tracebacks.
pub(crate) fn error_message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(s) => s.split("\nstack traceback:").next().unwrap_or_default().to_string(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::MemoryError(_) => "Not enough memory.".to_string(),
        mlua::Error::CallbackError { cause, .. } => error_message(cause),
        mlua::Error::WithContext { cause, .. } => error_message(cause),
        other => other.to_string(),
    }
}

fn is_memory_error(e: &mlua::Error) -> bool {
    match e {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } | mlua::Error::WithContext { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

const REMOVED_GLOBALS: [&str; 12] = [
    "collectgarbage",
    "dofile",
    "getmetatable",
    "setmetatable",
    "load",
    "loadfile",
    "rawequal",
    "rawlen",
    "rawget",
    "rawset",
    "warn",
    "require",
];

/// A new sandboxed state with the budget hook installed.
pub(crate) fn new_state(limits: &Limits, budget: &Rc<Budget>) -> mlua::Result<Lua> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
    let lua = Lua::new_with(libs, LuaOptions::new())?;
    lua.set_memory_limit(limits.memory)?;
    let globals = lua.globals();
    for name in REMOVED_GLOBALS {
        globals.raw_set(name, Value::Nil)?;
    }
    for name in ["io", "os", "debug", "package", "ffi", "jit"] {
        globals.raw_set(name, Value::Nil)?;
    }
    let string: mlua::Table = globals.get("string")?;
    string.raw_set("dump", Value::Nil)?;

    let hook_budget = budget.clone();
    lua.set_global_hook(HookTriggers::new().every_nth_instruction(HOOK_INTERVAL), move |_, _| {
        if hook_budget.tick() { Err(mlua::Error::runtime("Timed out.")) } else { Ok(VmState::Continue) }
    })?;

    let b = budget.clone();
    let pcall = lua.create_function(move |lua, (f, args): (Value, MultiValue)| {
        protected_call(lua, &b, f, args, None)
    })?;
    let b = budget.clone();
    let xpcall = lua.create_function(move |lua, (f, handler, args): (Value, Function, MultiValue)| {
        protected_call(lua, &b, f, args, Some(handler))
    })?;
    globals.raw_set("pcall", pcall)?;
    globals.raw_set("xpcall", xpcall)?;
    Ok(lua)
}

/// `pcall`/`xpcall`: catches errors like Lua's, except when the callback
/// is over its budget or out of memory (then the error goes on up).
fn protected_call(lua: &Lua, budget: &Budget, f: Value, args: MultiValue, handler: Option<Function>) -> mlua::Result<MultiValue> {
    let result = match f {
        Value::Function(f) => f.call::<MultiValue>(args),
        other => Err(mlua::Error::runtime(format!("attempt to call a {} value", other.type_name()))),
    };
    match result {
        Ok(mut values) => {
            values.push_front(Value::Boolean(true));
            Ok(values)
        }
        Err(e) if budget.exhausted() || is_memory_error(&e) => Err(e),
        Err(e) => {
            let mut message = Value::String(lua.create_string(error_message(&e))?);
            if let Some(h) = handler {
                message = h.call::<Value>(message)?;
            }
            Ok(MultiValue::from_vec(vec![Value::Boolean(false), message]))
        }
    }
}

/// Global values read after the plugin's main chunk ran.
#[derive(Clone, Debug)]
pub(crate) struct Metadata {
    pub name: String,
    pub version: PluginVersion,
    pub author: String,
    pub description: String,
}

/// `luabridge::cast<QString>` of a global: strings, and numbers converted
/// like `lua_tolstring`; anything else is missing.
fn global_text(lua: &Lua, name: &str) -> String {
    match lua.globals().get::<Value>(name) {
        Ok(Value::String(s)) => s.to_string_lossy(),
        Ok(Value::Integer(i)) => i.to_string(),
        Ok(Value::Number(n)) => lua.coerce_string(Value::Number(n)).ok().flatten().map(|s| s.to_string_lossy()).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Loads and runs a plugin's main chunk in a new state (`Plugin::load`):
/// text only, within the budget; then reads `NAME` and `VERSION` (and the
/// optional `AUTHOR` and `DESCRIPTION`).
pub(crate) fn load(source: &[u8], chunk_name: &str, limits: &Limits) -> Result<(Lua, Rc<Budget>, Metadata), String> {
    let budget = Rc::new(Budget::default());
    let lua = new_state(limits, &budget).map_err(|_| "Failed to allocate Lua state.".to_string())?;
    let chunk = lua.load(source).set_name(format!("={chunk_name}")).set_mode(ChunkMode::Text);
    budget.start(limits);
    chunk.exec().map_err(|e| error_message(&e))?;
    let name = global_text(&lua, "NAME");
    if name.is_empty() {
        return Err("Plugin name is missing or invalid.".to_string());
    }
    let version =
        PluginVersion::parse(&global_text(&lua, "VERSION")).ok_or_else(|| "Plugin version is missing or invalid.".to_string())?;
    let metadata = Metadata {
        name,
        version,
        author: global_text(&lua, "AUTHOR"),
        description: global_text(&lua, "DESCRIPTION"),
    };
    Ok((lua, budget, metadata))
}

/// Whether the global `name` is a function.
pub(crate) fn has_function(lua: &Lua, name: &str) -> bool {
    matches!(lua.globals().get::<Value>(name), Ok(Value::Function(_)))
}
