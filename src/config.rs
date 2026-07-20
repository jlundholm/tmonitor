use std::fs;
use std::path::Path;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default)]
    pub hosts: Vec<HostConfig>,
}

fn default_interval() -> u64 { 60 }
fn default_concurrency() -> usize { 10 }

#[derive(Debug, Clone, Deserialize)]
pub struct HostConfig {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub port: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse TOML config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config path is not a file: {0}")]
    BadPath(String),
    #[error("host at index {index} has an empty name")]
    EmptyHostname { index: usize },
    #[error("duplicate hostname '{name}' at index {index} (first at index {first_index})")]
    DuplicateHostname { name: String, index: usize, first_index: usize },
    #[error("host '{host}' service '{service}' has invalid port {port} (must be 1-65535)")]
    InvalidPort { host: String, service: String, port: u16 },
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Config, ConfigError> {
        match path {
            Some(p) => {
                if !p.exists() {
                    return Err(ConfigError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("config file not found: {}", p.display()),
                    )));
                }
                if !p.is_file() {
                    return Err(ConfigError::BadPath(p.display().to_string()));
                }
                let content = fs::read_to_string(p)?;
                let config: Config = toml::from_str(&content)?;
                config.validate()?;
                let mut config = config;
                truncate_hostnames(&mut config);
                Ok(config)
            }
            None => {
                let default_path = Path::new("tmonitor.toml");
                if default_path.exists() {
                    Self::load(Some(default_path))
                } else {
                    Ok(Self::default_config())
                }
            }
        }
    }

    fn default_config() -> Config {
        Config {
            interval_secs: default_interval(),
            concurrency: default_concurrency(),
            hosts: vec![HostConfig {
                name: "localhost".to_string(),
                address: "127.0.0.1".to_string(),
                services: vec![],
            }],
        }
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.concurrency == 0 {
            return Err(ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "concurrency must be greater than 0",
            )));
        }
        let mut seen = std::collections::HashMap::new();
        for (i, host) in self.hosts.iter().enumerate() {
            if host.name.trim().is_empty() {
                return Err(ConfigError::EmptyHostname { index: i });
            }
            if let Some(&first) = seen.get(&host.name) {
                return Err(ConfigError::DuplicateHostname {
                    name: host.name.clone(),
                    index: i,
                    first_index: first,
                });
            }
            seen.insert(&host.name, i);

            for svc in &host.services {
                if svc.port == 0 {
                    return Err(ConfigError::InvalidPort {
                        host: host.name.clone(),
                        service: svc.name.clone(),
                        port: svc.port,
                    });
                }
            }
        }
        Ok(())
    }
}

fn truncate_hostnames(config: &mut Config) {
    const MAX_LEN: usize = 22;
    for host in &mut config.hosts {
        if host.name.chars().count() > MAX_LEN {
            host.name = host.name.chars().take(MAX_LEN).collect::<String>() + "…";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{}", content).unwrap();
        file
    }

    #[test]
    fn test_valid_config_parses() {
        let toml = r#"
interval_secs = 30
concurrency = 5

[[hosts]]
name = "router"
address = "192.168.1.1"

[[hosts.services]]
name = "ssh"
port = 22
"#;
        let file = write_temp_config(toml);
        let config = Config::load(Some(file.path())).unwrap();
        assert_eq!(config.interval_secs, 30);
        assert_eq!(config.concurrency, 5);
        assert_eq!(config.hosts.len(), 1);
        assert_eq!(config.hosts[0].name, "router");
        assert_eq!(config.hosts[0].services[0].port, 22);
    }

    #[test]
    fn test_default_config_when_no_file_and_no_flag() {
        let config = Config::load(None).unwrap();
        assert_eq!(config.interval_secs, 60);
        assert_eq!(config.concurrency, 10);
        assert_eq!(config.hosts.len(), 1);
        assert_eq!(config.hosts[0].address, "127.0.0.1");
    }

    #[test]
    fn test_config_file_not_found() {
        let result = Config::load(Some(Path::new("/nonexistent/path/tmonitor.toml")));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_path_is_directory() {
        let result = Config::load(Some(Path::new("/tmp")));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::BadPath(_) => {}
            _ => panic!("expected BadPath error"),
        }
    }

    #[test]
    fn test_hostname_truncation() {
        let toml = r#"
[[hosts]]
name = "this-is-a-very-long-hostname-exceeding-22-chars"
address = "10.0.0.1"
"#;
        let file = write_temp_config(toml);
        let config = Config::load(Some(file.path())).unwrap();
        assert_eq!(config.hosts[0].name.chars().count(), 23); // 22 chars + …
        assert!(config.hosts[0].name.ends_with('…'));
    }

    #[test]
    fn test_empty_hostname_fails() {
        let toml = r#"
[[hosts]]
name = ""
address = "10.0.0.1"
"#;
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::EmptyHostname { .. } => {}
            _ => panic!("expected EmptyHostname error"),
        }
    }

    #[test]
    fn test_duplicate_hostname_fails() {
        let toml = r#"
[[hosts]]
name = "router"
address = "10.0.0.1"

[[hosts]]
name = "router"
address = "10.0.0.2"
"#;
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::DuplicateHostname { .. } => {}
            _ => panic!("expected DuplicateHostname error"),
        }
    }

    #[test]
    fn test_invalid_port_zero_fails() {
        let toml = r#"
[[hosts]]
name = "server"
address = "10.0.0.1"

[[hosts.services]]
name = "bad"
port = 0
"#;
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::InvalidPort { .. } => {}
            _ => panic!("expected InvalidPort error"),
        }
    }

    #[test]
    fn test_valid_port_65535_accepted() {
        let toml = r#"
[[hosts]]
name = "server"
address = "10.0.0.1"

[[hosts.services]]
name = "rpc"
port = 65535
"#;
        let file = write_temp_config(toml);
        let config = Config::load(Some(file.path())).unwrap();
        assert_eq!(config.hosts[0].services[0].port, 65535);
    }

    #[test]
    fn test_concurrency_zero_fails() {
        let toml = r#"
concurrency = 0

[[hosts]]
name = "server"
address = "10.0.0.1"
"#;
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_with_defaults_applied() {
        let toml = r#"
[[hosts]]
name = "switch"
address = "10.0.0.2"
"#;
        let file = write_temp_config(toml);
        let config = Config::load(Some(file.path())).unwrap();
        assert_eq!(config.interval_secs, 60);
        assert_eq!(config.concurrency, 10);
    }
}
