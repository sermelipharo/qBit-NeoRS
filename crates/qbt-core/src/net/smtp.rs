//! E-mail notification on download completion (Downloads page; qBittorrent
//! `Net::Smtp` and `Application::sendNotificationEmail`).
//!
//! Connection security: with "This server requires a secure connection
//! (SSL)" the client uses implicit TLS on port 465 (the default) and
//! mandatory STARTTLS on any other port; without it, STARTTLS is used when
//! the server offers it. Certificates are always validated. Authentication
//! (PLAIN or LOGIN) is used when enabled and both username and password are
//! set. Addresses and subjects containing line breaks are refused (no header
//! injection); passwords never appear in logs or errors.

use std::str::FromStr;
use std::time::Duration;

use lettre::message::header::{ContentTransferEncoding, ContentType};
use lettre::message::{Body, Mailbox};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::prefs::DownloadsPrefs;

const APP_NAME: &str = "qBit NeoRS";
const DEFAULT_PORT: u16 = 25;
const DEFAULT_PORT_SSL: u16 = 465;
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolved `mail_notification_*` settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailSettings {
    pub sender: Address,
    pub recipients: Vec<Address>,
    pub host: String,
    pub port: u16,
    pub ssl: bool,
    /// (username, password) when authentication is enabled and complete.
    pub credentials: Option<(String, String)>,
}

fn has_line_break(s: &str) -> bool {
    s.contains(['\r', '\n', '\0'])
}

fn parse_address(field: &str, value: &str) -> QbtResult<Address> {
    let v = value.trim();
    if has_line_break(v) {
        return Err(QbtError::invalid(format!("the {field} address contains a line break")));
    }
    Address::from_str(v).map_err(|_| QbtError::invalid(format!("invalid {field} address: \"{v}\"")))
}

/// `host` or `host:port` (IPv6 as `[addr]:port`).
fn parse_server(server: &str, ssl: bool) -> QbtResult<(String, u16)> {
    let s = server.trim();
    let invalid = || QbtError::invalid(format!("invalid SMTP server: \"{s}\""));
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '@') {
        return Err(invalid());
    }
    let default_port = if ssl { DEFAULT_PORT_SSL } else { DEFAULT_PORT };
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']').ok_or_else(invalid)?;
        match tail.strip_prefix(':') {
            Some(p) => (h, Some(p)),
            None if tail.is_empty() => (h, None),
            None => return Err(invalid()),
        }
    } else {
        match s.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        }
    };
    if host.is_empty() {
        return Err(invalid());
    }
    let port = match port {
        // Like qBittorrent: an unparsable port falls back to the default.
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).unwrap_or(default_port),
        None => default_port,
    };
    Ok((host.to_string(), port))
}

/// Reads and validates the e-mail settings.
pub fn settings_from_prefs(d: &DownloadsPrefs) -> QbtResult<MailSettings> {
    let sender = parse_address("sender", &d.mail_notification_sender)?;
    let recipients = d
        .mail_notification_email
        .split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|r| parse_address("recipient", r))
        .collect::<QbtResult<Vec<_>>>()?;
    if recipients.is_empty() {
        return Err(QbtError::invalid("no e-mail recipient is configured"));
    }
    let (host, port) = parse_server(&d.mail_notification_smtp, d.mail_notification_ssl_enabled)?;
    let credentials = (d.mail_notification_auth_enabled
        && !d.mail_notification_username.is_empty()
        && !d.mail_notification_password.is_empty())
    .then(|| (d.mail_notification_username.clone(), d.mail_notification_password.clone()));
    Ok(MailSettings { sender, recipients, host, port, ssl: d.mail_notification_ssl_enabled, credentials })
}

/// Builds the message: `From: qBit NeoRS <sender>`, UTF-8 plain text,
/// base64 body (like qBittorrent).
pub fn build_message(settings: &MailSettings, subject: &str, body: &str) -> QbtResult<Message> {
    if has_line_break(subject) {
        return Err(QbtError::invalid("the e-mail subject contains a line break"));
    }
    let mut builder = Message::builder()
        .from(Mailbox::new(Some(APP_NAME.to_string()), settings.sender.clone()))
        .subject(subject)
        .date_now()
        .header(ContentType::TEXT_PLAIN);
    for r in &settings.recipients {
        builder = builder.to(Mailbox::new(None, r.clone()));
    }
    let body = Body::new_with_encoding(body.to_string(), ContentTransferEncoding::Base64)
        .map_err(|_| QbtError::invalid("cannot encode the e-mail body"))?;
    builder.body(body).map_err(|e| QbtError::invalid(format!("cannot build the e-mail: {e}")))
}

