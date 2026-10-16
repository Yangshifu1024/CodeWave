use super::*;
use crate::core::config::ProxyConfig;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

fn request(url: &str, method: Method, body: Option<Value>) -> HttpRequest {
    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer fixture-only".parse().unwrap());
    headers.insert("x-custom-secret", "fixture-only".parse().unwrap());
    headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
    if body.is_some() {
        headers.insert(CONTENT_LENGTH, "7".parse().unwrap());
    }
    headers.insert(CONTENT_ENCODING, "identity".parse().unwrap());
    HttpRequest {
        url: Url::parse(url).unwrap(),
        method,
        headers,
        body,
    }
}

fn local_config() -> ConfigState {
    ConfigState {
        proxy: Some(ProxyConfig {
            mode: ProxyMode::None,
            url: String::new(),
        }),
        network: crate::core::config::NetworkConfig {
            allow_private_network: true,
        },
        ..ConfigState::default()
    }
}

#[test]
fn redirects_apply_method_rules_and_remove_discarded_body_headers() {
    for (status, method, expected) in [
        (StatusCode::SEE_OTHER, Method::HEAD, Method::HEAD),
        (StatusCode::SEE_OTHER, Method::PUT, Method::GET),
        (StatusCode::MOVED_PERMANENTLY, Method::POST, Method::GET),
        (StatusCode::FOUND, Method::POST, Method::GET),
    ] {
        let mut req = request("https://fixture.invalid/a", method, Some(json!({"a": 1})));
        redirect(
            &mut req,
            status,
            Url::parse("https://fixture.invalid/b").unwrap(),
        )
        .unwrap();
        assert_eq!(req.method, expected);
        assert!(req.body.is_none());
        for name in [CONTENT_TYPE, CONTENT_LENGTH, CONTENT_ENCODING] {
            assert!(!req.headers.contains_key(name));
        }
        assert!(req.headers.contains_key("authorization"));
        assert_eq!(req.url.path(), "/b");
    }
}

#[test]
fn preserving_redirects_retain_same_origin_requests_but_reject_cross_origin_replay() {
    for status in [
        StatusCode::TEMPORARY_REDIRECT,
        StatusCode::PERMANENT_REDIRECT,
    ] {
        let mut req = request(
            "https://fixture.invalid/a",
            Method::POST,
            Some(json!({"a": 1})),
        );
        let headers = req.headers.clone();
        redirect(
            &mut req,
            status,
            Url::parse("https://fixture.invalid/b").unwrap(),
        )
        .unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.body, Some(json!({"a": 1})));
        assert_eq!(req.headers, headers);
        for next in ["https://other.invalid/b", "https://fixture.invalid:444/b"] {
            let err = redirect(&mut req, status, Url::parse(next).unwrap()).unwrap_err();
            assert_eq!(err.code, "E_REDIRECT_UNSAFE");
            assert_eq!(req.url.as_str(), "https://fixture.invalid/b");
        }
    }
    // 301/302 preserve non-POST methods and must not replay their body cross-origin.
    for status in [StatusCode::MOVED_PERMANENTLY, StatusCode::FOUND] {
        let mut req = request(
            "https://fixture.invalid/a",
            Method::PUT,
            Some(json!({"a": 1})),
        );
        assert_eq!(
            redirect(
                &mut req,
                status,
                Url::parse("https://other.invalid/").unwrap()
            )
            .unwrap_err()
            .code,
            "E_REDIRECT_UNSAFE"
        );
    }
}

#[test]
fn cross_origin_get_strips_all_caller_headers_and_https_never_downgrades() {
    let mut req = request(
        "https://fixture.invalid/a",
        Method::POST,
        Some(json!({"a": 1})),
    );
    redirect(
        &mut req,
        StatusCode::FOUND,
        Url::parse("https://other.invalid/b").unwrap(),
    )
    .unwrap();
    assert_eq!(req.method, Method::GET);
    assert!(req.body.is_none());
    assert!(req.headers.is_empty());
    for status in [
        StatusCode::FOUND,
        StatusCode::SEE_OTHER,
        StatusCode::TEMPORARY_REDIRECT,
    ] {
        assert_eq!(
            redirect(
                &mut req,
                status,
                Url::parse("http://other.invalid/c").unwrap()
            )
            .unwrap_err()
            .code,
            "E_REDIRECT_UNSAFE"
        );
        assert_eq!(req.url.scheme(), "https");
    }
}

