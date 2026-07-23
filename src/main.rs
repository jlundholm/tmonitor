mod check;
mod config;
mod display;
mod engine;
mod logging;

use std::env;
use std::path::PathBuf;
use tokio::signal;
use tokio_util::sync::CancellationToken;

struct CliArgs {
    config_path: Option<PathBuf>,
    log_file: Option<PathBuf>,
    log_level: Option<String>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args = parse_args();

    if let Some(ref log_file) = args.log_file {
        let level = args.log_level.as_deref().unwrap_or("info");
        if let Err(e) = logging::init(log_file, level) {
            eprintln!("Error: failed to initialize logging: {}", e);
            std::process::exit(1);
        }
        log::info!("Logging initialized (level={})", level);
    } else if args.log_level.is_some() {
        eprintln!("Warning: --log-level requires --log-file to be effective");
    }

    match config::Config::load(args.config_path.as_deref()) {
        Ok(config) => {
            let engine = match engine::Engine::new(config) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Error: {}", err);
                    std::process::exit(1);
                }
            };
            let state = engine.shared_state();
            let cell_order = engine.cell_order();
            let cancel = CancellationToken::new();
            let engine_cancel = cancel.clone();
            let display_cancel = cancel.clone();

            let engine_handle = tokio::spawn(async move {
                engine.run(engine_cancel).await
            });

            let app = display::App::new(state, cell_order);
            let display_handle = tokio::spawn(async move {
                display::run_display(app, display_cancel).await
            });

