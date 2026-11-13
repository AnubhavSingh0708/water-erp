use axum::{routing::get, Router};
use axum_server::tls_rustls::RustlsConfig;
use std::path::PathBuf;
use water_erp::tls::ensure_local_certs;

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("water-erp-tls-test-{}", uuid::Uuid::new_v4()));
    d.join("tls")
}

#[test]
fn generates_reuses_and_renews() {
    let dir = tmpdir();
    let a = ensure_local_certs(&dir, Some("erp.plant.test")).unwrap();
    assert!(a.regenerated);
    assert!(a.ca_pem.contains("BEGIN CERTIFICATE"));
    assert!(a.sans.iter().any(|s| s == "dns:localhost") && a.sans.iter().any(|s| s == "ip:127.0.0.1") && a.sans.iter().any(|s| s == "ip:::1"));
    assert!(a.sans.iter().any(|s| s == "dns:erp.plant.test"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["local-ca.key", "server.key"] {
            assert_eq!(std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
        }
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }
    assert_eq!(a.ca_path, dir.join("local-ca.pem"));
    let ca1 = std::fs::read(dir.join("local-ca.pem")).unwrap();
    // reuse
    let b = ensure_local_certs(&dir, Some("erp.plant.test")).unwrap();
    assert!(!b.regenerated);
    assert_eq!(a.server_cert_pem, b.server_cert_pem);
    // new domain => new server cert, SAME CA (devices keep trusting it)
    let c = ensure_local_certs(&dir, Some("192.168.77.5")).unwrap();
    assert!(c.regenerated && c.sans.iter().any(|s| s == "ip:192.168.77.5"));
    assert_eq!(ca1, std::fs::read(dir.join("local-ca.pem")).unwrap());
    // near expiry => renewed, same CA
    let meta_path = dir.join("server.meta.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    m["server_not_after"] = serde_json::json!(chrono::Utc::now().timestamp() + 3600);
    std::fs::write(&meta_path, serde_json::to_vec(&m).unwrap()).unwrap();
    let d = ensure_local_certs(&dir, Some("192.168.77.5")).unwrap();
    assert!(d.regenerated);
    assert_ne!(c.server_cert_pem, d.server_cert_pem);
    assert_eq!(ca1, std::fs::read(dir.join("local-ca.pem")).unwrap());
    // junk domain is ignored, not fatal
    let e = ensure_local_certs(&dir, Some("bad host name!!")).unwrap();
    assert!(!e.sans.iter().any(|s| s.contains("bad host")));
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[tokio::test]
async fn served_certificate_chains_to_the_local_ca() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = tmpdir();
    let tls = ensure_local_certs(&dir, None).unwrap();
    let cfg = RustlsConfig::from_pem(tls.server_cert_pem.clone(), tls.server_key_pem.clone()).await.unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    let app = Router::new().route("/api/health", get(|| async { "ok" }));
    let handle = axum_server::Handle::new();
    let h2 = handle.clone();
    tokio::spawn(async move { axum_server::from_tcp_rustls(l, cfg).handle(h2).serve(app.into_make_service()).await.unwrap() });
    let ca = reqwest::Certificate::from_pem(tls.ca_pem.as_bytes()).unwrap();
    let trusting = reqwest::Client::builder().no_proxy().add_root_certificate(ca).build().unwrap();
    for host in ["localhost", "127.0.0.1"] {
        let r = trusting.get(format!("https://{host}:{port}/api/health")).send().await.unwrap_or_else(|e| panic!("{host}: {e:?}"));
        assert_eq!(r.text().await.unwrap(), "ok");
    }
    // without the CA the connection is refused
    let strict = reqwest::Client::builder().no_proxy().build().unwrap();
    assert!(strict.get(format!("https://localhost:{port}/api/health")).send().await.is_err());
    handle.shutdown();
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
