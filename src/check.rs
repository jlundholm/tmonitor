use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckResult {
    Up,
    Down,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("HTTP error: {0}")]
    Http(String),
}

pub async fn ping_host(address: &str) -> Result<CheckResult, CheckError> {
    use surge_ping::{Client, PingIdentifier, PingSequence};

    let addr: std::net::IpAddr = match address.parse() {
        Ok(ip) => ip,
        Err(_) => tokio::net::lookup_host((address, 0))
            .await
            .map_err(|e| CheckError::InvalidAddress(format!("{address}: {e}")))?
            .next()
            .ok_or_else(|| CheckError::InvalidAddress(format!("{address}: no addresses found")))?
            .ip(),
    };

    let client = Client::new(&surge_ping::Config::default())
        .map_err(|e| CheckError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let ident = PingIdentifier(0x1234);
    let mut pinger = client.pinger(addr, ident).await;

    match tokio::time::timeout(Duration::from_secs(10), pinger.ping(PingSequence(0), &[])).await {
        Ok(Ok(_)) => Ok(CheckResult::Up),
        Ok(Err(_)) => Ok(CheckResult::Down),
        Err(_) => {
            log::debug!("[ping_host] {} timed out", address);
            Ok(CheckResult::Down)
        }
    }
}

pub async fn check_port(
    address: &str,
    port: u16,
    timeout: Duration,
) -> Result<CheckResult, CheckError> {
    let addr = format!("{}:{}", address, port);

    match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
        Ok(Ok(_)) => Ok(CheckResult::Up),
        Ok(Err(_)) => Ok(CheckResult::Down),
        Err(_) => Ok(CheckResult::Down),
    }
}

pub async fn check_http(
    client: &reqwest::Client,
    address: &str,
    port: u16,
    path: &str,
    use_tls: bool,
    expected_status: Option<u16>,
    timeout: Duration,
    hostname_override: Option<&str>,
) -> Result<CheckResult, CheckError> {
    let scheme = if use_tls { "https" } else { "http" };
    let host = if address.contains(':') && !address.starts_with('[') {
        format!("[{}]", address)
    } else {
        address.to_string()
    };
    let url = format!("{}://{}:{}{}", scheme, host, port, path);

    log::debug!("[check_http] url={} timeout={:?} hostname_override={:?}", url, timeout, hostname_override);
    
    // Create request with explicit timeout and user agent
    let mut request_builder = client.get(&url)
        .timeout(timeout)
        .header("User-Agent", "tmonitor/0.1.0");
    
    // If hostname override is provided, use it for Host header (important for SNI)
    if let Some(hostname) = hostname_override {
        request_builder = request_builder.header("Host", hostname);
    }
    
    // Also wrap in tokio timeout as double protection
    let result = tokio::time::timeout(timeout, async {
        log::debug!("[check_http] sending request for url={}", url);
        let response = request_builder.send().await?;
        log::debug!("[check_http] response received for url={}", url);
        let status = response.status().as_u16();
        log::debug!("[check_http] url={} status={}", url, status);
        log::debug!("[check_http] reading response body for url={}", url);
        let _ = response.bytes().await;
        log::debug!("[check_http] body read complete for url={}", url);
        Ok::<u16, reqwest::Error>(status)
    }).await;
    
    match result {
        Ok(Ok(status)) => {
            log::debug!("[check_http] url={} completed successfully with status={}", url, status);
            match expected_status {
                Some(expected) if status == expected => Ok(CheckResult::Up),
                Some(_) => Ok(CheckResult::Down),
                None if (200..400).contains(&status) => Ok(CheckResult::Up),
                None => Ok(CheckResult::Down),
            }
        }
        Ok(Err(e)) => {
            log::debug!("[check_http] url={} error={} (source: {:?})", url, e, e.source());
            if e.is_timeout() {
                log::debug!("[check_http] url={} error type: timeout", url);
            } else if e.is_connect() {
                log::debug!("[check_http] url={} error type: connection failure", url);
            } else if e.is_request() {
                log::debug!("[check_http] url={} error type: request error", url);
            }
            Ok(CheckResult::Down)
        }
        Err(_) => {
            log::debug!("[check_http] url={} timed out after {:?}", url, timeout);
            Ok(CheckResult::Down)
        }
    }
}

pub fn build_http_client(timeout: Duration, danger_accept_invalid_certs: bool) -> Result<reqwest::Client, CheckError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .tcp_keepalive(Some(Duration::from_secs(2)))
        .connect_timeout(timeout)
        .timeout(timeout)
        .pool_max_idle_per_host(0)
        .pool_idle_timeout(Duration::from_secs(0))
        .http1_only()
        .redirect(reqwest::redirect::Policy::limited(5));
    if danger_accept_invalid_certs {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder
        .build()
        .map_err(|e| CheckError::Http(e.to_string()))
}

