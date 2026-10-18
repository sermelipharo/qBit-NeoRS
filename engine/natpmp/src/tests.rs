use super::*;

async fn fake_gateway(fail_first: bool) -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        let mut dropped = !fail_first;
        loop {
            let (n, from) = sock.recv_from(&mut buf).await.unwrap();
            if !dropped {
                dropped = true; // lose the first packet to exercise retransmission
                continue;
            }
            match (n, buf[1]) {
                (2, 0) => {
                    let mut r = vec![0, 128, 0, 0, 0, 0, 0, 7];
                    r.extend_from_slice(&[203, 0, 113, 9]);
                    sock.send_to(&r, from).await.unwrap();
                }
                (12, op @ (1 | 2)) => {
                    let mut r = vec![0, 128 + op, 0, 0, 0, 0, 0, 7];
                    r.extend_from_slice(&buf[4..6]);
                    r.extend_from_slice(&(u16::from_be_bytes([buf[6], buf[7]]) + 1).to_be_bytes());
                    r.extend_from_slice(&buf[8..12]);
                    sock.send_to(&r, from).await.unwrap();
                }
                _ => {
                    sock.send_to(&[0, 128 + buf[1], 0, 5], from).await.unwrap();
                }
            }
        }
    });
    addr
}

#[tokio::test]
async fn external_address_and_mapping() {
    let gw = fake_gateway(true).await;
    let c = Client::with_gateway_addr(gw).await.unwrap();
    assert_eq!(c.external_address().await.unwrap(), Ipv4Addr::new(203, 0, 113, 9));
    let m = c.map(Protocol::Tcp, 6881, 6881, 3600).await.unwrap();
    assert_eq!(m, Mapping { protocol: Protocol::Tcp, internal_port: 6881, external_port: 6882, lifetime: 3600 });
}

#[tokio::test]
async fn error_codes_and_timeouts() {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        let (_, from) = sock.recv_from(&mut buf).await.unwrap();
        sock.send_to(&[0, 128, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0], from).await.unwrap();
    });
    let c = Client::with_gateway_addr(addr).await.unwrap();
    assert!(matches!(c.external_address().await, Err(NatPmpError::ResultCode(3))));

    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let c = Client::with_gateway_addr(silent.local_addr().unwrap()).await.unwrap();
    let start = std::time::Instant::now();
    assert!(matches!(c.external_address().await, Err(NatPmpError::Timeout)));
    assert!(start.elapsed() >= Duration::from_millis(250 * 31 / 2));
}

#[test]
fn parses_route_output() {
    let s = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n";
    assert_eq!(parse_route_output(s), Some(Ipv4Addr::new(192, 168, 1, 1)));
    assert_eq!(parse_route_output("route: writing to routing socket: not in table"), None);
}
