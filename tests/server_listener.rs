//! Listener hardening: keep-alive probing on accepted sockets (zombie connections / SSE streams of phones that left the Wi-Fi are reaped by
//! the kernel) and the 15 s SSE ping.
use std::net::SocketAddr;
use water_erp::server::{bind_listener, new_listen_socket};

#[tokio::test]
async fn listen_socket_has_keepalive_enabled_and_accepts_connections() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let sock = new_listen_socket(addr).unwrap();
    assert!(sock.keepalive().unwrap(), "SO_KEEPALIVE must be on so the host's tcp_keepalive_* settings apply to accepted connections");
    drop(sock);
    let l = bind_listener(addr).unwrap();
    let local = l.local_addr().unwrap();
    let client = tokio::spawn(async move { tokio::net::TcpStream::connect(local).await.unwrap() });
    let (_conn, _) = l.accept().await.unwrap();
    let _ = client.await.unwrap();
}

#[tokio::test]
async fn binding_a_busy_port_is_an_error_not_a_panic() {
    let l = bind_listener("127.0.0.1:0".parse().unwrap()).unwrap();
    let busy = l.local_addr().unwrap();
    assert!(bind_listener(busy).is_err());
}

#[test]
fn sse_ping_is_frequent_enough_to_surface_dead_peers() {
    assert_eq!(water_erp::events::PING_SECS, 15);
}
