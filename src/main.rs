use specio::commands::{parse_args, run};
use specio::env::Env;
use specio::error::{Error, exit};
use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

fn report_error(json: bool, error: &Error) {
    if json {
        let body = serde_json::json!({ "error": { "code": error.code(), "message": error.to_string(), "request_id": error.request_id() } });
        eprintln!("{body}");
    } else {
        eprintln!("error: {error}");
    }
}

fn main() -> ExitCode {
    let cli = match parse_args(std::env::args()) {
        Ok(cli) => cli,
        // Clap prints help/version/usage errors itself and picks the exit code (0 or 2).
        Err(e) => e.exit(),
    };
    // Logs go to stderr, at `warn` unless SPECIO_LOG says otherwise. Tokens and content are never logged.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("SPECIO_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let json = cli.json;
    let open_browser = !matches!(&cli.command, specio::cli::Command::Login(a) if a.no_browser);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: cannot start the runtime: {e}");
            return ExitCode::from(exit::FAILURE);
        }
    };
    let outcome = Env::production(cli.api_url.as_deref(), open_browser)
        .and_then(|env| runtime.block_on(run(cli, &env)));
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            report_error(json, &e);
            ExitCode::from(e.exit_code())
        }
    }
}
