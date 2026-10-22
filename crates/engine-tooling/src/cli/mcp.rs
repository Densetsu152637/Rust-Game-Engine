//! `engine mcp`: the MCP stdio server. Stdout carries JSON-RPC only.

use crate::mcp_tools::ExecTools;
use std::ffi::OsString;
use std::path::PathBuf;

const MCP_USAGE: &str = "Usage: engine mcp <project-root> [--allow-write] [--allow-exec]";

/// Parsed `engine mcp` arguments: the project root and the opt-in access flags.
struct McpArguments {
    root: PathBuf,
    allow_write: bool,
    allow_exec: bool,
}

fn parse_mcp(arguments: &[OsString]) -> std::result::Result<McpArguments, String> {
    let (mut root, mut write, mut exec) = (None, false, false);
    for argument in arguments {
        match argument.to_str() {
            Some("--allow-write") if !write => write = true,
            Some("--allow-exec") if !exec => exec = true,
            Some(flag @ ("--allow-write" | "--allow-exec")) => {
                return Err(format!("duplicate option {flag}"));
            }
            Some(flag) if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            _ if root.is_none() => root = Some(PathBuf::from(argument)),
            _ => return Err("expected exactly one project root".into()),
        }
    }
    Ok(McpArguments {
        root: root.ok_or("expected a project root")?,
        allow_write: write,
        allow_exec: exec,
    })
}

/// Runs the server until stdin closes and returns the process exit code.
pub(super) fn serve(arguments: &[OsString]) -> i32 {
    let mcp = match parse_mcp(arguments) {
        Ok(mcp) => mcp,
        Err(message) => {
            eprintln!("{message}\n{MCP_USAGE}");
            return 2;
        }
    };
    let mut config = engine_mcp::ServerConfig::new()
        .allow_write(mcp.allow_write)
        .allow_exec(mcp.allow_exec);
    if mcp.allow_exec {
        config = config.with_provider(ExecTools::new());
    }
    tracing::info!(
        root = %mcp.root.display(),
        allow_write = mcp.allow_write,
        allow_exec = mcp.allow_exec,
        "MCP session started"
    );
    let started = std::time::Instant::now();
    // Stdout carries JSON-RPC only; every diagnostic goes to stderr.
    match engine_mcp::serve_with_config(
        &mcp.root,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        config,
    ) {
        Ok(()) => {
            tracing::info!(
                duration_ms = started.elapsed().as_millis() as u64,
                "MCP session ended"
            );
            0
        }
        Err(error) => {
            tracing::warn!(%error, duration_ms = started.elapsed().as_millis() as u64, "MCP session failed");
            eprintln!("MCP: {error}");
            1
        }
    }
}
