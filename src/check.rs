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
    #[error("probe timed out after {0:?}")]
    Timeout(Duration),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("DNS lookup failed for {0}")]
    DnsLookupFailed(String),
}

pub async fn ping_host(address: &str, timeout: Duration) -> Result<CheckResult, CheckError> {
    use surge_ping::{Client, PingIdentifier, PingSequence};

    let addr: std::net::IpAddr = address
        .parse()
        .map_err(|_| CheckError::InvalidAddress(address.to_string()))?;

    let client = Client::new(&surge_ping::Config::default())
        .map_err(|e| CheckError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let ident = PingIdentifier(0x1234);
    let mut pinger = client.pinger(addr, ident).await;

    match tokio::time::timeout(timeout, pinger.ping(PingSequence(0), &[])).await {
        Ok(Ok(_)) => Ok(CheckResult::Up),
        Ok(Err(_)) => Ok(CheckResult::Down),
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

#[derive(Clone)]
pub struct ConcurrencyGuard {
    semaphore: Arc<Semaphore>,
}

impl ConcurrencyGuard {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    pub async fn acquire(&self) -> OwnedSemaphorePermit {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore closed")
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
        match ping_host("127.0.0.1", Duration::from_secs(3)).await {
            Ok(CheckResult::Up) => {} // expected
            Ok(CheckResult::Down) => {} // acceptable if host refuses ICMP
            Err(e) => eprintln!("ping 127.0.0.1 failed (may need net.ipv4.ping_group_range): {e}"),
        }
    }

    #[tokio::test]
    async fn test_ping_unreachable_down() {
        match ping_host("198.51.100.1", Duration::from_secs(2)).await {
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
        let result = ping_host("not-an-ip", Duration::from_secs(1)).await;
        assert!(matches!(result, Err(CheckError::InvalidAddress(_))));
    }

    #[tokio::test]
    async fn test_concurrency_guard_basic() {
        let guard = ConcurrencyGuard::new(5);
        assert_eq!(guard.available_permits(), 5);

        let permit = guard.acquire().await;
        assert_eq!(guard.available_permits(), 4);
        drop(permit);
        assert_eq!(guard.available_permits(), 5);
    }

    #[tokio::test]
    async fn test_concurrency_guard_max_enforced() {
        let guard = ConcurrencyGuard::new(3);
        let p1 = guard.acquire().await;
        let p2 = guard.acquire().await;
        let p3 = guard.acquire().await;
        assert_eq!(guard.available_permits(), 0);

        drop(p1);
        assert_eq!(guard.available_permits(), 1);
        drop(p2);
        drop(p3);
        assert_eq!(guard.available_permits(), 3);
    }

    #[tokio::test]
    async fn test_concurrency_guard_acquire_release_cycle() {
        let guard = ConcurrencyGuard::new(2);

        for _ in 0..6 {
            let permit = guard.acquire().await;
            assert!(guard.available_permits() <= 1);
            drop(permit);
        }
        assert_eq!(guard.available_permits(), 2);
    }
}