#[tokio::test]
async fn checked_addresses_rejects_any_private_answer_including_mapped_ipv6() {
    let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
    let public_v6: SocketAddr = "[2606:4700::1111]:443".parse().unwrap();
    assert_eq!(
        checked_addresses(vec![public, public_v6], false).unwrap(),
        vec![public, public_v6]
    );
    for private in [
        "127.0.0.1:443",
        "10.0.0.1:443",
        "[::1]:443",
        "[fd12::1]:443",
        "[::ffff:127.0.0.1]:443",
    ] {
        let private: SocketAddr = private.parse().unwrap();
        for answers in [vec![public, private], vec![private, public_v6]] {
            assert_eq!(
                checked_addresses(answers.clone(), false).unwrap_err().code,
                "E_SSRF_BLOCKED"
            );
            assert_eq!(checked_addresses(answers.clone(), true).unwrap(), answers);
        }
    }
    assert_eq!(checked_addresses(vec![], true).unwrap_err().code, "E_DNS");
    for target in ["http://localhost/", "http://[::1]/", "http://10.0.0.1/"] {
        let url = Url::parse(target).unwrap();
        assert_eq!(
            resolve(&url, false).await.unwrap_err().code,
            "E_SSRF_BLOCKED"
        );
        assert!(!resolve(&url, true).await.unwrap().is_empty());
    }
}

#[test]
fn proxy_routes_refuse_remote_dns_without_silently_bypassing_proxy() {
    let url = Url::parse("https://fixture.invalid/").unwrap();
    let addresses = ["8.8.8.8:443".parse().unwrap()];
    for proxy_url in [
        "http://127.0.0.1:1080",
        "https://127.0.0.1:1080",
        "socks5h://127.0.0.1:1080",
        "socks4a://127.0.0.1:1080",
    ] {
        let matcher = Matcher::builder().all(proxy_url).build();
        let proxy = matcher
            .intercept(&url.as_str().parse().unwrap())
            .expect("configured route must exist");
        assert_eq!(
            pinned_client(&url, &addresses, Some(proxy.clone()), false)
                .err()
                .unwrap()
                .code,
            "E_SSRF_PROXY_UNSUPPORTED"
        );
        assert!(pinned_client(&url, &addresses, Some(proxy), true).is_ok());
    }
    for proxy_url in ["socks4://127.0.0.1:1080", "socks5://127.0.0.1:1080"] {
        let matcher = Matcher::builder().all(proxy_url).build();
        assert!(
            pinned_client(
                &url,
                &addresses,
                matcher.intercept(&url.as_str().parse().unwrap()),
                false
            )
            .is_ok()
        );
    }
    let literal = Url::parse("https://8.8.8.8/").unwrap();
    let matcher = Matcher::builder().all("http://127.0.0.1:1080").build();
    assert!(
        pinned_client(
            &literal,
            &addresses,
            matcher.intercept(&literal.as_str().parse().unwrap()),
            false
        )
        .is_ok()
    );
}