fn transport(settings: &MailSettings) -> QbtResult<AsyncSmtpTransport<Tokio1Executor>> {
    let tls_params = || {
        TlsParameters::new(settings.host.clone()).map_err(|e| QbtError::network(format!("TLS setup failed: {e}")))
    };
    let tls = if settings.ssl {
        if settings.port == DEFAULT_PORT_SSL { Tls::Wrapper(tls_params()?) } else { Tls::Required(tls_params()?) }
    } else {
        Tls::Opportunistic(tls_params()?)
    };
    let mut b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(settings.host.as_str())
        .port(settings.port)
        .tls(tls)
        .timeout(Some(SMTP_TIMEOUT));
    if let Some((user, pass)) = &settings.credentials {
        b = b
            .credentials(Credentials::new(user.clone(), pass.clone()))
            .authentication(vec![Mechanism::Plain, Mechanism::Login]);
    }
    Ok(b.build())
}

/// Sends a mail with the configured `mail_notification_*` settings (does
/// not check whether notifications are enabled). Failures are logged as
/// `Email Notification Error: ...` and returned.
pub async fn send_mail(ctx: &CoreContext, subject: &str, body: &str) -> QbtResult<()> {
    let prefs = ctx.prefs.current();
    let result = async {
        let settings = settings_from_prefs(&prefs.downloads)?;
        let message = build_message(&settings, subject, body)?;
        let transport = transport(&settings)?;
        match tokio::time::timeout(SMTP_TIMEOUT * 3, transport.send(message)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(QbtError::network(e.to_string())),
            Err(_) => Err(QbtError::network("the SMTP server did not answer in time")),
        }
    }
    .await;
    if let Err(e) = &result {
        let msg = match e {
            QbtError::Network { message } | QbtError::InvalidArgument { message } => message.clone(),
            other => other.to_string(),
        };
        ctx.logger.warning(format!("Email Notification Error: {msg}"));
    }
    result
}

/// "Send test email": subject `Test email`. Fails when notifications are
/// disabled (the WebAPI ignores the result and always answers 200).
pub async fn send_test_email(ctx: &CoreContext) -> QbtResult<()> {
    if !ctx.prefs.current().downloads.mail_notification_enabled {
        return Err(QbtError::invalid("e-mail notification is disabled"));
    }
    let body = format!("This is a test email.\nThank you for using {APP_NAME}.\n");
    send_mail(ctx, "Test email", &body).await
}

/// Subject and body of the completion e-mail (qBittorrent's format).
/// Control characters in the name are replaced so a crafted torrent name
/// cannot break the subject header.
pub fn completion_mail(name: &str, wanted_size: i64, save_path: &str, active_time_secs: i64) -> (String, String) {
    let safe_name: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let subject = format!("Torrent \"{safe_name}\" has finished downloading");
    let body = format!(
        "Torrent name: {name}\nTorrent size: {}\nSave path: {save_path}\n\nThe torrent was downloaded in {}.\n\n\nThank you for using {APP_NAME}.\n",
        crate::utils::misc::friendly_unit(wanted_size, false),
        crate::utils::misc::user_friendly_duration(active_time_secs, -1),
    );
    (subject, body)
}

