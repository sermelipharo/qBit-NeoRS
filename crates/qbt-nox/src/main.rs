//! Headless daemon (`qbittorrent-nox` equivalent): starts the core with the
//! WebUI forced on, prints how to reach it (and the temporary password when
//! none is configured), and runs until a WebAPI `app/shutdown` request or
//! SIGINT/SIGTERM.

use std::time::{Duration, Instant};

use qbt_core::model::{QBT_COMPAT_VERSION, WEBAPI_VERSION};
use qbt_core::{App, AppConfig};

const NAME: &str = "qbit-neors-nox";

fn usage_text() -> String {
    format!(
        "Usage:\n    {NAME} [options]\n\n\
         Runs qBit NeoRS without a GUI. The WebUI is always enabled.\n\n\
         Options:\n\
         \x20   -v | --version             Display program version and exit\n\
         \x20   -h | --help                Display this help message and exit\n\
         \x20   --webui-port=<port>        Change the WebUI port\n\
         \x20   --profile=<dir>            Store configuration files in <dir>\n\n\
         {NAME} migrate-from-qbittorrent --help    Import a qBittorrent 5.x installation\n\
         {NAME} migrate-from-transmission --help   Import the Transmission app (macOS)\n"
    )
}

fn migrate_usage() -> String {
    format!(
        "Usage:\n    {NAME} migrate-from-qbittorrent --profile <dir> [options]\n\n\
         Builds a qBit NeoRS profile from a qBittorrent 5.x installation (only read; see docs/MIGRATION.md).\n\n\
         Options:\n\
         \x20   --profile <dir>             Target profile (required; the default one is\n\
         \x20                               ~/Library/Application Support/qBit NeoRS)\n\
         \x20   --source-config <dir>       Folder with qBittorrent.ini (default ~/.config/qBittorrent)\n\
         \x20   --source-prefs <dir>        Folder with categories.json, watched_folders.json, rss/\n\
         \x20                               (default ~/Library/Preferences/qBittorrent)\n\
         \x20   --source-data <dir>         qBittorrent's data folder with BT_backup\n\
         \x20                               (default ~/Library/Application Support/qBittorrent)\n\
         \x20   --webui <url>               Read preferences from qBittorrent's running WebUI (GET only)\n\
         \x20   --webui-prefs <file>        Read preferences from a saved app/preferences JSON\n\
         \x20   --dry-run                   Only print the report, write nothing\n\
         \x20   --merge                     Merge into an existing profile (torrents already there only\n\
         \x20                               get statistics, tracker tiers, web seeds, limits, queue position)\n\
         \x20   --force                     Write into a profile that already has data (without --merge)\n\
         \x20   --stop-all                  Add every torrent stopped\n\
         \x20   --skipped-file-data <mode>  recover (default): rebuild pieces shared with skipped files\n\
         \x20                               from libtorrent's part file; unclaim: download them again\n\
         \x20   --sandbox                   Test profile: stopped torrents, no DHT/LSD/PeX/UPnP/RSS/watched\n\
         \x20                               folders, WebUI on 127.0.0.1 (implies --stop-all)\n\
         \x20   --sandbox-listen-port <n>   Listen port of the sandbox (default: a free one)\n\
         \x20   --sandbox-webui-port <n>    WebUI port of the sandbox (default: a free one)\n"
    )
}

fn transmission_usage() -> String {
    format!(
        "Usage:\n    {NAME} migrate-from-transmission --profile <dir> [options]\n\n\
         Builds a qBit NeoRS profile from the Transmission app for macOS (only read; see docs/MIGRATION.md).\n\
         Quit Transmission first: it saves its resume data on exit.\n\n\
         Options:\n\
         \x20   --profile <dir>             Target profile (required; the default one is\n\
         \x20                               ~/Library/Application Support/qBit NeoRS)\n\
         \x20   --source <dir>              Transmission's folder (default ~/Library/Application Support/Transmission)\n\
         \x20   --defaults <file>           Transmission's settings exported with\n\
         \x20                               `defaults export org.m0k.transmission <file>`\n\
         \x20                               (default: read the org.m0k.transmission domain, read only)\n\
         \x20   --dry-run                   Only print the report, write nothing\n\
         \x20   --merge                     Merge into an existing profile (torrents already there only\n\
         \x20                               get statistics, dates, limits, queue position)\n\
         \x20   --force                     Write into a profile that already has data (without --merge)\n\
         \x20   --no-backup                 Don't copy the profile's files to <profile>.before-migration-<time> first\n\
         \x20   --stop-all                  Add every torrent stopped\n\
         \x20   --sandbox                   Test profile: stopped torrents, no DHT/LSD/PeX/UPnP/RSS/watched\n\
         \x20                               folders, WebUI on 127.0.0.1 (implies --stop-all)\n\
         \x20   --sandbox-listen-port <n>   Listen port of the sandbox (default: a free one)\n\
         \x20   --sandbox-webui-port <n>    WebUI port of the sandbox (default: a free one)\n"
    )
}

