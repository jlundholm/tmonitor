use std::fs;
use std::path::Path;
use serde::Deserialize;
use serde_spanned::Spanned;

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

fn default_port() -> Spanned<u32> {
    Spanned::new(0..0, 0)
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostConfig {
    pub name: Spanned<String>,
    pub address: String,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    #[serde(default = "default_port")]
    pub port: Spanned<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse TOML config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("config path is not a file: {0}")]
    BadPath(String),
    #[error("host at index {index} has an empty name (line ~{line})")]
    EmptyHostname { index: usize, line: usize },
    #[error("duplicate hostname '{name}' at index {index} (line ~{line}), first defined at index {first_index} (line ~{first_line})")]
    DuplicateHostname { name: String, index: usize, first_index: usize, line: usize, first_line: usize },
    #[error("host '{host}' service '{service}' has invalid port {port} (line ~{line})")]
    InvalidPort { host: String, service: String, port: u32, line: usize },
    #[error("interval_secs must be at least 1, got {0}")]
    InvalidInterval(u64),
    #[error("hostnames '{name1}' and '{name2}' become identical after truncation to 22 characters (lines ~{line1} and ~{line2})")]
    TruncationCollision { name1: String, name2: String, line1: usize, line2: usize },
}

fn byte_offset_to_line(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())].chars().filter(|&c| c == '\n').count() + 1
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
                let mut config: Config = toml::from_str(&content)?;
                config.validate(&content)?;
                truncate_hostnames(&mut config);
                config.validate_truncation_collisions(&content)?;
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
                name: Spanned::new(0..0, "localhost".to_string()),
                address: "127.0.0.1".to_string(),
                services: vec![],
            }],
        }
    }

    fn validate(&self, content: &str) -> Result<(), ConfigError> {
        if self.interval_secs < 1 {
            return Err(ConfigError::InvalidInterval(self.interval_secs));
        }
        if self.concurrency == 0 {
            return Err(ConfigError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "concurrency must be greater than 0",
            )));
        }
        let mut seen: std::collections::HashMap<&str, (usize, usize)> = std::collections::HashMap::new();
        for (i, host) in self.hosts.iter().enumerate() {
            let name = host.name.get_ref();
            let line = byte_offset_to_line(content, host.name.span().start);
            if name.trim().is_empty() {
                return Err(ConfigError::EmptyHostname { index: i, line });
            }
            if let Some(&(first_index, first_line)) = seen.get(name.as_str()) {
                return Err(ConfigError::DuplicateHostname {
                    name: name.clone(),
                    index: i,
                    first_index,
                    line,
                    first_line,
                });
            }
            seen.insert(name, (i, line));

            for svc in &host.services {
                let port = *svc.port.get_ref();
                let port_line = byte_offset_to_line(content, svc.port.span().start);
                if port == 0 || port > 65535 {
                    return Err(ConfigError::InvalidPort {
                        host: host.name.get_ref().clone(),
                        service: svc.name.clone(),
                        port,
                        line: port_line,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_truncation_collisions(&self, content: &str) -> Result<(), ConfigError> {
        for i in 0..self.hosts.len() {
            for j in (i + 1)..self.hosts.len() {
                if self.hosts[i].name.get_ref() == self.hosts[j].name.get_ref() {
                    return Err(ConfigError::TruncationCollision {
                        name1: self.hosts[i].name.get_ref().clone(),
                        name2: self.hosts[j].name.get_ref().clone(),
                        line1: byte_offset_to_line(content, self.hosts[i].name.span().start),
                        line2: byte_offset_to_line(content, self.hosts[j].name.span().start),
                    });
                }
            }
        }
        Ok(())
    }
}

fn truncate_hostnames(config: &mut Config) {
    const MAX_LEN: usize = 22;
    let mut new_hosts: Vec<HostConfig> = Vec::with_capacity(config.hosts.len());
    for host in config.hosts.drain(..) {
        let span = host.name.span();
        let exceeds = host.name.get_ref().chars().count() > MAX_LEN;
        let new_name = if exceeds {
            host.name.get_ref().chars().take(MAX_LEN).collect::<String>() + "…"
        } else {
            host.name.get_ref().clone()
        };
        new_hosts.push(HostConfig {
            name: Spanned::new(span, new_name),
            ..host
        });
    }
    config.hosts = new_hosts;
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
        assert_eq!(config.hosts[0].name.get_ref(), "router");
        assert_eq!(*config.hosts[0].services[0].port.get_ref(), 22);
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
        assert_eq!(config.hosts[0].name.get_ref().chars().count(), 23);
        assert!(config.hosts[0].name.get_ref().ends_with('…'));
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
        assert_eq!(*config.hosts[0].services[0].port.get_ref(), 65535);
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
    fn test_interval_zero_fails() {
        let toml = r#"
interval_secs = 0

[[hosts]]
name = "server"
address = "10.0.0.1"
"#;
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::InvalidInterval(0) => {}
            e => panic!("expected InvalidInterval(0), got {:?}", e),
        }
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

    #[test]
    fn test_invalid_toml_syntax() {
        let toml = "[[hosts\nname = \"x\"\naddress = \"1.2.3.4\"\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line") || msg.contains("line"), "expected line number in error: {}", msg);
    }

    #[test]
    fn test_empty_hostname_with_line() {
        let toml = "[[hosts]]\nname = \"\"\naddress = \"10.0.0.1\"\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line ~2"), "expected line ~2 in error: {}", msg);
        assert!(msg.contains("empty name"), "expected 'empty name' in error: {}", msg);
    }

    #[test]
    fn test_duplicate_hostname_with_lines() {
        let toml = "[[hosts]]\nname = \"router\"\naddress = \"10.0.0.1\"\n\n[[hosts]]\nname = \"router\"\naddress = \"10.0.0.2\"\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line ~2"), "expected first line reference in error: {}", msg);
        assert!(msg.contains("line ~6"), "expected second line reference in error: {}", msg);
    }

    #[test]
    fn test_port_zero_with_line() {
        let toml = "[[hosts]]\nname = \"server\"\naddress = \"10.0.0.1\"\n\n[[hosts.services]]\nname = \"bad\"\nport = 0\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("line ~7"), "expected line ~7 in error: {}", msg);
        assert!(msg.contains("invalid port"), "expected 'invalid port' in error: {}", msg);
    }

    #[test]
    fn test_port_65536_fails() {
        let toml = "[[hosts]]\nname = \"server\"\naddress = \"10.0.0.1\"\n\n[[hosts.services]]\nname = \"bad\"\nport = 65536\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("invalid port"), "expected 'invalid port' in error: {}", msg);
    }

    #[test]
    fn test_truncation_collision_fails() {
        let toml = "[[hosts]]\nname = \"abc-01234567890123456789\"\naddress = \"10.0.0.1\"\n\n[[hosts]]\nname = \"abc-0123456789012345678X\"\naddress = \"10.0.0.2\"\n";
        let file = write_temp_config(toml);
        let result = Config::load(Some(file.path()));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConfigError::TruncationCollision { .. } => {}
            e => panic!("expected TruncationCollision, got: {:?}", e),
        }
    }
}
