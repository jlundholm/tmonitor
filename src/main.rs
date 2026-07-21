mod check;
mod config;
mod display;
mod engine;

use std::env;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config_path = parse_args();

    match config::Config::load(config_path.as_deref()) {
        Ok(config) => {
            let engine = match engine::Engine::new(config) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("Error: {}", err);
                    std::process::exit(1);
                }
            };
            let state = engine.shared_state();
            let host_order = engine.host_order();
            let cancel = CancellationToken::new();
            let engine_cancel = cancel.clone();
            let display_cancel = cancel.clone();

            let engine_handle = tokio::spawn(async move {
                engine.run(engine_cancel).await
            });

            let app = display::App::new(state, host_order);
            let display_handle = tokio::spawn(async move {
                display::run_display(app, display_cancel).await
            });

            tokio::select! {
                result = engine_handle => {
                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => eprintln!("Engine error: {}", e),
                        Err(e) => eprintln!("Engine panicked: {}", e),
                    }
                    cancel.cancel();
                }
                result = display_handle => {
                    if let Err(e) = result {
                        eprintln!("Display error: {:?}", e);
                    }
                    cancel.cancel();
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

fn parse_args() -> Option<PathBuf> {
    let args: Vec<String> = env::args().collect();
    parse_args_from(args.iter().skip(1))
}

fn parse_args_from<'a, I>(args: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a String>,
{
    let args: Vec<&'a String> = args.into_iter().collect();
    let mut result = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if arg == "--config" {
            i += 1;
            if i < args.len() {
                result = Some(PathBuf::from(args[i]));
            } else {
                eprintln!("Error: --config requires a path argument");
                std::process::exit(1);
            }
        } else if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                eprintln!("Error: --config= requires a non-empty path");
                std::process::exit(1);
            }
            result = Some(PathBuf::from(path));
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: tmonitor [--config <path>]");
            std::process::exit(0);
        } else {
            eprintln!("Error: unknown argument '{}'", arg);
            std::process::exit(1);
        }
        i += 1;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args_no_args() {
        let result = parse_args_from(std::iter::empty());
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_args_with_config() {
        let args = vec!["--config".to_string(), "/path/to/config.toml".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result, Some(PathBuf::from("/path/to/config.toml")));
    }

    #[test]
    fn test_parse_args_with_config_equals() {
        let args = vec!["--config=/path/to/config.toml".to_string()];
        let result = parse_args_from(args.iter());
        assert_eq!(result, Some(PathBuf::from("/path/to/config.toml")));
    }
}