#[tokio::test]
async fn explicit_proxy_and_no_proxy_routes_do_not_depend_on_process_environment() {
    let matcher = Matcher::builder()
        .all("http://127.0.0.1:1080")
        .no("fixture.invalid,127.0.0.1,::1")
        .build();
    for url in [
        "https://fixture.invalid/",
        "https://sub.fixture.invalid/",
        "http://127.0.0.1/",
        "http://[::1]/",
    ] {
        assert!(matcher.intercept(&url.parse().unwrap()).is_none(), "{url}");
    }
    assert!(
        matcher
            .intercept(&"https://notfixture.invalid/".parse().unwrap())
            .is_some()
    );
    let mut cfg = local_config();
    assert!(
        proxy_matcher(&cfg)
            .await
            .unwrap()
            .intercept(&"https://fixture.invalid/".parse().unwrap())
            .is_none()
    );
    cfg.proxy = Some(ProxyConfig {
        mode: ProxyMode::Manual,
        url: "http://127.0.0.1:1080".into(),
    });
    assert!(
        proxy_matcher(&cfg)
            .await
            .unwrap()
            .intercept(&"https://fixture.invalid/".parse().unwrap())
            .is_some()
    );
}

#[tokio::test]
async fn invalid_proxy_schemes_fail_instead_of_becoming_direct_routes() {
    for input in ["ftp://127.0.0.1:1080", "file:///tmp/proxy", "not a proxy"] {
        let mut cfg = local_config();
        cfg.proxy = Some(ProxyConfig {
            mode: ProxyMode::Manual,
            url: input.into(),
        });
        let Err(error) = proxy_matcher(&cfg).await else {
            panic!("invalid proxy accepted: {input}");
        };
        assert_eq!(error.code, "E_PROXY");
    }
    for input in [
        "127.0.0.1:1080",
        "http://bücher.invalid:1080",
        "socks5://bücher.invalid",
    ] {
        let mut cfg = local_config();
        cfg.proxy = Some(ProxyConfig {
            mode: ProxyMode::Manual,
            url: input.into(),
        });
        let matcher = proxy_matcher(&cfg).await.unwrap();
        let proxy = matcher
            .intercept(&"https://fixture.invalid/".parse().unwrap())
            .expect(input);
        assert!(
            !proxy.uri().host().unwrap().contains('%'),
            "proxy host must be normalized"
        );
        if input.starts_with("socks5:") {
            assert_eq!(proxy.uri().port_u16(), Some(1080));
        }
    }
}

#[tokio::test]
async fn execute_keeps_remote_dns_and_proxy_authentication_when_private_network_is_allowed() {
    use base64::Engine;
    let mut fixture = Fixture::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
        false,
    )
    .await;
    let mut cfg = local_config();
    cfg.proxy = Some(ProxyConfig {
        mode: ProxyMode::Manual,
        url: format!(
            "http://fixture-user:fixture-pass@{}",
            fixture.listener.local_addr().unwrap()
        ),
    });
    let response = execute(
        &cfg,
        request("http://remote-only.invalid/resource", Method::GET, None),
        &CancellationToken::new(),
        Duration::from_secs(3),
    )
    .await
    .unwrap();
    assert_eq!(response.body, b"ok");
    let headers = tokio::time::timeout(Duration::from_secs(2), fixture.sent.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    let headers = String::from_utf8(headers).unwrap();
    assert!(headers.starts_with("GET http://remote-only.invalid/resource HTTP/1.1\r\n"));
    let auth = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("proxy-authorization")
                .then_some(value.trim())
        })
        .expect("proxy credentials must reach the configured proxy");
    let expected_auth =
        base64::engine::general_purpose::STANDARD.encode("fixture-user:fixture-pass");
    assert_eq!(auth, format!("Basic {expected_auth}"));
    fixture.finish().await;

    cfg.network.allow_private_network = false;
    let err = execute(
        &cfg,
        request("http://remote-only.invalid/resource", Method::GET, None),
        &CancellationToken::new(),
        Duration::from_secs(3),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err.code, "E_SSRF_PROXY_UNSUPPORTED");
}

/// The listener stays owned by the test even after the response task finishes.
struct Fixture {
    listener: Arc<TcpListener>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    sent: Option<oneshot::Receiver<Vec<u8>>>,
}

