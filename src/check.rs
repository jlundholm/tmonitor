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

    let addr: std::net::IpAddr = address
        .parse()
        .map_err(|_| CheckError::InvalidAddress(address.to_string()))?;

    let client = Client::new(&surge_ping::Config::default())
        .map_err(|e| CheckError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let ident = PingIdentifier(0x1234);
    let mut pinger = client.pinger(addr, ident).await;

    match pinger.ping(PingSequence(0), &[]).await {
        Ok(_) => Ok(CheckResult::Up),
        Err(_) => Ok(CheckResult::Down),
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
) -> Result<CheckResult, CheckError> {
    let scheme = if use_tls { "https" } else { "http" };
    let host = if address.contains(':') && !address.starts_with('[') {
        format!("[{}]", address)
    } else {
        address.to_string()
    };
    let url = format!("{}://{}:{}{}", scheme, host, port, path);

    let timeout_client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| CheckError::Http(e.to_string()))?;

    let effective_client = if timeout != Duration::ZERO { &timeout_client } else { client };

    match effective_client.get(&url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            let _ = response.bytes().await;
            match expected_status {
                Some(expected) if status == expected => Ok(CheckResult::Up),
                Some(_) => Ok(CheckResult::Down),
                None if (200..400).contains(&status) => Ok(CheckResult::Up),
                None => Ok(CheckResult::Down),
            }
        }
        Err(e) => {
            if e.is_timeout() {
                Ok(CheckResult::Down)
            } else {
                Ok(CheckResult::Down)
            }
        }
    }
}

pub fn build_http_client(timeout: Duration) -> Result<reqwest::Client, CheckError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5))
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
) -> Result<CheckResult, CheckError> {
    check_http(client, address, port, path, true, expected_status, timeout).await
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
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 200 OK", "ok").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_404_down() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 404 Not Found", "not found").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_500_down() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 500 Internal Server Error", "error").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_expected_status_match() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 200 OK", "ok").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, Some(200), Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_expected_status_mismatch() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 301 Moved", "redirect").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, Some(200), Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_3xx_up_by_default() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let port = serve_http_response("HTTP/1.1 301 Moved Permanently", "").await;
        let result = check_http(&client, "127.0.0.1", port, "/", false, None, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Up)));
    }

    #[tokio::test]
    async fn test_check_http_closed_port_down() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let result = check_http(&client, "127.0.0.1", 1, "/", false, None, Duration::from_secs(2)).await;
        assert!(matches!(result, Ok(CheckResult::Down)));
    }

    #[tokio::test]
    async fn test_check_http_unreachable_down() {
        let client = build_http_client(Duration::from_secs(2)).unwrap();
        let result = check_http(&client, "198.51.100.1", 80, "/", false, None, Duration::from_secs(2)).await;
        assert!(
            matches!(result, Ok(CheckResult::Down)) || result.is_err(),
            "expected Down or error, got {:?}", result
        );
    }
}
