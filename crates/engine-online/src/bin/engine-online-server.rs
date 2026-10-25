//! Reference online-services server: admission gate + lobby + matchmaking over mutual-TLS QUIC
//! (or TLS-over-TCP). Self-hostable; contacts no external service.
//!
//! ```text
//! engine-online-server run --config server.json [--exit-on-stdin-close]
//! engine-online-server generate-key --out signing.pem --key-id k1 --issuer my-studio
//! engine-online-server issue-token --config server.json --client-cert client.pem --subject alice [--ttl 3600]
//! ```
//!
//! Secrets are read from the files named in the config and are never printed. `run` prints one
//! line (`engine-online-server listening <addr> transport=<quic|tcp> key_id=<id>`) once bound.
use engine_online::config::{
    ServerFileConfig, TransportChoice, build_server, encode_public_key, generate_signing_key,
    issue_connect_token,
};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const USAGE: &str = "usage:\n  engine-online-server run --config <file> [--exit-on-stdin-close]\n  engine-online-server generate-key --out <file> --key-id <id> --issuer <name>\n  engine-online-server issue-token --config <file> --client-cert <file> --subject <name> [--ttl <secs>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("engine-online-server: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Value of `--name <value>` (each flag at most once).
fn flag(args: &[String], name: &str) -> Result<Option<String>, String> {
    let mut found = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == name {
            if found.is_some() {
                return Err(format!("{name} given twice"));
            }
            found = Some(
                iter.next()
                    .ok_or_else(|| format!("{name} needs a value"))?
                    .clone(),
            );
        }
    }
    Ok(found)
}
fn required(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name)?.ok_or_else(|| format!("missing {name}\n{USAGE}"))
}
fn reject_unknown(args: &[String], allowed: &[&str], switches: &[&str]) -> Result<(), String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if allowed.contains(&arg.as_str()) {
            iter.next();
        } else if !switches.contains(&arg.as_str()) {
            return Err(format!("unexpected argument {arg:?}\n{USAGE}"));
        }
    }
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    match command.as_str() {
        "run" => {
            reject_unknown(rest, &["--config"], &["--exit-on-stdin-close"])?;
            serve(
                &PathBuf::from(required(rest, "--config")?),
                rest.iter().any(|a| a == "--exit-on-stdin-close"),
            )
        }
        "generate-key" => {
            reject_unknown(rest, &["--out", "--key-id", "--issuer"], &[])?;
            let public = generate_signing_key(
                &PathBuf::from(required(rest, "--out")?),
                &required(rest, "--key-id")?,
                &required(rest, "--issuer")?,
            )
            .map_err(|e| e.to_string())?;
            // Only the public half is printed; the private key lives in the file.
            println!(
                "key_id={} public_key={}",
                public.key_id(),
                encode_public_key(&public)
            );
            Ok(())
        }
        "issue-token" => {
            reject_unknown(
                rest,
                &["--config", "--client-cert", "--subject", "--ttl"],
                &[],
            )?;
            let config = ServerFileConfig::load(&PathBuf::from(required(rest, "--config")?))
                .map_err(|e| e.to_string())?;
            let ttl = match flag(rest, "--ttl")? {
                Some(text) => text.parse::<u64>().map_err(|_| "--ttl must be seconds")?,
                None => 3600,
            };
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| "system clock is before 1970")?
                .as_secs();
            let token = issue_connect_token(
                &config,
                &PathBuf::from(required(rest, "--client-cert")?),
                &required(rest, "--subject")?,
                ttl,
                now,
            )
            .map_err(|e| e.to_string())?;
            println!("{token}");
            Ok(())
        }
        _ => Err(format!("unknown command {command:?}\n{USAGE}")),
    }
}

fn serve(config_path: &Path, exit_on_stdin_close: bool) -> Result<(), String> {
    let config = ServerFileConfig::load(config_path).map_err(|e| e.to_string())?;
    let mut built = build_server(&config).map_err(|e| e.to_string())?;
    let transport = match config.transport {
        TransportChoice::Quic => "quic",
        TransportChoice::Tcp => "tcp",
    };
    println!(
        "engine-online-server listening {} transport={transport} key_id={}",
        built.local_addr,
        built.public_key.key_id()
    );
    std::io::stdout().flush().map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    if exit_on_stdin_close {
        // Opt-in: supervisors that start the process with a closed stdin must not use this.
        let stop = stop.clone();
        thread::spawn(move || {
            let mut sink = [0u8; 256];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            stop.store(true, Ordering::SeqCst);
        });
    }
    while !stop.load(Ordering::SeqCst) {
        let handled = built.server.poll().map_err(|e| e.to_string())?;
        if handled == 0 {
            thread::sleep(Duration::from_millis(2));
        }
    }
    built.server.shutdown();
    Ok(())
}
