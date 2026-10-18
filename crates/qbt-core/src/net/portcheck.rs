//! "Test Port" (Settings › Network): asks Transmission's public port checker
//! whether the listening port is reachable from the internet.
//!
//! The service is `https://portcheck.transmissionbt.com/<port>`. It connects
//! back to the address the request came from on `<port>` and answers with a
//! body of `1` (reachable) or `0` (not reachable). The check sends the
//! user's public IP address and the port to that service, so it only ever
//! runs on an explicit click (the UI says so next to the button); nothing
//! calls it automatically.
//!
//! Requests go through the shared HTTP client ([`super::http`]): https with
//! certificate verification, the general-purpose proxy when one is set
//! (the service then tests the proxy's address), at most
//! [`MAX_RESPONSE_SIZE`] bytes and [`TIMEOUT_SECS`] in total.

use super::http::{self, HttpPurpose, HttpRequest};
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};

pub const PORT_CHECK_URL: &str = "https://portcheck.transmissionbt.com/";
/// The answer is a single digit; anything longer is not the port checker.
pub const MAX_RESPONSE_SIZE: u64 = 16;
pub const TIMEOUT_SECS: u64 = 20;

/// Whether `port` is reachable from the internet (`true` = open).
pub async fn check_port(ctx: &CoreContext, port: i64) -> QbtResult<bool> {
    check_port_at(ctx, PORT_CHECK_URL, port).await
}

pub(crate) async fn check_port_at(ctx: &CoreContext, base_url: &str, port: i64) -> QbtResult<bool> {
    let port = u16::try_from(port).ok().filter(|p| *p != 0).ok_or_else(|| QbtError::invalid("invalid port"))?;
    let req = HttpRequest {
        url: format!("{base_url}{port}"),
        cookie: None,
        max_size: MAX_RESPONSE_SIZE,
        timeout_secs: TIMEOUT_SECS,
    };
    let resp = http::fetch(ctx, req, HttpPurpose::General).await?;
    if resp.is_magnet_redirect() || !(200..300).contains(&resp.status) {
        return Err(QbtError::network(format!("The port checker answered with HTTP status {}", resp.status)));
    }
    parse_reply(&resp.body)
}

/// `1` = open, `0` = closed (surrounding whitespace allowed); anything else is an error.
pub fn parse_reply(body: &[u8]) -> QbtResult<bool> {
    match body.trim_ascii() {
        b"1" => Ok(true),
        b"0" => Ok(false),
        _ => Err(QbtError::network("The port checker sent an unexpected answer")),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::net::testutil;

    #[test]
    fn replies_are_parsed_strictly() {
        assert!(parse_reply(b"1").unwrap());
        assert!(!parse_reply(b"0\n").unwrap());
        assert!(parse_reply(b"").is_err());
        assert!(parse_reply(b"10").is_err());
        assert!(parse_reply(b"<html>1</html>").is_err());
    }

    /// Serves `body` with `status` and returns the base URL and the requested path.
    async fn serve(status: &'static str, body: &'static [u8]) -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = [0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap_or(0);
            let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
            let _ = tx.send(line.split(' ').nth(1).unwrap_or("").to_string());
            let head =
                format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(body).await;
        });
        (format!("http://127.0.0.1:{port}/"), rx)
    }

    #[test]
    fn check_asks_for_the_port_and_validates_the_answer() {
        let rt = testutil::runtime();
        let dir = tempfile::tempdir().unwrap();
        let ctx = testutil::context(&rt, dir.path());
        rt.block_on(async {
            let (base, path) = serve("200 OK", b"1").await;
            assert!(check_port_at(&ctx, &base, 51413).await.unwrap());
            assert_eq!(path.await.unwrap(), "/51413");

            let (base, _) = serve("200 OK", b"0").await;
            assert!(!check_port_at(&ctx, &base, 6881).await.unwrap());

            let (base, _) = serve("500 Internal Server Error", b"1").await;
            assert!(check_port_at(&ctx, &base, 6881).await.is_err());

            static BIG: [u8; 4096] = [b'1'; 4096];
            let (base, _) = serve("200 OK", &BIG).await;
            assert!(check_port_at(&ctx, &base, 6881).await.is_err());

            assert!(check_port_at(&ctx, &base, 0).await.is_err());
            assert!(check_port_at(&ctx, &base, 70_000).await.is_err());
        });
    }
}
