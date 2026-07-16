use clap::{Parser, Subcommand};
use dllmd::NetworkStore;
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "dllm",
    version,
    about = "Self-hosted inference network management"
)]
struct Cli {
    #[arg(long, default_value = "dllm-state.json")]
    state: PathBuf,
    #[arg(long, default_value = "dllm-owner.key")]
    owner_key: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Create {
        name: String,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    Invite,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Create { name } => {
            let store = NetworkStore::create(name);
            store.save_owner_key(&cli.owner_key)?;
            store.save(&cli.state)?;
            println!("created network {}", store.state.state.network_id);
        }
        Command::Status { json } => {
            let bytes = fs::read(&cli.state)?;
            let state: serde_json::Value = serde_json::from_slice(&bytes)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                let network = &state["state"];
                println!(
                    "network {} generation {} members {}",
                    network["name"],
                    network["generation"],
                    network["members"].as_array().map_or(0, Vec::len)
                );
            }
        }
        Command::Invite => {
            let _ = NetworkStore::load_owner_key(&cli.owner_key)?;
            return Err(
                "invite requires a running daemon; local token redemption is not implemented yet"
                    .into(),
            );
        }
    }
    Ok(())
}
