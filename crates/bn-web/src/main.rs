//! `bn-web` — the bn web-dashboard server (#386 / #388).
//!
//! A native-Rust axum server over `bn-core`, sibling to `bn` and `bn-mcp`. This skeleton serves
//! `GET /api/health` and the embedded single-page-app shell; the read API, SSE stream, and write
//! layer land in later issues on this same router + [`AppState`]. Normally launched via `bn serve`
//! (a thin bn-cli front-end that resolves and execs this binary).

mod actions;
mod api;
mod assets;
mod audit;
mod auth;
mod bn_runner;
mod cost;
mod mcp;
mod repos;
mod routes;
mod state;
mod terminal;
mod watch;

use bn_core::resolver::Config;
use state::AppState;
use std::net::SocketAddr;
use tokio::net::TcpListener;

const USAGE: &str = "\
bn-web — bn web dashboard server

USAGE:
    bn-web [OPTIONS]

OPTIONS:
    --port <PORT>    TCP port to bind (default: [web].port, else 7770)
    --bind <ADDR>    Bind address (default: [web].bind, else 127.0.0.1)
    --lan            Bind 0.0.0.0 (reachable on the LAN); overrides --bind
    --show-token     Print the web auth token and exit
    -h, --help       Print this help
    -V, --version    Print version";

/// Parsed command-line arguments. All optional; unset port/bind fall back to `[web]` config.
#[derive(Debug, Default)]
struct Args {
    port: Option<u16>,
    bind: Option<String>,
    lan: bool,
    show_token: bool,
    help: bool,
    version: bool,
}

/// Light hand-rolled parser (keeps clap out of bn-web's dep tree). Supports `--flag`, `--opt V`,
/// and `--opt=V`.
fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--lan" => args.lan = true,
            "--show-token" => args.show_token = true,
            "-h" | "--help" => args.help = true,
            "-V" | "--version" => args.version = true,
            "--port" => {
                let v = it
                    .next()
                    .ok_or_else(|| "--port requires a value".to_string())?;
                args.port = Some(parse_port(&v)?);
            }
            "--bind" => {
                args.bind = Some(
                    it.next()
                        .ok_or_else(|| "--bind requires a value".to_string())?,
                );
            }
            other => {
                if let Some(v) = other.strip_prefix("--port=") {
                    args.port = Some(parse_port(v)?);
                } else if let Some(v) = other.strip_prefix("--bind=") {
                    args.bind = Some(v.to_string());
                } else {
                    return Err(format!("unexpected argument '{other}'"));
                }
            }
        }
    }
    Ok(args)
}

fn parse_port(v: &str) -> Result<u16, String> {
    v.parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| format!("invalid --port '{v}' (expected 1-65535)"))
}

#[tokio::main]
async fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("bn-web: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return;
    }
    if args.version {
        println!("bn-web {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let cfg = Config::load();

    // Bootstrap the web token file (design §5): `--show-token` prints it and exits, so an external
    // dev tunnel can pair against it. The server itself no longer gates on the token (#523), so a
    // bootstrap failure is fatal only when the caller asked to show it; otherwise it's ignored.
    match auth::ensure_token() {
        Ok(token) if args.show_token => {
            println!("{token}");
            return;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("bn-web: could not bootstrap web token: {e}");
            if args.show_token {
                std::process::exit(1);
            }
        }
    }

    // --lan forces the all-interfaces bind; otherwise an explicit --bind, else the [web] default.
    let bind = if args.lan {
        "0.0.0.0".to_string()
    } else {
        args.bind.unwrap_or_else(|| cfg.web.bind.clone())
    };
    let port = args.port.unwrap_or(cfg.web.port);

    let addr: SocketAddr = match format!("{bind}:{port}").parse() {
        Ok(a) => a,
        Err(_) => {
            eprintln!(
                "bn-web: invalid bind '{bind}:{port}' (expected IP:port, e.g. 127.0.0.1:7770)"
            );
            std::process::exit(2);
        }
    };

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bn-web: cannot bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    let state = AppState::new(cfg);

    // Spawn the single SSE watcher (design §2.2): one task feeds the broadcast channel that every
    // `/api/stream` client subscribes to, so N browsers share one set of file reads.
    watch::spawn(state.clone());

    let app = routes::router(state);

    eprintln!("bn-web {} → http://{addr}", env!("CARGO_PKG_VERSION"));

    // `into_make_service_with_connect_info` surfaces each caller's `SocketAddr` to handlers (for the
    // audit trail's `remote_addr`).
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        eprintln!("bn-web: server error: {e}");
        std::process::exit(1);
    }
}

/// Resolve when the process receives Ctrl-C, so axum drains in-flight requests and exits cleanly.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags_and_options_both_syntaxes() {
        let a = parse(&["--port", "8080", "--bind=0.0.0.0", "--lan"]).unwrap();
        assert_eq!(a.port, Some(8080));
        assert_eq!(a.bind.as_deref(), Some("0.0.0.0"));
        assert!(a.lan);

        let b = parse(&["--port=1234", "--show-token"]).unwrap();
        assert_eq!(b.port, Some(1234));
        assert!(b.show_token);
    }

    #[test]
    fn rejects_bad_port_and_unknown_args() {
        assert!(parse(&["--port", "0"]).is_err());
        assert!(parse(&["--port", "notanum"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--port"]).is_err());
    }

    #[test]
    fn empty_args_are_all_defaults() {
        let a = parse(&[]).unwrap();
        assert!(a.port.is_none() && a.bind.is_none());
        assert!(!a.lan && !a.show_token && !a.help && !a.version);
    }
}
