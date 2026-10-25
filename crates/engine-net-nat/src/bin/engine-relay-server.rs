//! Reference self-hosted discovery + relay server for `engine-net-nat`.
//!
//! `engine-relay-server [--config PATH] [--bind ADDR]`. Prints the bound address on stdout,
//! nonsecret statistics as JSON lines on stderr, and never logs credentials or secrets.
use engine_net_nat::{RelayServerConfig, RelayService};
use std::{
    io::Write,
    net::{SocketAddr, UdpSocket},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

const USAGE: &str = "usage: engine-relay-server [--config PATH] [--bind ADDR]\n\
  --config PATH  JSON config (bind, limits, auth); see docs/online-nat-relay.md\n\
  --bind ADDR    overrides the configured bind address (default 127.0.0.1:0)";

/// Datagrams handled per loop iteration before checking timers again.
const DATAGRAMS_PER_POLL: usize = 256;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("engine-relay-server: {message}");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &str) -> Result<RelayServerConfig, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read config: {}", e.kind()))?;
    RelayServerConfig::from_json(&text).map_err(|e| {
        match RelayServerConfig::json_error_position(&text) {
            Some((line, column)) => format!("{e} (line {line}, column {column})"),
            None => e.to_string(),
        }
    })
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = None;
    let mut bind = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = Some(args.next().ok_or("--config needs a path")?),
            "--bind" => {
                let value = args.next().ok_or("--bind needs an address")?;
                bind = Some(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|_| "invalid --bind address")?,
                );
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            // The argument is not echoed: a mistyped invocation could contain a secret.
            _ => return Err(format!("unknown argument\n{USAGE}")),
        }
    }
    let mut config = match config_path {
        Some(path) => load(&path)?,
        None => RelayServerConfig::default(),
    };
    if let Some(bind) = bind {
        config.bind = bind;
    }
    let server = config
        .server(|name| std::env::var(name).ok())
        .map_err(|e| e.to_string())?;
    let socket = UdpSocket::bind(config.bind).map_err(|e| format!("cannot bind: {}", e.kind()))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("cannot configure socket: {}", e.kind()))?;
    let mut service = RelayService::new(socket, server);
    let addr = service
        .local_addr()
        .map_err(|e| format!("cannot read bound address: {}", e.kind()))?;
    println!("engine-relay-server listening on {addr}");
    let _ = std::io::stdout().flush();

    let interval =
        (config.stats_interval_secs > 0).then(|| Duration::from_secs(config.stats_interval_secs));
    let mut next_stats = interval.map(|i| Instant::now() + i);
    loop {
        let read = service
            .poll(DATAGRAMS_PER_POLL)
            .map_err(|e| format!("socket failed: {}", e.kind()))?;
        if read == 0 {
            thread::sleep(Duration::from_millis(1));
        }
        if let (Some(at), Some(interval)) = (next_stats, interval)
            && Instant::now() >= at
        {
            let stats = serde_json::to_string(&service.server().stats()).unwrap_or_default();
            eprintln!("stats {stats}");
            next_stats = Some(Instant::now() + interval);
        }
    }
}
