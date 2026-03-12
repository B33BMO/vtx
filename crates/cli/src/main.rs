use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use vtx_core::config::Config;
use vtx_core::ipc::{ClientMsg, ServerMsg};

#[derive(Parser)]
#[command(name = "vtx", version, about = "A next-generation terminal multiplexer")]
struct Cli {
    /// Use the GPU-accelerated renderer (requires the `gpu` feature)
    #[arg(long, global = true)]
    gpu: bool,

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
    /// Reload configuration from a Lua file
    Source {
        /// Path to config file (defaults to ~/.config/vtx/config.lua)
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Kill a session or the server
    Kill {
        #[command(subcommand)]
        target: KillTarget,
    },
    /// Save and restore sessions
    Resurrect {
        #[command(subcommand)]
        action: ResurrectAction,
    },
}

#[derive(Subcommand)]
enum KillTarget {
    /// Kill a specific session by name
    Session {
        /// Session name
        name: String,
    },
    /// Shut down the vtx server
    Server,
}

#[derive(Subcommand)]
enum ResurrectAction {
    /// Save the current session
    Save,
    /// Restore a saved session
    Restore {
        /// Name of the saved session to restore
        name: String,
    },
    /// List saved sessions
    List,
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
            let client = vtx_client::VtxClient::new(&config);
            if cli.gpu {
                run_gpu_attach(client, name)?;
            } else {
                client.run_attach(name).await?;
            }
        }
        Some(Commands::List) => {
            let client = vtx_client::VtxClient::new(&config);
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
            let client = vtx_client::VtxClient::new(&config);
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
            let client = vtx_client::VtxClient::new(&config);
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
        Some(Commands::Source { file }) => {
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(&config);
            let msg = ClientMsg::SourceConfig { path: file };
            let resp = client.send_command(msg).await?;
            match resp {
                ServerMsg::ConfigReloaded => {
                    println!("Config reloaded.");
                }
                ServerMsg::Error { msg } => {
                    eprintln!("Error: {msg}");
                }
                _ => {}
            }
        }
        Some(Commands::Kill { target: KillTarget::Session { name } }) => {
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(&config);
            let msg = ClientMsg::KillSession { name: name.clone() };
            let resp = client.send_command(msg).await?;
            match resp {
                ServerMsg::SessionKilled { name } => {
                    println!("Killed session '{name}'.");
                }
                ServerMsg::Error { msg } => {
                    eprintln!("Error: {msg}");
                }
                _ => {}
            }
        }
        Some(Commands::Kill { target: KillTarget::Server }) => {
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(&config);
            let msg = ClientMsg::KillServer;
            let resp = client.send_command(msg).await?;
            match resp {
                ServerMsg::ServerShutdown => {
                    println!("Server shutting down.");
                }
                ServerMsg::Error { msg } => {
                    eprintln!("Error: {msg}");
                }
                _ => {}
            }
        }
        Some(Commands::Resurrect { action }) => {
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(&config);
            match action {
                ResurrectAction::Save => {
                    let resp = client.send_command(ClientMsg::SaveSession).await?;
                    match resp {
                        ServerMsg::SessionSaved => println!("Session saved."),
                        ServerMsg::Error { msg } => eprintln!("Error: {msg}"),
                        _ => {}
                    }
                }
                ResurrectAction::Restore { name } => {
                    let resp = client.send_command(ClientMsg::RestoreSession { name: name.clone() }).await?;
                    match resp {
                        ServerMsg::SessionReady { session, .. } => {
                            println!("Restored session '{}' (id: {})", name, session.0);
                        }
                        ServerMsg::Error { msg } => eprintln!("Error: {msg}"),
                        _ => {}
                    }
                }
                ResurrectAction::List => {
                    let resp = client.send_command(ClientMsg::ListSavedSessions).await?;
                    match resp {
                        ServerMsg::SavedSessions { list } => {
                            if list.is_empty() {
                                println!("No saved sessions.");
                            } else {
                                println!("Saved sessions:");
                                for name in list {
                                    println!("  {name}");
                                }
                            }
                        }
                        ServerMsg::Error { msg } => eprintln!("Error: {msg}"),
                        _ => {}
                    }
                }
            }
        }
        Some(Commands::Attach { target: _ }) => {
            // TODO: attach to specific session
            let client = vtx_client::VtxClient::new(&config);
            if cli.gpu {
                run_gpu_attach(client, None)?;
            } else {
                client.run_attach(None).await?;
            }
        }
        None => {
            // Default: create new session
            ensure_server(&config).await?;
            let client = vtx_client::VtxClient::new(&config);
            if cli.gpu {
                run_gpu_attach(client, None)?;
            } else {
                client.run_attach(None).await?;
            }
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

/// Launch the GPU-accelerated renderer.  This takes over the main thread
/// with the winit event loop, so it cannot be async.
#[cfg(feature = "gpu")]
fn run_gpu_attach(
    client: vtx_client::VtxClient,
    session_name: Option<String>,
) -> vtx_core::Result<()> {
    client.run_attach_gpu(session_name)
}

#[cfg(not(feature = "gpu"))]
fn run_gpu_attach(
    _client: vtx_client::VtxClient,
    _session_name: Option<String>,
) -> vtx_core::Result<()> {
    Err(vtx_core::VtxError::Other(
        "GPU renderer not available. Rebuild with: cargo build --features gpu".into(),
    ))
}