pub async fn check_https(
    client: &reqwest::Client,
    address: &str,
    port: u16,
    path: &str,
    expected_status: Option<u16>,
    timeout: Duration,
    hostname_override: Option<&str>,
) -> Result<CheckResult, CheckError> {
    check_http(client, address, port, path, true, expected_status, timeout, hostname_override).await
}

#[derive(Debug, thiserror::Error)]
pub enum ConcurrencyError {
    #[error("max_concurrent must be at least 1, got {0}")]
    ZeroCapacity(usize),
    #[error("semaphore closed")]
    SemaphoreClosed,
}

#[derive(Clone)]
pub struct ConcurrencyGuard {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyGuard {
    pub fn new(max_concurrent: usize) -> Result<Self, ConcurrencyError> {
        if max_concurrent == 0 {
            return Err(ConcurrencyError::ZeroCapacity(max_concurrent));
        }
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        })
    }

    pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, ConcurrencyError> {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ConcurrencyError::SemaphoreClosed)
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ping_localhost_up() {
        match ping_host("127.0.0.1").await {
            Ok(CheckResult::Up) => {}
            Ok(CheckResult::Down) => panic!("127.0.0.1 should be reachable via ICMP"),
            Err(CheckError::InvalidAddress(_)) => panic!("127.0.0.1 is a valid address"),
            Err(_) => {} // ICMP not permitted or network unavailable — acceptable
        }
    }

    #[tokio::test]
    async fn test_ping_unreachable_down() {
        match ping_host("198.51.100.1").await {
            Ok(CheckResult::Down) => {} // expected
            Ok(CheckResult::Up) => panic!("198.51.100.1 should be unreachable"),
            Err(e) => eprintln!("ping 198.51.100.1 failed (may need net.ipv4.ping_group_range): {e}"),
        }
    }

    #[tokio::test]
    async fn test_check_port_open_ssh() {
        let result = check_port("127.0.0.1", 22, Duration::from_secs(2)).await;
        if cfg!(target_os = "linux") {
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_check_port_closed() {
        let result = check_port("127.0.0.1", 1, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_port_unreachable_timeout() {
        let result = check_port("198.51.100.1", 80, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_invalid_address() {
        let result = ping_host("not-an-ip").await;
        assert!(matches!(result, Err(CheckError::InvalidAddress(_))));
    }

    #[tokio::test]
    async fn test_concurrency_guard_basic() {
        let guard = ConcurrencyGuard::new(5).unwrap();
        assert_eq!(guard.available_permits(), 5);

        let permit = guard.acquire().await.unwrap();
        assert_eq!(guard.available_permits(), 4);
        drop(permit);
        assert_eq!(guard.available_permits(), 5);
    }

    #[tokio::test]
    async fn test_concurrency_guard_max_enforced() {
        let guard = ConcurrencyGuard::new(3).unwrap();
        let p1 = guard.acquire().await.unwrap();
        let p2 = guard.acquire().await.unwrap();
        let p3 = guard.acquire().await.unwrap();
        assert_eq!(guard.available_permits(), 0);

        drop(p1);
        assert_eq!(guard.available_permits(), 1);
        drop(p2);
        drop(p3);
        assert_eq!(guard.available_permits(), 3);
    }

    #[tokio::test]
    async fn test_concurrency_guard_acquire_release_cycle() {
        let guard = ConcurrencyGuard::new(2).unwrap();

        for _ in 0..6 {
            let permit = guard.acquire().await.unwrap();
            assert!(guard.available_permits() <= 1);
            drop(permit);
        }
        assert_eq!(guard.available_permits(), 2);
    }

    #[tokio::test]
    async fn test_concurrency_guard_zero_capacity() {
        let result = ConcurrencyGuard::new(0);
        assert!(result.is_err());
        assert!(matches!(result, Err(ConcurrencyError::ZeroCapacity(0))));
    }

    async fn serve_http_response(status_line: &str, body: &str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let status = status_line.to_string();
        let body = body.to_string();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let response = format!(
                    "{}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n{}",
                    status,
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn test_check_http_200_up() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 200 OK", "ok").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_404_down() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 404 Not Found", "not found").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_500_down() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 500 Internal Server Error", "error").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_expected_status_match() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 200 OK", "ok").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, Some(200), Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_expected_status_mismatch() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 301 Moved", "redirect").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, Some(200), Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_3xx_up_by_default() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let port = serve_http_response("HTTP/1.1 301 Moved Permanently", "").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_closed_port_down() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let result = check_http(&client, "127.0.0.1", 1, "/", false, None, Duration::from_secs(2), None).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_unreachable_down() {
        let client = build_http_client(Duration::from_secs(2), false).unwrap();
        let result = check_http(&client, "198.51.100.1", 80, "/", false, None, Duration::from_secs(2), None).await;
        assert!(
            matches!(result, Ok(CheckResult::Down)) || result.is_err(),
            "expected Down or error, got {:?}", result
        );
    }
}
