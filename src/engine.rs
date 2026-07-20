use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::check::{self, CheckResult, ConcurrencyGuard};
use crate::config::Config;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct HostKey(pub String);

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
    #[error("engine stopped")]
    Cancelled,
}

pub struct Engine {
    config: Config,
    state: Arc<RwLock<HashMap<HostKey, HostState>>>,
    host_order: Vec<HostKey>,
    guard: ConcurrencyGuard,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self, EngineError> {
        let guard = ConcurrencyGuard::new(config.concurrency)?;
        let mut state_map = HashMap::new();
        let mut host_order = Vec::new();

        for host in &config.hosts {
            let key = HostKey(host.name.clone());
            state_map.insert(key.clone(), HostState::new());
            host_order.push(key);
        }

        Ok(Engine {
            config,
            state: Arc::new(RwLock::new(state_map)),
            host_order,
            guard,
        })
    }

    pub fn shared_state(&self) -> Arc<RwLock<HashMap<HostKey, HostState>>> {
        self.state.clone()
    }

    pub fn host_order(&self) -> Vec<HostKey> {
        self.host_order.clone()
    }

    pub async fn run(&self, cancel: CancellationToken) -> Result<(), EngineError> {
        loop {
            let cycle_start = Instant::now();

            let mut handles: Vec<(String, tokio::task::JoinHandle<CheckResult>)> = Vec::new();

            for host in &self.config.hosts {
                if host.services.is_empty() {
                    let guard = self.guard.clone();
                    let address = host.address.clone();
                    let host_name = host.name.clone();
                    let handle = tokio::spawn(async move {
                        let _permit = match guard.acquire().await {
                            Ok(p) => p,
                            Err(_) => return CheckResult::Down,
                        };
                        check::ping_host(&address, Duration::from_secs(5))
                            .await
                            .unwrap_or(CheckResult::Down)
                    });
                    handles.push((host_name, handle));
                } else {
                    for svc in &host.services {
                        let guard = self.guard.clone();
                        let address = host.address.clone();
                        let port = svc.port;
                        let host_name = host.name.clone();
                        let handle = tokio::spawn(async move {
                            let _permit = match guard.acquire().await {
                                Ok(p) => p,
                                Err(_) => return CheckResult::Down,
                            };
                            check::check_port(&address, port, Duration::from_secs(5))
                                .await
                                .unwrap_or(CheckResult::Down)
                        });
                        handles.push((host_name, handle));
                    }
                }
            }

            let mut results: HashMap<String, CheckResult> = HashMap::new();
            for (host_name, handle) in handles {
                let result = match handle.await {
                    Ok(r) => r,
                    Err(_) => CheckResult::Down,
                };
                let entry = results.entry(host_name).or_insert(CheckResult::Up);
                if result == CheckResult::Down {
                    *entry = CheckResult::Down;
                }
            }

            let mut state = self.state.write().await;
            for host_key in &self.host_order {
                if let Some(result) = results.get(&host_key.0) {
                    if let Some(host_state) = state.get_mut(host_key) {
                        if *result != host_state.status {
                            let now = Instant::now();
                            host_state.status = *result;
                            match *result {
                                CheckResult::Up => {
                                    host_state.down_since = None;
                                    host_state.up_since = Some(now);
                                }
                                CheckResult::Down => {
                                    host_state.up_since = None;
                                    host_state.down_since = Some(now);
                                }
                            }
                        }
                    }
                }
            }
            drop(state);

            let elapsed = cycle_start.elapsed();
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

    fn test_config() -> Config {
        Config {
            interval_secs: 60,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: "localhost".to_string(),
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
        assert_eq!(engine.host_order.len(), 1);
        assert_eq!(engine.host_order[0].0, "localhost");
        assert_eq!(engine.guard.available_permits(), 10);
    }

    #[tokio::test]
    async fn test_engine_shared_state_and_host_order() {
        let config = test_config();
        let engine = Engine::new(config).unwrap();
        let shared = engine.shared_state();
        let order = engine.host_order();
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
        assert!(engine.host_order.is_empty());
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
                    name: "beta".to_string(),
                    address: "10.0.0.2".to_string(),
                    services: vec![],
                },
                HostConfig {
                    name: "alpha".to_string(),
                    address: "10.0.0.1".to_string(),
                    services: vec![],
                },
            ],
        };
        let engine = Engine::new(config).unwrap();
        assert_eq!(engine.host_order[0].0, "beta");
        assert_eq!(engine.host_order[1].0, "alpha");
    }

    #[tokio::test]
    async fn test_engine_run_updates_state() {
        let config = Config {
            interval_secs: 1,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: "localhost".to_string(),
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
        let host_state = guard.get(&HostKey("localhost".to_string())).unwrap();
        assert!(host_state.up_since.is_some() || host_state.down_since.is_some());
    }

    #[tokio::test]
    async fn test_engine_concurrency_cap() {
        let config = Config {
            interval_secs: 1,
            concurrency: 3,
            hosts: (0..10)
                .map(|i| HostConfig {
                    name: format!("host-{}", i),
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
    async fn test_engine_services_host_determines_status() {
        let config = Config {
            interval_secs: 1,
            concurrency: 10,
            hosts: vec![HostConfig {
                name: "test-host".to_string(),
                address: "127.0.0.1".to_string(),
                services: vec![ServiceConfig {
                    name: "ssh".to_string(),
                    port: 22,
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
        let host_state = guard.get(&HostKey("test-host".to_string())).unwrap();
        assert!(host_state.up_since.is_some() || host_state.down_since.is_some());
    }
}
