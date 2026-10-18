use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

async fn fake_http_proxy(reply: &'static str) -> (u16, tokio::task::JoinHandle<String>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut buf = vec![0u8; 2048];
        let n = s.read(&mut buf).await.unwrap();
        s.write_all(reply.as_bytes()).await.unwrap();
        s.write_all(b"tunnel-data").await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    (port, h)
}

fn cfg(kind: ProxyKind, port: u16, auth: bool) -> ProxyConfig {
    ProxyConfig {
        kind,
        host: "127.0.0.1".into(),
        port,
        auth: auth.then(|| ("user".to_string(), "pa:ss".to_string())),
        remote_dns: true,
    }
}

#[test]
fn debug_output_hides_the_password() {
    let s = format!("{:?}", cfg(ProxyKind::Socks5, 1, true));
    assert!(s.contains("user") && !s.contains("pa:ss"), "{s}");
}

#[tokio::test]
async fn http_connect_with_auth() {
    let (port, req) = fake_http_proxy("HTTP/1.1 200 Connection established\r\nX: y\r\n\r\n").await;
    let target = Target::Host("example.org".into(), 6881);
    let mut s = connect(&cfg(ProxyKind::Http, port, true), target, Duration::from_secs(5)).await.unwrap();
    let mut got = [0u8; 11];
    s.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"tunnel-data");
    let req = req.await.unwrap();
    assert!(req.starts_with("CONNECT example.org:6881 HTTP/1.1\r\n"));
    assert!(req.contains("Proxy-Authorization: Basic dXNlcjpwYTpzcw=="));
}

#[tokio::test]
async fn http_connect_refused_and_garbage() {
    let (port, _) = fake_http_proxy("HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
    let r = connect(&cfg(ProxyKind::Http, port, false), Target::Addr("1.2.3.4:5".parse().unwrap()), Duration::from_secs(5)).await;
    assert!(matches!(r, Err(ProxyError::Refused(m)) if m.contains("407")));

    let (port, _) = fake_http_proxy("SSH-2.0-OpenSSH\r\n\r\n").await;
    let r = connect(&cfg(ProxyKind::Http, port, false), Target::Addr("1.2.3.4:5".parse().unwrap()), Duration::from_secs(5)).await;
    assert!(matches!(r, Err(ProxyError::BadResponse)));
}

#[tokio::test]
async fn ipv6_target_formatting_and_socks4_limits() {
    assert_eq!(Target::Addr("[::1]:80".parse().unwrap()).host_port(), "[::1]:80");
    let r = connect(&cfg(ProxyKind::Socks4, 1, false), Target::Addr("[::1]:80".parse().unwrap()), Duration::from_secs(1)).await;
    assert!(matches!(r, Err(ProxyError::Unsupported)));
}

#[tokio::test]
async fn timeout_is_enforced() {
    // A listener that never answers.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let _keep = tokio::spawn(async move {
        let (_s, _) = l.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
    let r = connect(&cfg(ProxyKind::Http, port, false), Target::Addr("1.2.3.4:5".parse().unwrap()), Duration::from_millis(300)).await;
    assert!(matches!(r, Err(ProxyError::Timeout)));
}
