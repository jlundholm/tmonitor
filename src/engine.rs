use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::check::{self, CheckResult, ConcurrencyGuard};
use crate::config::Config;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum CellKey {
    Host(String),
    Service { host: String, service: String },
}

impl CellKey {
    pub fn label(&self) -> String {
        match self {
            CellKey::Host(name) => name.clone(),
            CellKey::Service { host, service } => format!("{}/{}", host, service),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostState {
    pub status: CheckResult,
    pub up_since: Option<Instant>,
    pub down_since: Option<Instant>,
}

impl HostState {
    pub fn new() -> Self {
        let now = Instant::now();
        HostState {
            status: CheckResult::Up,
            up_since: Some(now),
            down_since: None,
        }
    }

    pub fn uptime_duration(&self) -> Duration {
        self.up_since.map_or(Duration::ZERO, |t| t.elapsed())
    }

    pub fn downtime_duration(&self) -> Duration {
        self.down_since.map_or(Duration::ZERO, |t| t.elapsed())
    }
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("concurrency error: {0}")]
    Concurrency(#[from] check::ConcurrencyError),
    #[error("check error: {0}")]
    Check(#[from] check::CheckError),
}

pub struct Engine {
    config: Config,
    state: Arc<RwLock<HashMap<CellKey, HostState>>>,
    cell_order: Vec<CellKey>,
    guard: ConcurrencyGuard,
    http_client: reqwest::Client,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self, EngineError> {
        let guard = ConcurrencyGuard::new(config.concurrency)?;
        let http_client = check::build_http_client(Duration::from_secs(10), true)?;
        let mut state_map = HashMap::new();
        let mut cell_order = Vec::new();

        for host in &config.hosts {
            let key = CellKey::Host(host.name.get_ref().clone());
            state_map.insert(key.clone(), HostState::new());
            cell_order.push(key);

            for svc in &host.services {
                let key = CellKey::Service {
                    host: host.name.get_ref().clone(),
                    service: svc.name.clone(),
                };
                state_map.insert(key.clone(), HostState::new());
                cell_order.push(key);
            }
        }

        Ok(Engine {
            config,
            state: Arc::new(RwLock::new(state_map)),
            cell_order,
            guard,
            http_client,
        })
    }

    pub fn shared_state(&self) -> Arc<RwLock<HashMap<CellKey, HostState>>> {
        self.state.clone()
    }

    pub fn cell_order(&self) -> Vec<CellKey> {
        self.cell_order.clone()
    }

    pub async fn run(&self, cancel: CancellationToken) -> Result<(), EngineError> {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let cycle_start = Instant::now();

            let mut handles: Vec<(CellKey, tokio::task::JoinHandle<CheckResult>)> = Vec::new();

            for host in &self.config.hosts {
                {
                    let guard = self.guard.clone();
                    let address = host.address.clone();
                    let key = CellKey::Host(host.name.get_ref().clone());
                    let handle = tokio::spawn(async move {
                        let _permit = match guard.acquire().await {
                            Ok(p) => p,
                            Err(_) => return CheckResult::Down,
                        };
                        check::ping_host(&address)
                            .await
                            .unwrap_or(CheckResult::Down)
                    });
                    handles.push((key, handle));
                }
                for svc in &host.services {
                    let guard = self.guard.clone();
                    let address = host.address.clone();
                    let port = *svc.port.get_ref() as u16;
                    let key = CellKey::Service {
                        host: host.name.get_ref().clone(),
                        service: svc.name.clone(),
                    };
                    let key_label = key.label();
                    let svc_type = svc.service_type.clone();
                    let path = svc.path.as_deref().filter(|p| !p.is_empty()).unwrap_or("/").to_string();
                    let expected_status = svc.expected_status;
                    let http_client = self.http_client.clone();
                    let handle = tokio::spawn(async move {
                        let _permit = match guard.acquire().await {
                            Ok(p) => p,
                            Err(_) => return CheckResult::Down,
                        };
                        match svc_type.as_str() {
                            "http" => {
                                log::info!("check {} type=http addr={}:{}", key_label, address, port);
                                check::check_http(&http_client, &address, port, &path, false, expected_status, Duration::from_secs(5))
                                    .await
                                    .unwrap_or(CheckResult::Down)
                            }
                            "https" => {
                                log::info!("check {} type=https addr={}:{}", key_label, address, port);
                                check::check_http(&http_client, &address, port, &path, true, expected_status, Duration::from_secs(5))
                                    .await
                                    .unwrap_or(CheckResult::Down)
                            }
                            _ => {
                                check::check_port(&address, port, Duration::from_secs(5))
                                    .await
                                    .unwrap_or(CheckResult::Down)
                            }
                        }
                    });
                    handles.push((key, handle));
                }
            }

            let mut results: HashMap<CellKey, CheckResult> = HashMap::new();
            for (cell_key, handle) in handles {
                let result = match handle.await {
                    Ok(r) => r,
                    Err(_) => CheckResult::Down,
                };
                results.insert(cell_key, result);
            }

            let mut state = self.state.write().await;
            for cell_key in &self.cell_order {
                if let Some(result) = results.get(cell_key) {
                    if let Some(cell_state) = state.get_mut(cell_key) {
                        if *result != cell_state.status {
                            let now = Instant::now();
                            let old_status = cell_state.status;
                            let label = cell_key.label();
                            log::info!("{} transitioned: {:?} → {:?}", label, old_status, *result);
                            cell_state.status = *result;
                            match *result {
                                CheckResult::Up => {
                                    cell_state.down_since = None;
                                    cell_state.up_since = Some(now);
                                }
                                CheckResult::Down => {
                                    cell_state.up_since = None;
                                    cell_state.down_since = Some(now);
                                }
                            }
                        }
                    }
                }
            }
            drop(state);

            let elapsed = cycle_start.elapsed();
            let host_count = self.config.hosts.len();
            let svc_count: usize = self.config.hosts.iter().map(|h| h.services.len()).sum();
            let down_count = results.values().filter(|r| **r == CheckResult::Down).count();
            log::info!("Cycle complete: {} hosts, {} services, {} down, {:.2}s", host_count, svc_count, down_count, elapsed.as_secs_f64());
            let interval = Duration::from_secs(self.config.interval_secs);
            if elapsed < interval {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Ok(());
                    }
                    _ = tokio::time::sleep(interval - elapsed) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, HostConfig, ServiceConfig};
    use serde_spanned::Spanned;

    fn test_config() -> Config {
        Config {
            interval_secs: 60,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: Spanned::new(0..0, "localhost".to_string()),
                address: "127.0.0.1".to_string(),
                services: vec![],
            }],
        }
    }

    #[test]
    fn test_host_state_initial() {
        let state = HostState::new();
        assert_eq!(state.status, CheckResult::Up);
        assert!(state.up_since.is_some());
        assert!(state.down_since.is_none());
    }

    #[test]
    fn test_host_state_default() {
        let state = HostState::default();
        assert_eq!(state.status, CheckResult::Up);
    }

    #[test]
    fn test_host_state_up_to_down_transition() {
        let state = HostState {
            status: CheckResult::Down,
            up_since: None,
            down_since: Some(Instant::now()),
        };

        assert_eq!(state.status, CheckResult::Down);
        assert!(state.up_since.is_none());
        assert!(state.down_since.is_some());
        assert_eq!(state.uptime_duration(), Duration::ZERO);
        assert!(state.downtime_duration() < Duration::from_millis(100));
    }

    #[test]
    fn test_host_state_down_to_up_transition() {
        let mut state = HostState {
            status: CheckResult::Down,
            up_since: None,
            down_since: Some(Instant::now()),
        };

        let now = Instant::now();
        state.status = CheckResult::Up;
        state.down_since = None;
        state.up_since = Some(now);

        assert_eq!(state.status, CheckResult::Up);
        assert!(state.down_since.is_none());
        assert!(state.up_since.is_some());
        assert_eq!(state.downtime_duration(), Duration::ZERO);
        assert!(state.uptime_duration() < Duration::from_millis(100));
    }

    #[test]
    fn test_host_state_no_transition_preserves_fields() {
        let up_since = Instant::now() - Duration::from_secs(3600);
        let state = HostState {
            status: CheckResult::Up,
            up_since: Some(up_since),
            down_since: None,
        };

        assert_eq!(state.up_since, Some(up_since));
        assert!(state.down_since.is_none());
        assert!(state.uptime_duration() >= Duration::from_secs(3600));
    }

    #[test]
    fn test_engine_new_initializes_state() {
        let config = test_config();
        let engine = Engine::new(config).unwrap();
        assert_eq!(engine.cell_order.len(), 1);
        assert_eq!(
            engine.cell_order[0],
            CellKey::Host("localhost".to_string())
        );
        assert_eq!(engine.guard.available_permits(), 10);
    }

    #[tokio::test]
    async fn test_engine_shared_state_and_cell_order() {
        let config = test_config();
        let engine = Engine::new(config).unwrap();
        let shared = engine.shared_state();
        let order = engine.cell_order();
        assert_eq!(order.len(), 1);
        let guard = shared.read().await;
        assert!(guard.contains_key(&order[0]));
    }

    #[tokio::test]
    async fn test_engine_empty_hosts() {
        let config = Config {
            interval_secs: 60,
            concurrency: 5,
            hosts: vec![],
        };
        let engine = Engine::new(config).unwrap();
        assert!(engine.cell_order.is_empty());
        let guard = engine.state.read().await;
        assert!(guard.is_empty());
    }

    #[test]
    fn test_engine_config_order_preserved() {
        let config = Config {
            interval_secs: 60,
            concurrency: 5,
            hosts: vec![
                HostConfig {
                    name: Spanned::new(0..0, "beta".to_string()),
                    address: "10.0.0.2".to_string(),
                    services: vec![],
                },
                HostConfig {
                    name: Spanned::new(0..0, "alpha".to_string()),
                    address: "10.0.0.1".to_string(),
                    services: vec![],
                },
            ],
        };
        let engine = Engine::new(config).unwrap();
        assert_eq!(engine.cell_order[0], CellKey::Host("beta".to_string()));
        assert_eq!(engine.cell_order[1], CellKey::Host("alpha".to_string()));
    }

    #[tokio::test]
    async fn test_engine_run_updates_state() {
        let config = Config {
            interval_secs: 1,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: Spanned::new(0..0, "localhost".to_string()),
                address: "127.0.0.1".to_string(),
                services: vec![],
            }],
        };
        let engine = Engine::new(config).unwrap();
        let state = engine.shared_state();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            engine.run(cancel_clone).await.ok();
        });

        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let guard = state.read().await;
        let cell_key = CellKey::Host("localhost".to_string());
        let cell_state = guard.get(&cell_key).unwrap();
        assert!(cell_state.up_since.is_some() || cell_state.down_since.is_some());
    }

    #[tokio::test]
    async fn test_engine_concurrency_cap() {
        let config = Config {
            interval_secs: 1,
            concurrency: 3,
            hosts: (0..10)
                .map(|i| HostConfig {
                    name: Spanned::new(0..0, format!("host-{}", i)),
                    address: "198.51.100.1".to_string(),
                    services: vec![],
                })
                .collect(),
        };
        let engine = Engine::new(config).unwrap();
        let guard = engine.guard.clone();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            engine.run(cancel_clone).await.ok();
        });

        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert_eq!(guard.available_permits(), 3);
    }

    #[tokio::test]
    async fn test_engine_host_and_service_independent() {
        let config = Config {
            interval_secs: 1,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: Spanned::new(0..0, "test-host".to_string()),
                address: "127.0.0.1".to_string(),
                services: vec![ServiceConfig {
                    name: "ssh".to_string(),
                    port: Spanned::new(0..0, 22),
                    service_type: "tcp".to_string(),
                    path: None,
                    expected_status: None,
                    danger_accept_invalid_certs: false,
                }],
            }],
        };
        let engine = Engine::new(config).unwrap();
        let state = engine.shared_state();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        tokio::spawn(async move {
            engine.run(cancel_clone).await.ok();
        });

        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.cancel();

        tokio::time::sleep(Duration::from_millis(100)).await;
        let guard = state.read().await;

        let host_key = CellKey::Host("test-host".to_string());
        let svc_key = CellKey::Service {
            host: "test-host".to_string(),
            service: "ssh".to_string(),
        };

        assert!(guard.contains_key(&host_key));
        assert!(guard.contains_key(&svc_key));

        let host_state = guard.get(&host_key).unwrap();
        let svc_state = guard.get(&svc_key).unwrap();
        assert!(host_state.up_since.is_some() || host_state.down_since.is_some());
        assert!(svc_state.up_since.is_some() || svc_state.down_since.is_some());
    }

    #[test]
    fn test_cell_key_label() {
        assert_eq!(
            CellKey::Host("router".to_string()).label(),
            "router"
        );
        assert_eq!(
            CellKey::Service {
                host: "router".to_string(),
                service: "ssh".to_string()
            }
            .label(),
            "router/ssh"
        );
    }

    #[test]
    fn test_engine_cell_order_includes_services() {
        let config = Config {
            interval_secs: 60,
            concurrency: 5,
            hosts: vec![HostConfig {
                name: Spanned::new(0..0, "server".to_string()),
                address: "10.0.0.1".to_string(),
                services: vec![
                    ServiceConfig {
                        name: "ssh".to_string(),
                        port: Spanned::new(0..0, 22),
                        service_type: "tcp".to_string(),
                        path: None,
                        expected_status: None,
                        danger_accept_invalid_certs: false,
                    },
                    ServiceConfig {
                        name: "web".to_string(),
                        port: Spanned::new(0..0, 80),
                        service_type: "tcp".to_string(),
                        path: None,
                        expected_status: None,
                        danger_accept_invalid_certs: false,
                    },
                ],
            }],
        };
        let engine = Engine::new(config).unwrap();
        assert_eq!(engine.cell_order.len(), 3);
        assert_eq!(engine.cell_order[0], CellKey::Host("server".to_string()));
        assert_eq!(
            engine.cell_order[1],
            CellKey::Service {
                host: "server".to_string(),
                service: "ssh".to_string()
            }
        );
        assert_eq!(
            engine.cell_order[2],
            CellKey::Service {
                host: "server".to_string(),
                service: "web".to_string()
            }
        );
    }
}