            let signal_cancel = cancel.clone();
            tokio::spawn(async move {
                #[cfg(unix)]
                {
                    let mut term = match tokio::signal::unix::signal(
                        tokio::signal::unix::SignalKind::terminate(),
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("Warning: failed to register SIGTERM handler: {}", e);
                            signal::ctrl_c().await.ok();
                            signal_cancel.cancel();
                            return;
                        }
                    };
                    tokio::select! {
                        _ = signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                #[cfg(not(unix))]
                signal::ctrl_c().await.ok();

                signal_cancel.cancel();
            });

            let display_result = display_handle.await;
            match &display_result {
                Ok(Err(e)) => {
                    eprintln!("Display error: {}", e);
                    log::error!("Display error: {}", e);
                }
                Err(e) => {
                    eprintln!("Display panicked: {}", e);
                }
                Ok(Ok(())) => {}
            }

            cancel.cancel();
            let engine_result = engine_handle.await;
            match engine_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    eprintln!("Engine error: {}", e);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Engine panicked: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn parse_args() -> CliArgs {
    let args: Vec<String> = env::args().collect();
    parse_args_from(args.iter().skip(1))
}

fn parse_args_from<'a, I>(args: I) -> CliArgs
where
    I: IntoIterator<Item = &'a String>,
{
    let args: Vec<&'a String> = args.into_iter().collect();
    let mut config_path: Option<PathBuf> = None;
    let mut log_file: Option<PathBuf> = None;
    let mut log_level: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if arg == "--config" {
            i += 1;
            if i < args.len() {
                config_path = Some(PathBuf::from(args[i]));
            } else {
                eprintln!("Error: --config requires a path argument");
                std::process::exit(1);
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                eprintln!("Error: --config= requires a non-empty path");
                std::process::exit(1);
            }
            config_path = Some(PathBuf::from(path));
        } else if arg == "--log-file" {
            i += 1;
            if i < args.len() && !args[i].starts_with("--") {
                log_file = Some(PathBuf::from(args[i]));
            } else {
                eprintln!("Error: --log-file requires a path argument");
                std::process::exit(1);
            }
        } else if let Some(path) = arg.strip_prefix("--log-file=") {
            if path.is_empty() {
                eprintln!("Error: --log-file= requires a non-empty path");
                std::process::exit(1);
            }
            log_file = Some(PathBuf::from(path));
        } else if arg == "--log-level" {
            i += 1;
            if i < args.len() {
                let level = args[i].to_lowercase();
                if !["error", "warn", "info", "debug"].contains(&level.as_str()) {
                    eprintln!("Error: invalid log level '{}'. Valid values: error, warn, info, debug", args[i]);
                    std::process::exit(1);
                }
                log_level = Some(level);
            } else {
                eprintln!("Error: --log-level requires a level argument");
                std::process::exit(1);
            }
        } else if let Some(level) = arg.strip_prefix("--log-level=") {
            let level = level.to_lowercase();
            if !["error", "warn", "info", "debug"].contains(&level.as_str()) {
                eprintln!("Error: invalid log level '{}'. Valid values: error, warn, info, debug", level);
                std::process::exit(1);
            }
            log_level = Some(level);
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: tmonitor [--config <path>] [--log-file <path>] [--log-level <level>]");
            std::process::exit(0);
        } else {
            eprintln!("Error: unknown argument '{}'", arg);
            std::process::exit(1);
        }
        i += 1;
    }

    CliArgs { config_path, log_file, log_level }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_log(args: &CliArgs) {
        assert!(args.log_file.is_none());
        assert!(args.log_level.is_none());
    }

    #[test]
    fn test_parse_args_no_args() {
        let result = parse_args_from(std::iter::empty());
        assert!(result.config_path.is_none());
        assert_no_log(&result);
    }

    #[test]
    fn test_parse_args_with_config() {
        let args = vec!["--config".to_string(), "/path/to/config.toml".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result.config_path, Some(PathBuf::from("/path/to/config.toml")));
        assert_no_log(&result);
    }

    #[test]
    fn test_parse_args_with_config_equals() {
        let args = vec!["--config=/path/to/config.toml".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result.config_path, Some(PathBuf::from("/path/to/config.toml")));
        assert_no_log(&result);
    }

    #[test]
    fn test_parse_args_log_file() {
        let args = vec!["--log-file".to_string(), "/tmp/test.log".to_string()];
        let result = parse_args_from(args.iter());
        assert!(result.config_path.is_none());
        assert_eq!(result.log_file, Some(PathBuf::from("/tmp/test.log")));
        assert!(result.log_level.is_none());
    }

    #[test]
    fn test_parse_args_log_file_equals() {
        let args = vec!["--log-file=/tmp/test.log".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result.log_file, Some(PathBuf::from("/tmp/test.log")));
        assert!(result.log_level.is_none());
    }

    #[test]
    fn test_parse_args_log_level() {
        let args = vec!["--log-level".to_string(), "debug".to_string()];
        let result = parse_args_from(args.iter());
        assert!(result.log_file.is_none());
        assert_eq!(result.log_level, Some("debug".to_string()));
    }

    #[test]
    fn test_parse_args_log_level_equals() {
        let args = vec!["--log-level=debug".to_string()];
        let result = parse_args_from(args.iter());
        assert!(result.log_file.is_none());
        assert_eq!(result.log_level, Some("debug".to_string()));
    }

    #[test]
    fn test_parse_args_log_file_and_level() {
        let args = vec!["--log-file".to_string(), "/tmp/t.log".to_string(), "--log-level".to_string(), "warn".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result.log_file, Some(PathBuf::from("/tmp/t.log")));
        assert_eq!(result.log_level, Some("warn".to_string()));
    }

    #[test]
    fn test_parse_args_all_flags() {
        let args = vec![
            "--config".to_string(), "cfg.toml".to_string(),
            "--log-file".to_string(), "log.txt".to_string(),
            "--log-level".to_string(), "error".to_string(),
        ];
        let result = parse_args_from(args.iter());
        assert_eq!(result.config_path, Some(PathBuf::from("cfg.toml")));
        assert_eq!(result.log_file, Some(PathBuf::from("log.txt")));
        assert_eq!(result.log_level, Some("error".to_string()));
    }

    #[test]
    fn test_parse_args_log_level_case_insensitive() {
        let args = vec!["--log-level".to_string(), "DEBUG".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result.log_level, Some("debug".to_string()));
    }
}
