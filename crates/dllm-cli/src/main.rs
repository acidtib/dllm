use clap::{Parser, Subcommand};
use dllm_daemon::{backup, NetworkStore};
use dllm_protocol::{now_unix, AccessRequest, SignedAccessRequest, SignedJoinToken};
use ed25519_dalek::SigningKey;
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
    #[arg(long)]
    management_token: Option<String>,
    #[arg(long, default_value = "dllm-state.json")]
    state: PathBuf,
    #[arg(long, default_value = "dllm-owner.key")]
    owner_key: PathBuf,
    #[arg(long, default_value = "dllm-node.key")]
    node_key: PathBuf,
    #[arg(long, default_value = "dllm-transport.key")]
    transport_key: PathBuf,
    #[arg(long)]
    credentials_path: Option<PathBuf>,
    #[arg(long, default_value = "http://127.0.0.1:7337")]
    node_endpoint: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    InitTransport,
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
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Join {
        token_file: PathBuf,
    },
    Revoke {
        node_key: PathBuf,
    },
    BindTransport {
        transport_peer_id: String,
        #[arg(long)]
        binding_generation: u64,
        #[arg(long)]
        expires_at_unix: u64,
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    RevokeTransport {
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    SetForwarder {
        max_reservations: u32,
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    RemoveForwarder {
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    Assign {
        model: String,
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    Unassign {
        model: String,
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    PublishProfile {
        profile_file: PathBuf,
    },
    Preview {
        model: String,
        #[arg(long)]
        architecture: String,
        #[arg(long)]
        required_memory_bytes: u64,
        #[arg(long, value_delimiter = ',')]
        backends: Vec<String>,
    },
    Credentials,
    InferencePolicy,
    CreateCredential {
        label: String,
        #[arg(value_parser = ["viewer", "operator", "admin"])]
        role: String,
    },
    RevokeCredential {
        credential_id: String,
    },
    Drain {
        placement_id: String,
    },
    Resume {
        placement_id: String,
    },
    Backup {
        output: PathBuf,
        #[arg(long)]
        passphrase_file: PathBuf,
    },
    Restore {
        input: PathBuf,
        #[arg(long)]
        passphrase_file: PathBuf,
    },
    TransferOwner {
        new_owner_key: PathBuf,
        #[arg(long)]
        old_owner_endpoint: String,
    },
    RequestAccess {
        owner_endpoint: String,
        #[arg(long)]
        note: Option<String>,
    },
    ListAccessRequests,
    ApproveAccess {
        node_key: PathBuf,
        #[arg(long)]
        endpoint: Option<String>,
    },
    DenyAccess {
        node_key: PathBuf,
    },
    SetBudget {
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
        #[arg(long)]
        max_in_flight: u32,
        #[arg(long)]
        max_per_window: u32,
        #[arg(long)]
        window_seconds: u32,
    },
    RemoveBudget {
        #[arg(long)]
        owner: bool,
        node_key: Option<PathBuf>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = Client::new();
    match cli.command {
        Command::Init => {
            let key = SigningKey::generate(&mut rand::thread_rng());
            write_private_key(&cli.node_key, &key.to_bytes())?;
            println!("created node identity {}", cli.node_key.display());
        }
        Command::InitTransport => {
            let key = dllm_transport::peer::load_or_create_identity(&cli.transport_key)?;
            println!("{}", key.public().to_peer_id());
        }
        Command::Create { name } => {
            let store = NetworkStore::create(name);
            store.save_owner_key(&cli.owner_key)?;
            store.save(&cli.state)?;
            println!("created network {}", store.state.state.network_id);
        }
        Command::Status { json } => {
            let state = request_json(auth(
                client.get(format!("{}/v1/status", cli.daemon)),
                &cli.management_token,
            ))?;
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
        Command::Invite {
            expires_at_unix,
            output,
        } => {
            let token = request_json(auth(
                client
                    .post(format!("{}/v1/invitations", cli.daemon))
                    .json(&json!({ "expires_at_unix": expires_at_unix })),
                &cli.management_token,
            ))?;
            let encoded = serde_json::to_vec_pretty(&token)?;
            if let Some(path) = output {
                fs::write(&path, encoded)?;
                println!("wrote invitation {}", path.display());
            } else {
                println!("{}", String::from_utf8(encoded)?);
            }
        }
        Command::Join { token_file } => {
            let token: SignedJoinToken = serde_json::from_slice(&fs::read(token_file)?)?;
            token.verify(now_unix())?;
            let node_pubkey = NetworkStore::load_owner_key(&cli.node_key)?
                .verifying_key()
                .to_bytes()
                .to_vec();
            let owner_endpoint = token.token.owner_endpoint.clone();
            let response = request_json(auth(
                client
                    .post(format!("{owner_endpoint}/v1/members/join"))
                    .json(&json!({
                        "token": token,
                        "node_pubkey": node_pubkey,
                        "node_endpoint": cli.node_endpoint
                    })),
                &None,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Revoke { node_key } => {
            let node_pubkey = read_key(node_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/members/revoke", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::BindTransport {
            transport_peer_id,
            binding_generation,
            expires_at_unix,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/transport-bindings", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "transport_peer_id": transport_peer_id,
                        "binding_generation": binding_generation,
                        "expires_at_unix": expires_at_unix
                    })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RevokeTransport { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/transport-bindings/revoke", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::SetForwarder {
            max_reservations,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/forwarding-policy", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_reservations": max_reservations
                    })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RemoveForwarder { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/forwarding-policy", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_reservations": null
                    })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Assign {
            model,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = assignment_request(
                &client,
                &cli.daemon,
                &cli.management_token,
                "POST",
                model,
                node_pubkey,
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Unassign {
            model,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = assignment_request(
                &client,
                &cli.daemon,
                &cli.management_token,
                "DELETE",
                model,
                node_pubkey,
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::PublishProfile { profile_file } => {
            let profile: Value = serde_json::from_slice(&fs::read(profile_file)?)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/hardware-profiles", cli.daemon))
                    .json(&profile),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Preview {
            model,
            architecture,
            required_memory_bytes,
            backends,
        } => {
            if backends.is_empty() {
                return Err("at least one --backends value is required".into());
            }
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/placements/preview", cli.daemon))
                    .json(&json!({
                        "model": model,
                        "architecture": architecture,
                        "required_memory_bytes": required_memory_bytes,
                        "compatible_backends": backends
                    })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Credentials => {
            let response = request_json(auth(
                client.get(format!("{}/v1/management/credentials", cli.daemon)),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::InferencePolicy => {
            let response = request_json(auth(
                client.get(format!("{}/v1/inference-policy", cli.daemon)),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::CreateCredential { label, role } => {
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/management/credentials", cli.daemon))
                    .json(&json!({ "label": label, "role": role })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RevokeCredential { credential_id } => {
            request_empty(auth(
                client.delete(format!(
                    "{}/v1/management/credentials/{credential_id}",
                    cli.daemon
                )),
                &cli.management_token,
            ))?;
            println!("revoked credential {credential_id}");
        }
        Command::Drain { placement_id } => {
            let response = request_json(auth(
                client.post(format!("{}/v1/placements/{placement_id}/drain", cli.daemon)),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Resume { placement_id } => {
            let response = request_json(auth(
                client.delete(format!("{}/v1/placements/{placement_id}/drain", cli.daemon)),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Backup {
            output,
            passphrase_file,
        } => {
            let passphrase = read_passphrase(&passphrase_file)?;
            backup::create_backup(
                &cli.state,
                &cli.owner_key,
                cli.credentials_path.as_deref(),
                &output,
                &passphrase,
            )?;
            println!("created encrypted backup {}", output.display());
        }
        Command::Restore {
            input,
            passphrase_file,
        } => {
            let passphrase = read_passphrase(&passphrase_file)?;
            backup::restore_backup(
                &input,
                &cli.state,
                &cli.owner_key,
                cli.credentials_path.as_deref(),
                &passphrase,
            )?;
            println!("restored control plane from {}", input.display());
        }
        Command::RequestAccess {
            owner_endpoint,
            note,
        } => {
            let node_pubkey = read_key(cli.node_key.clone())?;
            let node_pubkey_arr: [u8; 32] = node_pubkey
                .clone()
                .try_into()
                .map_err(|_| "node key must be 32 bytes")?;
            let request = AccessRequest {
                node_pubkey: node_pubkey_arr,
                requested_endpoint: cli.node_endpoint.clone(),
                note: note.unwrap_or_default(),
                requested_at_unix: now_unix(),
            };
            let node_signing_key = NetworkStore::load_owner_key(&cli.node_key)?;
            let signed = SignedAccessRequest::sign(request, &node_signing_key);
            let response = request_json(auth(
                client
                    .post(format!("{owner_endpoint}/v1/access-requests"))
                    .json(&json!({ "request": signed })),
                &None,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::ListAccessRequests => {
            let response = request_json(auth(
                client.get(format!("{}/v1/access-requests", cli.daemon)),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::ApproveAccess { node_key, endpoint } => {
            let node_pubkey = read_key(node_key)?;
            let mut body = json!({ "node_pubkey": node_pubkey });
            if let Some(ep) = endpoint {
                body["endpoint"] = json!(ep);
            }
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/access-requests/approve", cli.daemon))
                    .json(&body),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::DenyAccess { node_key } => {
            let node_pubkey = read_key(node_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/access-requests/deny", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::SetBudget {
            owner,
            node_key,
            max_in_flight,
            max_per_window,
            window_seconds,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/resource-budgets", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_in_flight": max_in_flight,
                        "max_requests_per_window": max_per_window,
                        "window_seconds": window_seconds
                    })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RemoveBudget { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &cli.owner_key)?;
            let response = request_json(auth(
                client
                    .delete(format!("{}/v1/resource-budgets", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &cli.management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::TransferOwner {
            new_owner_key,
            old_owner_endpoint,
        } => {
            let mut store = NetworkStore::load(&cli.state, &cli.owner_key)?;
            let new_owner_key = NetworkStore::load_owner_key(new_owner_key)?;
            store.transfer_owner(new_owner_key, old_owner_endpoint)?;
            store.save(&cli.state)?;
            store.save_owner_key(&cli.owner_key)?;
            println!(
                "transferred ownership at generation {}",
                store.state.state.generation
            );
        }
    }
    Ok(())
}

fn write_private_key(path: &PathBuf, bytes: &[u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn assignment_request(
    client: &Client,
    daemon: &str,
    management_token: &Option<String>,
    method: &str,
    model: String,
    node_pubkey: Vec<u8>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let url = format!("{daemon}/v1/assignments");
    let builder = if method == "POST" {
        client.post(url)
    } else {
        client.delete(url)
    };
    request_json(auth(
        builder.json(&json!({ "model": model, "node_pubkey": node_pubkey })),
        management_token,
    ))
}

fn auth(
    builder: reqwest::blocking::RequestBuilder,
    token: &Option<String>,
) -> reqwest::blocking::RequestBuilder {
    match token {
        Some(token) => builder.bearer_auth(token),
        None => builder,
    }
}

fn assignment_key(
    owner: bool,
    node_key: Option<PathBuf>,
    owner_key: &PathBuf,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match (owner, node_key) {
        (true, None) => Ok(NetworkStore::load_owner_key(owner_key)?
            .verifying_key()
            .to_bytes()
            .to_vec()),
        (false, Some(path)) => read_key(path),
        _ => Err("select exactly one assignment target: --owner or NODE_KEY".into()),
    }
}

fn read_key(path: PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(NetworkStore::load_owner_key(path)?
        .verifying_key()
        .to_bytes()
        .to_vec())
}

fn request_json(
    builder: reqwest::blocking::RequestBuilder,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = builder.send()?.error_for_status()?;
    Ok(response.json()?)
}

fn request_empty(
    builder: reqwest::blocking::RequestBuilder,
) -> Result<(), Box<dyn std::error::Error>> {
    builder.send()?.error_for_status()?;
    Ok(())
}

fn read_passphrase(path: &PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut passphrase = fs::read(path)?;
    while passphrase.last().is_some_and(u8::is_ascii_whitespace) {
        passphrase.pop();
    }
    if passphrase.is_empty() {
        return Err("passphrase file is empty".into());
    }
    Ok(passphrase)
}
