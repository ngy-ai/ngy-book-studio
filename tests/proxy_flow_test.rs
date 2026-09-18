//! Transport-level check of the "使用系统代理" switch of the AI settings window.
//!
//! `HTTP_PROXY` is process-wide, so this lives in its own integration target: a
//! test binary owns its process, and pointing the variable at a mock proxy here
//! cannot leak into the other targets or into a running application.

use std::{
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use ngy_book_studio::ai::{OpenAiCompatibleProvider, OpenAiHttpProvider, ProviderConfig};

const MODELS_BODY: &str = r#"{"data":[{"id":"mock-model"}]}"#;

/// HTTP server on a loopback port that records the request line of everything it
/// serves and answers every request with `MODELS_BODY`.
fn recording_server() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&requests);
    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = read_request(&mut stream);
            captured
                .lock()
                .unwrap()
                .push(request.lines().next().unwrap_or_default().to_string());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{MODELS_BODY}",
                MODELS_BODY.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (format!("http://{address}/v1/"), requests, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buffer).to_string()
}

/// Points every proxy variable reqwest reads at the mock proxy and clears the
/// bypass list, so the two clients below differ *only* in their proxy argument.
fn point_proxy_environment_at(proxy_url: &str) {
    // SAFETY: this test target owns its process and this is its only test.
    unsafe {
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::set_var(name, proxy_url);
        }
        for name in ["NO_PROXY", "no_proxy"] {
            std::env::set_var(name, "");
        }
    }
}

fn config(base_url: &str) -> ProviderConfig {
    ProviderConfig {
        base_url: base_url.to_string(),
        api_key: None,
        remote_content_confirmed: false,
        allow_insecure_remote_http: false,
        request_timeout_secs: 10,
    }
}

#[tokio::test]
async fn the_proxy_switch_decides_whether_requests_go_through_the_system_proxy() {
    let (target_url, target_requests, _target) = recording_server();
    let (proxy_url, proxy_requests, _proxy) = recording_server();
    point_proxy_environment_at(&proxy_url);

    // Switched off: the request reaches the endpoint directly, which is what a
    // local model endpoint needs when a system proxy would capture it.
    let direct = OpenAiHttpProvider::new_with_proxy(config(&target_url), false).unwrap();
    direct.models().await.unwrap();
    assert_eq!(target_requests.lock().unwrap().len(), 1);
    assert!(
        proxy_requests.lock().unwrap().is_empty(),
        "关闭代理后请求不得经过系统代理"
    );

    // Switched on: the same endpoint now goes through the proxy, i.e. the
    // default really keeps honouring the environment.
    let through = OpenAiHttpProvider::new_with_proxy(config(&target_url), true).unwrap();
    through.models().await.unwrap();
    assert_eq!(
        proxy_requests.lock().unwrap().len(),
        1,
        "打开代理后请求必须先到系统代理"
    );
    assert_eq!(
        target_requests.lock().unwrap().len(),
        1,
        "打开代理后端点不应被直接访问"
    );
}
