use super::*;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
enum Action {
    Reject,
    Reply(&'static [u8]),
    DropAfterRequest,
    Stall,
    BrokenTls,
}

#[derive(Clone, Default, Debug)]
struct Observed {
    targets: Vec<SocketAddr>,
    requests: Vec<Vec<u8>>,
    tls_hellos: Vec<Vec<u8>>,
}

/// An entire SOCKS server; never dials the requested public addresses.
struct Fixture {
    listener: Arc<TcpListener>,
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    observed: Arc<Mutex<Observed>>,
    connected: Arc<tokio::sync::Notify>,
}

impl Fixture {
    async fn new(actions: Vec<Action>) -> Self {
        let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
        let stop = CancellationToken::new();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let connected = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let listener = listener.clone();
            let stop = stop.clone();
            let observed = observed.clone();
            let connected = connected.clone();
            async move {
                for action in actions {
                    let (mut socket, _) = tokio::select! {
                        _ = stop.cancelled() => break,
                        connection = listener.accept() => connection.unwrap(),
                    };
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = Self::serve(&mut socket, action, &observed, &connected) => {}
                    }
                }
            }
        });
        Self {
            listener,
            stop,
            task: Some(task),
            observed,
            connected,
        }
    }

    fn proxy(&self) -> Intercept {
        Matcher::builder()
            .all(format!("socks5://{}", self.listener.local_addr().unwrap()))
            .build()
            .intercept(&"http://pin-target.invalid/".parse().unwrap())
            .unwrap()
    }

    async fn serve(
        socket: &mut TcpStream,
        action: Action,
        observed: &Mutex<Observed>,
        connected: &tokio::sync::Notify,
    ) {
        assert_eq!(socket.read_u8().await.unwrap(), 5);
        let count = socket.read_u8().await.unwrap();
        let mut methods = vec![0; count as usize];
        socket.read_exact(&mut methods).await.unwrap();
        assert!(methods.contains(&0));
        socket.write_all(&[5, 0]).await.unwrap();
        let mut connect = [0; 4];
        socket.read_exact(&mut connect).await.unwrap();
        assert_eq!(&connect[..3], &[5, 1, 0]);
        let ip = match connect[3] {
            1 => {
                let mut bytes = [0; 4];
                socket.read_exact(&mut bytes).await.unwrap();
                std::net::IpAddr::from(bytes)
            }
            4 => {
                let mut bytes = [0; 16];
                socket.read_exact(&mut bytes).await.unwrap();
                std::net::IpAddr::from(bytes)
            }
            _ => panic!("SOCKS must receive a validated IP, never a remote DNS name"),
        };
        let target = SocketAddr::new(ip, socket.read_u16().await.unwrap());
        observed.lock().unwrap().targets.push(target);
        connected.notify_one();
        if matches!(action, Action::Stall) {
            std::future::pending::<()>().await;
        }
        if matches!(action, Action::Reject) {
            socket
                .write_all(&[5, 4, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
            return;
        }
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        if matches!(action, Action::BrokenTls) {
            let mut header = [0; 5];
            socket.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 22, "client must start with a TLS handshake");
            let mut hello = vec![0; u16::from_be_bytes([header[3], header[4]]) as usize];
            socket.read_exact(&mut hello).await.unwrap();
            observed.lock().unwrap().tls_hellos.push(hello);
            // Fatal handshake_failure: the HTTP request has not been sent.
            socket.write_all(&[21, 3, 3, 0, 2, 2, 40]).await.unwrap();
            socket.shutdown().await.unwrap();
            return;
        }
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 65536);
            request.push(socket.read_u8().await.unwrap());
        }
        let body_len = String::from_utf8_lossy(&request)
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        assert!(body_len <= 65536);
        let mut body = vec![0; body_len];
        socket.read_exact(&mut body).await.unwrap();
        request.extend(body);
        observed.lock().unwrap().requests.push(request);
        if let Action::Reply(response) = action {
            socket.write_all(response).await.unwrap();
        }
        socket.shutdown().await.unwrap();
    }

    async fn finish(mut self) -> Observed {
        self.stop.cancel();
        let mut task = self.task.take().unwrap();
        match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
            Ok(result) => result.unwrap(),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("SOCKS fixture failed to stop");
            }
        }
        self.observed.lock().unwrap().clone()
    }

    async fn reached(&self, count: usize) {
        loop {
            let notified = self.connected.notified();
            if self.observed.lock().unwrap().targets.len() >= count {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

fn request() -> HttpRequest {
    HttpRequest {
        url: Url::parse("http://pin-target.invalid:18080/fixture").unwrap(),
        method: Method::POST,
        headers: HeaderMap::new(),
        body: Some(serde_json::json!({"x":1})),
    }
}

fn throttle() -> super::super::HostThrottle {
    let mut throttle = super::super::HostThrottle::new();
    throttle.min_interval = Duration::ZERO;
    throttle
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

#[tokio::test]
async fn local_socks_fallback_uses_all_validated_addresses() {
    for values in [
        ["[2001:4860:4860::8888]:18080", "8.8.8.8:18080"],
        ["8.8.8.8:18080", "[2001:4860:4860::8888]:18080"],
        ["8.8.8.8:18080", "1.1.1.1:18080"],
    ] {
        let addresses = checked_addresses(
            values.iter().map(|value| value.parse().unwrap()).collect(),
            false,
        )
        .unwrap();
        let fixture = Fixture::new(vec![Action::Reject, Action::Reply(OK)]).await;
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            send_pinned(
                &request(),
                &addresses,
                Some(fixture.proxy()),
                false,
                &throttle(),
            ),
        )
        .await;
        let observed = fixture.finish().await;
        assert_eq!(
            observed.targets, addresses,
            "failed CONNECT must advance to the next validated IP"
        );
        let response = result.unwrap().unwrap();
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"ok");
        assert_eq!(
            observed.requests.len(),
            1,
            "only a connected address may receive the HTTP POST"
        );
        assert!(
            String::from_utf8_lossy(&observed.requests[0])
                .contains("host: pin-target.invalid:18080")
        );
    }
}

#[tokio::test]
async fn local_socks_does_not_replay_a_sent_request_or_http_failure() {
    const FAILED: &[u8] =
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 2\r\nConnection: close\r\n\r\nno";
    const TRUNCATED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\nConnection: close\r\n\r\nx";
    for action in [
        Action::Reply(OK),
        Action::Reply(FAILED),
        Action::Reply(TRUNCATED),
        Action::DropAfterRequest,
    ] {
        let addresses = [
            "8.8.8.8:18080".parse().unwrap(),
            "1.1.1.1:18080".parse().unwrap(),
        ];
        let fixture = Fixture::new(vec![action, Action::Reply(OK)]).await;
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            send_pinned(
                &request(),
                &addresses,
                Some(fixture.proxy()),
                false,
                &throttle(),
            ),
        )
        .await
        .expect("request must terminate");
        match action {
            Action::DropAfterRequest => assert_eq!(result.unwrap_err().code, "E_NETWORK"),
            Action::Reply(response) => {
                let result = result.unwrap();
                if response == FAILED {
                    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
                }
                let body = result.bytes().await;
                assert_eq!(body.is_err(), response == TRUNCATED);
            }
            _ => unreachable!(),
        }
        let observed = fixture.finish().await;
        assert_eq!(observed.targets, addresses[..1]);
        assert_eq!(
            observed.requests.len(),
            1,
            "HTTP POST must never be replayed"
        );
    }
}

#[tokio::test]
async fn local_socks_fallback_is_finite_and_stops_when_dropped() {
    let addresses = [
        "8.8.8.8:18080".parse().unwrap(),
        "1.1.1.1:18080".parse().unwrap(),
    ];
    let fixture = Fixture::new(vec![Action::Reject, Action::Reject]).await;
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        send_pinned(
            &request(),
            &addresses,
            Some(fixture.proxy()),
            false,
            &throttle(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().code, "E_NETWORK");
    let observed = fixture.finish().await;
    assert_eq!(observed.targets, addresses);
    assert!(observed.requests.is_empty());

    // The caller owns the operation deadline/cancellation. Dropping an in-flight
    // CONNECT must not leave background attempts that advance to another IP.
    let fixture = Fixture::new(vec![Action::Reject, Action::Stall, Action::Reply(OK)]).await;
    let addresses = [addresses[0], addresses[1], "9.9.9.9:18080".parse().unwrap()];
    {
        let request = request();
        let throttle = throttle();
        let operation = send_pinned(
            &request,
            &addresses,
            Some(fixture.proxy()),
            false,
            &throttle,
        );
        tokio::pin!(operation);
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! {
                _ = fixture.reached(2) => {},
                result = &mut operation => panic!("stalled CONNECT finished: {result:?}"),
            }
        })
        .await
        .expect("the second CONNECT must start before dropping the request");
    }
    let observed = fixture.finish().await;
    assert_eq!(observed.targets, addresses[..2]);
    assert!(observed.requests.is_empty());
}

#[tokio::test]
async fn local_socks_tls_setup_failure_falls_back_without_sending_http() {
    let addresses = [
        "[2001:4860:4860::8888]:18080".parse().unwrap(),
        "8.8.8.8:18080".parse().unwrap(),
    ];
    let fixture = Fixture::new(vec![Action::BrokenTls, Action::BrokenTls]).await;
    let mut request = request();
    request.url.set_scheme("https").unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(3),
        send_pinned(
            &request,
            &addresses,
            Some(fixture.proxy()),
            false,
            &throttle(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().code, "E_NETWORK");
    let observed = fixture.finish().await;
    assert_eq!(observed.targets, addresses);
    assert!(observed.requests.is_empty());
    assert_eq!(observed.tls_hellos.len(), 2);
    for hello in observed.tls_hellos {
        assert!(
            hello
                .windows(b"pin-target.invalid".len())
                .any(|part| part == b"pin-target.invalid")
        );
    }
}