impl Fixture {
    async fn new(response: Vec<u8>, hold_open: bool) -> Self {
        let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
        let server_listener = listener.clone();
        let stop = CancellationToken::new();
        let server_stop = stop.clone();
        let (sent_tx, sent) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) =
                tokio::time::timeout(Duration::from_secs(5), server_listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let header = tokio::time::timeout(Duration::from_secs(2), async {
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    assert!(header.len() < 65536, "request headers too large");
                    header.push(socket.read_u8().await.unwrap());
                }
                socket.write_all(&response).await.unwrap();
                header
            })
            .await
            .unwrap();
            let _ = sent_tx.send(header);
            if hold_open {
                let _ = tokio::time::timeout(Duration::from_secs(5), server_stop.cancelled()).await;
            }
            tokio::time::timeout(Duration::from_secs(2), socket.shutdown())
                .await
                .unwrap()
                .unwrap();
        });
        Self {
            listener,
            stop,
            task,
            sent: Some(sent),
        }
    }

    fn url(&self) -> String {
        format!("http://{}/fixture", self.listener.local_addr().unwrap())
    }

    async fn finish(mut self) {
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(3), &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}

#[tokio::test]
async fn total_deadline_includes_throttle_before_connecting() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let throttle = super::super::HostThrottle::new();
    throttle.wait("127.0.0.1").await;
    let started = std::time::Instant::now();
    let result = execute_with_throttle(
        &local_config(),
        request(&url, Method::GET, None),
        &CancellationToken::new(),
        Duration::from_millis(50),
        &throttle,
    )
    .await;
    let err = result.err().expect("throttled request should time out");
    assert_eq!(err.code, "E_NETWORK");
    assert!(err.message.contains("超时"));
    assert!(started.elapsed() < Duration::from_millis(800));
    assert!(
        tokio::time::timeout(Duration::from_millis(80), listener.accept())
            .await
            .is_err(),
        "request must not connect after timeout"
    );
}

#[tokio::test]
async fn total_deadline_includes_body_and_precancellation_never_connects() {
    let mut fixture = Fixture::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx".to_vec(),
        true,
    )
    .await;
    let url = fixture.url();
    let started = std::time::Instant::now();
    let operation = tokio::spawn(async move {
        let mut throttle = super::super::HostThrottle::new();
        throttle.min_interval = Duration::ZERO;
        execute_with_throttle(
            &local_config(),
            request(&url, Method::GET, None),
            &CancellationToken::new(),
            Duration::from_secs(3),
            &throttle,
        )
        .await
    });
    tokio::time::timeout(Duration::from_millis(2500), fixture.sent.take().unwrap())
        .await
        .expect("request must reach body reading before deadline")
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(4), operation)
        .await
        .unwrap()
        .unwrap();
    let err = result.err().expect("unfinished body should time out");
    assert_eq!(err.code, "E_NETWORK");
    assert!(err.message.contains("超时"));
    assert!(started.elapsed() < Duration::from_secs(4));
    fixture.finish().await;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = execute(
        &local_config(),
        request(
            &format!("http://{}/", listener.local_addr().unwrap()),
            Method::GET,
            None,
        ),
        &cancel,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(result.err().unwrap().code, "E_CANCELLED");
    assert!(
        tokio::time::timeout(Duration::from_millis(80), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn body_limit_applies_to_chunked_and_decompressed_bytes() {
    for limit in [12, 16] {
        let fixture = Fixture::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n8\r\nabcdefgh\r\n0\r\n\r\n".to_vec(), false).await;
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap()
            .get(fixture.url())
            .send()
            .await
            .unwrap();
        let result =
            tokio::time::timeout(Duration::from_secs(2), read_body_limited(response, limit))
                .await
                .unwrap();
        if limit == 16 {
            assert_eq!(result.unwrap(), b"12345678abcdefgh");
        } else {
            assert_eq!(result.unwrap_err().code, "E_TOO_LARGE");
        }
        fixture.finish().await;
    }

    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&vec![b'x'; 4096]).unwrap();
    let compressed = encoder.finish().unwrap();
    assert!(compressed.len() < 128);
    let mut wire = format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", compressed.len()).into_bytes();
    wire.extend(compressed);
    let fixture = Fixture::new(wire, false).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .gzip(true)
        .build()
        .unwrap()
        .get(fixture.url())
        .send()
        .await
        .unwrap();
    let err = tokio::time::timeout(Duration::from_secs(2), read_body_limited(response, 128))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "E_TOO_LARGE");
    fixture.finish().await;
}

#[tokio::test]
async fn cancellation_interrupts_an_incomplete_body_after_headers_arrive() {
    let mut fixture = Fixture::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx".to_vec(),
        true,
    )
    .await;
    let cancel = CancellationToken::new();
    let run_cancel = cancel.clone();
    let url = fixture.url();
    let operation = tokio::spawn(async move {
        let mut throttle = super::super::HostThrottle::new();
        throttle.min_interval = Duration::ZERO;
        execute_with_throttle(
            &local_config(),
            request(&url, Method::GET, None),
            &run_cancel,
            Duration::from_secs(10),
            &throttle,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), fixture.sent.take().unwrap())
        .await
        .expect("response must start before cancellation")
        .unwrap();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), operation)
        .await
        .expect("cancellation must not wait for the request deadline")
        .unwrap();
    assert_eq!(result.err().unwrap().code, "E_CANCELLED");
    fixture.finish().await;
}