/// Called by the session when a torrent finishes: sends the completion
/// mail when enabled (logs `Torrent: %1, sending mail notification`).
pub async fn notify_torrent_finished(
    ctx: &CoreContext,
    name: &str,
    wanted_size: i64,
    save_path: &str,
    active_time_secs: i64,
) -> QbtResult<()> {
    if !ctx.prefs.current().downloads.mail_notification_enabled {
        return Ok(());
    }
    ctx.logger.info(format!("Torrent: {name}, sending mail notification"));
    let (subject, body) = completion_mail(name, wanted_size, save_path, active_time_secs);
    send_mail(ctx, &subject, &body).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;
    use crate::net::testutil;
    use crate::prefs::Preferences;

    fn prefs(server: &str) -> DownloadsPrefs {
        DownloadsPrefs {
            mail_notification_enabled: true,
            mail_notification_sender: "from@example.com".into(),
            mail_notification_email: "to@example.com".into(),
            mail_notification_smtp: server.into(),
            ..Default::default()
        }
    }

    #[test]
    fn parses_settings() {
        let s = settings_from_prefs(&prefs("smtp.example.com")).unwrap();
        assert_eq!((s.host.as_str(), s.port), ("smtp.example.com", 25));
        let mut p = prefs("smtp.example.com:587");
        p.mail_notification_ssl_enabled = true;
        p.mail_notification_auth_enabled = true;
        p.mail_notification_username = "u".into();
        p.mail_notification_password = "secret".into();
        p.mail_notification_email = "a@x.org, b@y.org".into();
        let s = settings_from_prefs(&p).unwrap();
        assert_eq!(s.port, 587);
        assert_eq!(s.recipients.len(), 2);
        assert_eq!(s.credentials, Some(("u".into(), "secret".into())));
        p.mail_notification_smtp = "mail.example.com".into();
        assert_eq!(settings_from_prefs(&p).unwrap().port, 465);
        p.mail_notification_smtp = "[::1]:2525".into();
        assert_eq!(settings_from_prefs(&p).unwrap().host, "::1");
        // Auth needs both username and password.
        p.mail_notification_password.clear();
        assert_eq!(settings_from_prefs(&p).unwrap().credentials, None);

        // Header injection through configured addresses is refused.
        let mut bad = prefs("smtp.example.com");
        bad.mail_notification_email = "to@example.com\r\nBcc: victim@example.com".into();
        assert!(settings_from_prefs(&bad).is_err());
        let mut bad = prefs("smtp.example.com");
        bad.mail_notification_sender = "from@example.com\nX: y".into();
        assert!(settings_from_prefs(&bad).is_err());
        assert!(settings_from_prefs(&prefs("bad host")).is_err());
        let mut none = prefs("smtp.example.com");
        none.mail_notification_email = " ".into();
        assert!(settings_from_prefs(&none).is_err());
    }

    #[test]
    fn formats_completion_mail() {
        let (subject, body) = completion_mail("Some\nTorrent", 1536 * 1024 * 1024, "/Downloads", 3720);
        assert_eq!(subject, "Torrent \"Some Torrent\" has finished downloading");
        assert_eq!(
            body,
            "Torrent name: Some\nTorrent\nTorrent size: 1.50\u{a0}GiB\nSave path: /Downloads\n\nThe torrent was downloaded in 1h 2m.\n\n\nThank you for using qBit NeoRS.\n"
        );

        let s = settings_from_prefs(&prefs("smtp.example.com")).unwrap();
        let msg = build_message(&s, &subject, &body).unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("From: \"qBit NeoRS\" <from@example.com>") || raw.contains("From: qBit NeoRS <from@example.com>"), "{raw}");
        assert!(raw.contains("To: to@example.com"), "{raw}");
        assert!(raw.contains("Subject: Torrent \"Some Torrent\" has finished downloading"), "{raw}");
        assert!(raw.contains("Content-Type: text/plain; charset=utf-8"), "{raw}");
        assert!(raw.contains("Content-Transfer-Encoding: base64"), "{raw}");
        assert!(raw.contains("Date: "));
        assert!(build_message(&s, "a\r\nBcc: x@y.z", "body").is_err());
    }

    /// Plain-text fake SMTP server; returns the transcript of client lines.
    async fn fake_smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let log2 = log.clone();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            w.write_all(b"220 fake ESMTP\r\n").await.unwrap();
            let mut in_data = false;
            while let Ok(Some(line)) = lines.next_line().await {
                log2.lock().push(line.clone());
                let reply: &[u8] = if in_data {
                    if line == "." {
                        in_data = false;
                        b"250 queued\r\n"
                    } else {
                        continue;
                    }
                } else {
                    let upper = line.to_ascii_uppercase();
                    if upper.starts_with("EHLO") {
                        b"250-fake\r\n250 AUTH PLAIN LOGIN\r\n"
                    } else if upper.starts_with("AUTH") {
                        b"235 ok\r\n"
                    } else if upper.starts_with("DATA") {
                        in_data = true;
                        b"354 go\r\n"
                    } else if upper.starts_with("QUIT") {
                        let _ = w.write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        b"250 ok\r\n"
                    }
                };
                w.write_all(reply).await.unwrap();
            }
        });
        (port, log)
    }

    #[test]
    fn sends_through_local_server() {
        let rt = testutil::runtime();
        let dir = tempfile::tempdir().unwrap();
        let (port, log) = rt.block_on(fake_smtp());
        let mut p = Preferences { downloads: prefs(&format!("127.0.0.1:{port}")), ..Default::default() };
        p.downloads.mail_notification_auth_enabled = true;
        p.downloads.mail_notification_username = "user".into();
        p.downloads.mail_notification_password = "pa55".into();
        let ctx = testutil::context_with(&rt, dir.path(), p);
        rt.block_on(send_test_email(&ctx)).unwrap();
        let log = log.lock().clone();
        assert!(log.iter().any(|l| l.starts_with("MAIL FROM:<from@example.com>")), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with("RCPT TO:<to@example.com>")), "{log:?}");
        assert!(log.iter().any(|l| l.starts_with("AUTH ")), "{log:?}");
        assert!(log.iter().any(|l| l == "Subject: Test email"), "{log:?}");

        // Disabled notifications: the test mail is refused; failures are logged
        // without the password.
        let mut p = Preferences { downloads: prefs("127.0.0.1:1"), ..Default::default() };
        p.downloads.mail_notification_password = "pa55".into();
        let ctx = testutil::context_with(&rt, dir.path(), p.clone());
        assert!(rt.block_on(send_mail(&ctx, "s", "b")).is_err());
        let logged = ctx.logger.messages(-1, &[]);
        assert!(logged.iter().any(|m| m.message.starts_with("Email Notification Error:")));
        assert!(logged.iter().all(|m| !m.message.contains("pa55")));
        p.downloads.mail_notification_enabled = false;
        let ctx = testutil::context_with(&rt, dir.path(), p);
        assert!(rt.block_on(send_test_email(&ctx)).is_err());
    }
}
