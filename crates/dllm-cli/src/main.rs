use clap::{Parser, Subcommand};
use dllmd::NetworkStore;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(
    name = "dllm",
    version,
    about = "Self-hosted inference network management"
)]
struct Cli {
    #[arg(long, default_value = "http://127.0.0.1:7337")]
    daemon: String,
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
    Invite {
        #[arg(long)]
        expires_at_unix: Option<u64>,
    },
    Join {
        token_file: PathBuf,
        node_key: PathBuf,
    },
    Revoke {
        node_key: PathBuf,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = Client::new();
    match cli.command {
        Command::Create { name } => {
            let store = NetworkStore::create(name);
            store.save_owner_key(&cli.owner_key)?;
            store.save(&cli.state)?;
            println!("created network {}", store.state.state.network_id);
        }
        Command::Status { json } => {
            let state = request_json(client.get(format!("{}/v1/status", cli.daemon)))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                let network = &state["network"]["state"];
                println!(
                    "network {} generation {} members {}",
                    network["name"],
                    network["generation"],
                    network["members"].as_array().map_or(0, Vec::len)
                );
            }
        }
        Command::Invite { expires_at_unix } => {
            let token = request_json(
                client
                    .post(format!("{}/v1/invitations", cli.daemon))
                    .json(&json!({ "expires_at_unix": expires_at_unix })),
            )?;
            println!("{}", serde_json::to_string_pretty(&token)?);
        }
        Command::Join {
            token_file,
            node_key,
        } => {
            let token: Value = serde_json::from_slice(&fs::read(token_file)?)?;
            let node_pubkey = read_key(node_key)?;
            let response = request_json(
                client
                    .post(format!("{}/v1/members/join", cli.daemon))
                    .json(&json!({ "token": token, "node_pubkey": node_pubkey })),
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Revoke { node_key } => {
            let node_pubkey = read_key(node_key)?;
            let response = request_json(
                client
                    .post(format!("{}/v1/members/revoke", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
    }
    Ok(())
}

fn read_key(path: PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() != 32 {
        return Err("node key must contain exactly 32 bytes".into());
    }
    Ok(bytes)
}

fn request_json(
    builder: reqwest::blocking::RequestBuilder,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = builder.send()?.error_for_status()?;
    Ok(response.json()?)
}