#[tokio::test]
async fn local_dns_socks_sends_the_validated_ip_instead_of_the_hostname() {
    // This fixture is the entire proxy: it never dials the requested public address.
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let server_listener = listener.clone();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async {
            let (mut socket, _) = server_listener.accept().await.unwrap();
            assert_eq!(socket.read_u8().await.unwrap(), 5);
            let count = socket.read_u8().await.unwrap();
            let mut methods = vec![0; count as usize];
            socket.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&0));
            socket.write_all(&[5, 0]).await.unwrap();
            let mut connect = [0; 4];
            socket.read_exact(&mut connect).await.unwrap();
            assert_eq!(
                connect,
                [5, 1, 0, 1],
                "SOCKS must receive an IPv4 target, not a DNS name"
            );
            let mut ip = [0; 4];
            socket.read_exact(&mut ip).await.unwrap();
            let port = socket.read_u16().await.unwrap();
            socket
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                assert!(headers.len() < 65536);
                headers.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
            (ip, port)
        })
        .await
        .unwrap()
    });
    let url = Url::parse("http://pin-target.invalid:18080/fixture").unwrap();
    let matcher = Matcher::builder()
        .all(format!("socks5://{}", listener.local_addr().unwrap()))
        .build();
    let client = pinned_client(
        &url,
        &["8.8.8.8:18080".parse().unwrap()],
        matcher.intercept(&url.as_str().parse().unwrap()),
        false,
    )
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), client.get(url).send())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), response.bytes())
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"ok"
    );
    let (ip, port) = server.await.unwrap();
    assert_eq!(ip, [8, 8, 8, 8]);
    assert_eq!(port, 18080);
    drop(listener);
}

#[tokio::test]
async fn no_proxy_route_connects_to_the_pinned_address_without_using_configured_proxy() {
    let mut fixture = Fixture::new(
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec(),
        false,
    )
    .await;
    let address = fixture.listener.local_addr().unwrap();
    let url = Url::parse(&format!("http://fixture.invalid:{}/", address.port())).unwrap();
    let matcher = Matcher::builder()
        .all("http://127.0.0.1:1")
        .no("fixture.invalid")
        .build();
    let client = pinned_client(
        &url,
        &[address],
        matcher.intercept(&url.as_str().parse().unwrap()),
        true,
    )
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(3), client.get(url).send())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let headers = tokio::time::timeout(Duration::from_secs(2), fixture.sent.take().unwrap())
        .await
        .unwrap()
        .unwrap();
    let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
    assert!(headers.contains(&format!("host: fixture.invalid:{}", address.port())));
    fixture.finish().await;
}