/// `migrate-from-transmission`; returns the process exit code.
fn migrate_transmission(args: impl IntoIterator<Item = String>) -> i32 {
    use qbt_core::migrate::Sandbox;
    use qbt_core::migrate::transmission::Options;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let mut o = Options::macos_defaults(&home, std::path::PathBuf::new());
    let (mut sandbox, mut lport, mut wport, mut backup) = (false, 0u16, 0u16, true);
    let mut args = args.into_iter();
    let parsed = (|| -> Result<bool, String> {
        while let Some(arg) = args.next() {
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
                _ => (arg.clone(), None),
            };
            let mut value = || inline.clone().or_else(|| args.next()).filter(|v| !v.is_empty()).ok_or(format!("Parameter '{name}' expects a value"));
            match name.as_str() {
                "-h" | "--help" => return Ok(false),
                "--profile" => o.profile = value()?.into(),
                "--source" => o.source_dir = value()?.into(),
                "--defaults" => o.defaults_plist = Some(value()?.into()),
                "--dry-run" => o.dry_run = true,
                "--merge" => o.merge = true,
                "--force" => o.force = true,
                "--no-backup" => backup = false,
                "--stop-all" => o.stop_all = true,
                "--sandbox" => sandbox = true,
                "--sandbox-listen-port" => lport = value()?.parse().map_err(|_| "invalid --sandbox-listen-port".to_string())?,
                "--sandbox-webui-port" => wport = value()?.parse().map_err(|_| "invalid --sandbox-webui-port".to_string())?,
                other => return Err(format!("Unknown option: '{other}'")),
            }
        }
        if o.profile.as_os_str().is_empty() {
            return Err("--profile is required".into());
        }
        Ok(true)
    })();
    match parsed {
        Ok(true) => {}
        Ok(false) => {
            print!("{}", transmission_usage());
            return 0;
        }
        Err(e) => {
            eprintln!("{e}\n\n{}", transmission_usage());
            return 2;
        }
    }
    if sandbox {
        let l = if lport == 0 { free_port() } else { lport };
        let mut w = if wport == 0 { free_port() } else { wport };
        while w == l || w == 9292 {
            w = free_port();
        }
        o.sandbox = Some(Sandbox { listen_port: l, webui_port: w });
        o.stop_all = true;
    }
    if !o.dry_run && backup && (o.merge || o.force) {
        // A dry run first: the backup is only taken when the migration can proceed.
        let mut check = o.clone();
        check.dry_run = true;
        if let Err(e) = qbt_core::migrate::transmission::run(&check) {
            eprintln!("Migration failed: {e}");
            return 1;
        }
        if qbt_core::migrate::profile_in_use(&o.profile).is_some() || qbt_core::migrate::transmission::transmission_running().is_some() {
            // `run` reports why below.
        } else {
            match qbt_core::migrate::backup_profile(&o.profile) {
                Ok(Some(dir)) => println!("Profile files backed up to: {}", dir.display()),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("Migration failed: cannot back up the profile: {e}");
                    return 1;
                }
            }
        }
    }
    match qbt_core::migrate::transmission::run(&o) {
        Ok(report) => {
            print!("{}", report.to_text());
            if report.failed_torrents.is_empty() { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("Migration failed: {e}");
            1
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(0)
}

/// `migrate-from-qbittorrent`; returns the process exit code.
fn migrate(args: impl IntoIterator<Item = String>) -> i32 {
    use qbt_core::migrate::{Options, Sandbox, SkippedData};
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let mut o = Options::macos_defaults(&home, std::path::PathBuf::new());
    let (mut sandbox, mut lport, mut wport) = (false, 0u16, 0u16);
    let mut args = args.into_iter();
    let parsed = (|| -> Result<bool, String> {
        while let Some(arg) = args.next() {
            let (name, inline) = match arg.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
                _ => (arg.clone(), None),
            };
            let mut value = || inline.clone().or_else(|| args.next()).filter(|v| !v.is_empty()).ok_or(format!("Parameter '{name}' expects a value"));
            match name.as_str() {
                "-h" | "--help" => return Ok(false),
                "--profile" => o.profile = value()?.into(),
                "--source-config" => o.source_config = value()?.into(),
                "--source-prefs" => o.source_prefs = value()?.into(),
                "--source-data" => o.source_data = value()?.into(),
                "--webui" => o.webui = Some(value()?),
                "--webui-prefs" => o.webui_prefs_file = Some(value()?.into()),
                "--dry-run" => o.dry_run = true,
                "--merge" => o.merge = true,
                "--force" => o.force = true,
                "--stop-all" => o.stop_all = true,
                "--sandbox" => sandbox = true,
                "--sandbox-listen-port" => lport = value()?.parse().map_err(|_| "invalid --sandbox-listen-port".to_string())?,
                "--sandbox-webui-port" => wport = value()?.parse().map_err(|_| "invalid --sandbox-webui-port".to_string())?,
                "--skipped-file-data" => {
                    o.skipped_data = match value()?.as_str() {
                        "recover" => SkippedData::Recover,
                        "unclaim" => SkippedData::Unclaim,
                        v => return Err(format!("--skipped-file-data must be recover or unclaim, got '{v}'")),
                    }
                }
                other => return Err(format!("Unknown option: '{other}'")),
            }
        }
        if o.profile.as_os_str().is_empty() {
            return Err("--profile is required".into());
        }
        Ok(true)
    })();
    match parsed {
        Ok(true) => {}
        Ok(false) => {
            print!("{}", migrate_usage());
            return 0;
        }
        Err(e) => {
            eprintln!("{e}\n\n{}", migrate_usage());
            return 2;
        }
    }
    if sandbox {
        let l = if lport == 0 { free_port() } else { lport };
        let mut w = if wport == 0 { free_port() } else { wport };
        while w == l || w == 9292 {
            w = free_port();
        }
        o.sandbox = Some(Sandbox { listen_port: l, webui_port: w });
        o.stop_all = true;
    }
    match qbt_core::migrate::run(&o) {
        Ok(report) => {
            print!("{}", report.to_text());
            if report.failed_torrents.is_empty() { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("Migration failed: {e}");
            1
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}\n\n{}", usage_text());
    std::process::exit(2)
}

enum Command {
    Run(AppConfig),
    Help,
    Version,
}

/// Accepts `--opt value` and `--opt=value`.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut config = AppConfig { front_end: "nox".into(), log_to_stderr: true, ..Default::default() };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = |what: &str| inline.clone().or_else(|| args.next()).ok_or(format!("Parameter '{what}' expects a value"));
        match name.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-v" | "--version" => return Ok(Command::Version),
            "--profile" => {
                let dir = value("--profile")?;
                if dir.is_empty() {
                    return Err("Parameter '--profile' expects a value".into());
                }
                config.profile_dir = dir;
            }
            "--webui-port" => {
                let v = value("--webui-port")?;
                config.webui_port_override = v
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or(format!("Parameter '--webui-port' must be a port in [1, 65535], got '{v}'"))?;
            }
            other => return Err(format!("Unknown option: '{other}'")),
        }
    }
    Ok(Command::Run(config))
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("migrate-from-qbittorrent") {
        std::process::exit(migrate(std::env::args().skip(2)));
    }
    if std::env::args().nth(1).as_deref() == Some("migrate-from-transmission") {
        std::process::exit(migrate_transmission(std::env::args().skip(2)));
    }
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(Command::Run(c)) => c,
        Ok(Command::Help) => {
            print!("{}", usage_text());
            return;
        }
        Ok(Command::Version) => {
            println!(
                "qBit NeoRS nox v{} (qBittorrent {QBT_COMPAT_VERSION} compatible, WebAPI {WEBAPI_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
        Err(e) => fail(&e),
    };

    let app = match App::new(config) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("failed to start: {e}");
            std::process::exit(1);
        }
    };

    // Like qbittorrent-nox, the daemon always serves the WebUI.
    let webui = app.webui_controller().clone();
    webui.set_force_enabled(true);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let s = webui.status();
        if s.running || !s.error.is_empty() || Instant::now() > deadline {
            break s;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    println!("\n******** Information ********");
    if !status.error.is_empty() {
        eprintln!(
            "WebUI configuration failed. Reason: {}\nTo fix the error, you may need to edit the config file manually.",
            status.error
        );
    } else {
        println!("To control qBittorrent, access the WebUI at: {}", status.url);
        if !status.temporary_password.is_empty() {
            println!("The WebUI administrator username is: {}", status.username);
            println!(
                "The WebUI administrator password was not set. A temporary password is provided for this session: {}",
                status.temporary_password
            );
            println!("You should set your own password in program preferences.");
        }
    }

    let shutdown_app = app.clone();
    app.runtime().spawn(async move {
        qbt_core::utils::signals::wait_for_termination().await;
        shutdown_app.request_shutdown();
    });

    app.wait_for_shutdown_request();
    app.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<AppConfig, String> {
        match parse_args(args.iter().map(|s| s.to_string()))? {
            Command::Run(c) => Ok(c),
            _ => Err("not run".into()),
        }
    }

    #[test]
    fn arguments() {
        let c = run(&["--profile", "/tmp/p", "--webui-port=9090"]).unwrap();
        assert_eq!(c.profile_dir, "/tmp/p");
        assert_eq!(c.webui_port_override, 9090);
        assert_eq!(c.front_end, "nox");
        let c = run(&["--profile=/x", "--webui-port", "1"]).unwrap();
        assert_eq!((c.profile_dir.as_str(), c.webui_port_override), ("/x", 1));
        assert!(run(&["--webui-port=0"]).is_err());
        assert!(run(&["--webui-port=70000"]).is_err());
        assert!(run(&["--webui-port"]).is_err());
        assert!(run(&["--profile="]).is_err());
        assert!(run(&["--bogus"]).is_err());
        assert!(matches!(parse_args(["-h".to_string()]), Ok(Command::Help)));
        assert!(matches!(parse_args(["--version".to_string()]), Ok(Command::Version)));
    }
}
