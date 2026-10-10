//! The default HTTPS fetcher, exercised offline against local sockets only.
#![cfg(feature = "modules")]

use lmrust::modules::{
    cache_root, find_on_path, Channel, FetchError, Fetcher, HttpRequest, HttpResponse, InstallAction, ModuleError,
    ModuleErrorCode, UreqFetcher,
};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn request(url: String, timeout_ms: u64) -> HttpRequest {
    HttpRequest {
        url,
        headers: vec![("If-None-Match".into(), "\"x\"".into())],
        timeout: Duration::from_millis(timeout_ms),
    }
}

#[test]
fn refuses_plain_http_before_any_io() {
    let err = UreqFetcher
        .fetch(&request("http://127.0.0.1:9/x".into(), 1000))
        .unwrap_err();
    assert!(
        matches!(err, FetchError::Network(ref m) if m.contains("non-HTTPS")),
        "{err}"
    );
}

#[test]
fn connection_refused_is_a_network_error() {
    // Bind then drop: nothing listens on the port any more.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let err = UreqFetcher
        .fetch(&request(format!("https://127.0.0.1:{port}/x"), 2000))
        .unwrap_err();
    assert!(matches!(err, FetchError::Network(_)), "{err:?}");
    assert!(!err.to_string().is_empty());
}

#[test]
fn a_silent_server_times_out() {
    // Accepts the TCP connection but never answers the TLS handshake.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let holder = std::thread::spawn(move || {
        let conn = listener.accept();
        std::thread::sleep(Duration::from_millis(1500));
        drop(conn);
    });
    let started = Instant::now();
    let err = UreqFetcher
        .fetch(&request(format!("https://127.0.0.1:{port}/x"), 300))
        .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(1400),
        "deadline not honoured: {:?}",
        started.elapsed()
    );
    assert!(matches!(err, FetchError::Timeout(_)), "{err:?}");
    holder.join().unwrap();
}

#[test]
fn small_helpers() {
    assert!(cache_root().ends_with("modules"));
    assert!(find_on_path("sh", None).is_some() || cfg!(not(unix)));
    assert_eq!(Channel::Dev.to_string(), "dev");
    assert_eq!(InstallAction::Unchanged.as_str(), "unchanged");
    let codes = [
        ModuleErrorCode::NotInstalled,
        ModuleErrorCode::CacheCorrupt,
        ModuleErrorCode::InvalidSchema,
        ModuleErrorCode::BadJson,
        ModuleErrorCode::OutputLimit,
        ModuleErrorCode::FetchFailed,
        ModuleErrorCode::PathTraversal,
        ModuleErrorCode::AtomicWriteFailed,
    ];
    let names: Vec<String> = codes.iter().map(|c| c.to_string()).collect();
    assert_eq!(
        names,
        [
            "NOT_INSTALLED",
            "CACHE_CORRUPT",
            "INVALID_SCHEMA",
            "BAD_JSON",
            "OUTPUT_LIMIT",
            "FETCH_FAILED",
            "PATH_TRAVERSAL",
            "ATOMIC_WRITE_FAILED"
        ]
    );
    assert!(ModuleErrorCode::BadJson == "BAD_JSON" && "BAD_JSON" == ModuleErrorCode::BadJson);
    let e = ModuleError::new(ModuleErrorCode::FetchFailed, "boom");
    assert_eq!(e.to_string(), "FETCH_FAILED: boom");
    assert_eq!(FetchError::Timeout("t".into()).to_string(), "t");
    let resp = HttpResponse {
        status: 204,
        status_text: String::new(),
        headers: vec![],
        body: Box::new(std::io::empty()),
    };
    assert!(resp.ok());
    assert!(format!("{resp:?}").contains("204"));
}
