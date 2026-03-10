use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use vtx_core::config::Config;
use vtx_core::ipc::{ClientMsg, ServerMsg};

#[derive(Parser)]
#[command(name = "vtx", version, about = "A next-generation terminal multiplexer")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the vtx server daemon
    Server,
    /// Create a new session and attach
    New {
        /// Session name
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Attach to an existing session
    Attach {
        /// Session name or ID
        target: Option<String>,
    },
    /// List active sessions
    #[command(alias = "ls")]
    List,
    /// Open a new pane with an SSH connection
    Ssh {
        /// Destination in [user@]host format
        destination: String,
        /// SSH port
        #[arg(short, long)]
        port: Option<u16>,
    },
    /// Open a system monitoring widget pane
    Widget {
        /// Widget type: cpu, mem, disk, net, sysinfo
        #[arg(value_parser = ["cpu", "mem", "disk", "net", "sysinfo"])]
        kind: String,
    },
}

#[tokio::main]
async fn main() -> vtx_core::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let config = Config::default();

    match cli.command {
        Some(Commands::Server) => {
            let server = vtx_server::VtxServer::new(config);
            server.run().await?;
        }
        Some(Commands::New { name }) => {
            // Start server in background if not running, then attach
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(config.socket_path);
            client.run_attach(name).await?;
        }
        Some(Commands::List) => {
            let client = vtx_client::VtxClient::new(config.socket_path);
            let resp = client.send_command(ClientMsg::ListSessions).await?;
            if let ServerMsg::Sessions { list } = resp {
                if list.is_empty() {
                    println!("No active sessions.");
                } else {
                    for s in list {
                        println!("{}: {} ({} panes)", s.id, s.name, s.pane_count);
                    }
                }
            }
        }
        Some(Commands::Ssh { destination, port }) => {
            ensure_server(&config).await?;
            let (user, host) = if let Some(at_pos) = destination.find('@') {
                (
                    Some(destination[..at_pos].to_string()),
                    destination[at_pos + 1..].to_string(),
                )
            } else {
                (None, destination)
            };
            let client = vtx_client::VtxClient::new(config.socket_path);
            let msg = ClientMsg::SshPane { host, user, port };
            let resp = client.send_command(msg).await?;
            match resp {
                ServerMsg::Render { .. } => {
                    println!("SSH pane opened.");
                }
                ServerMsg::Error { msg } => {
                    eprintln!("Error: {msg}");
                }
                _ => {}
            }
        }
        Some(Commands::Widget { kind }) => {
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(config.socket_path);
            let msg = ClientMsg::Widget { kind };
            let resp = client.send_command(msg).await?;
            match resp {
                ServerMsg::Render { .. } => {
                    println!("Widget pane opened.");
                }
                ServerMsg::Error { msg } => {
                    eprintln!("Error: {msg}");
                }
                _ => {}
            }
        }
        Some(Commands::Attach { target: _ }) => {
            // TODO: attach to specific session
            let client = vtx_client::VtxClient::new(config.socket_path);
            client.run_attach(None).await?;
        }
        None => {
            // Default: create new session
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(config.socket_path);
            client.run_attach(None).await?;
        }
    }

    Ok(())
}

/// Ensure the server is running; if not, fork it into the background.
async fn ensure_server(config: &Config) -> vtx_core::Result<()> {
    use tokio::net::UnixStream;

    // Try to connect — if it works, server is already running
    if UnixStream::connect(&config.socket_path).await.is_ok() {
        return Ok(());
    }

    // Fork server as a background process
    let exe = std::env::current_exe().map_err(|e| vtx_core::VtxError::Other(e.to_string()))?;
    std::process::Command::new(exe)
        .arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| vtx_core::VtxError::Other(format!("Failed to start server: {e}")))?;

    // Wait for server to be ready
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if UnixStream::connect(&config.socket_path).await.is_ok() {
            return Ok(());
        }
    }

    Err(vtx_core::VtxError::Other(
        "Server failed to start".into(),
    ))
}
