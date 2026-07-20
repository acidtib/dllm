use clap::{Parser, Subcommand};
use dllm_daemon::{backup, NetworkStore};
use dllm_protocol::{now_unix, AccessRequest, SignedAccessRequest, SignedJoinToken};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;
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
    #[arg(long, help = "Defaults to ~/.dllm/state.json")]
    state: Option<PathBuf>,
    #[arg(
        long = "authority-key",
        alias = "owner-key",
        help = "Defaults to ~/.dllm/authority.key"
    )]
    owner_key: Option<PathBuf>,
    #[arg(long, help = "Defaults to ~/.dllm/node.key")]
    node_key: Option<PathBuf>,
    #[arg(long, help = "Defaults to ~/.dllm/transport.key")]
    transport_key: Option<PathBuf>,
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
    Model {
        #[command(subcommand)]
        command: ModelCommand,
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
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    RevokeTransport {
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    SetForwarder {
        max_reservations: u32,
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    RemoveForwarder {
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    Assign {
        model: String,
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    Unassign {
        model: String,
        #[arg(long = "authority", alias = "owner")]
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
    #[command(name = "transfer-authority", alias = "transfer-owner")]
    TransferOwner {
        new_owner_key: PathBuf,
        #[arg(long)]
        old_owner_endpoint: String,
    },
    Onboard {
        authority_url: String,
        #[arg(long)]
        timeout: Option<u64>,
    },
    OnboardingStatus,
    RequestAccess {
        authority_url: String,
        #[arg(long)]
        note: Option<String>,
    },
    ListAccessRequests {
        #[arg(long)]
        json: bool,
    },
    ApproveAccess {
        node_key: PathBuf,
        #[arg(long)]
        endpoint: Option<String>,
    },
    DenyAccess {
        node_key: PathBuf,
    },
    SetBudget {
        #[arg(long = "authority", alias = "owner")]
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
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
        node_key: Option<PathBuf>,
    },
    BanNode {
        node_key: PathBuf,
        #[arg(long)]
        reason: String,
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
    },
    UnbanNode {
        node_key: PathBuf,
        #[arg(long = "authority", alias = "owner")]
        owner: bool,
    },
    ReportAbuse {
        subject_key: PathBuf,
        #[arg(long)]
        category: String,
        #[arg(long)]
        note: String,
    },
    ListAbuseReports {
        #[arg(long)]
        json: bool,
    },
    AuditLog {
        #[arg(long)]
        since: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    PeerStatus {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ModelCommand {
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        node_key: Option<PathBuf>,
    },
    List,
    Status {
        model: String,
    },
    Remove {
        model: String,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let state = resolve_path(cli.state, dllm_daemon::default_state_path)?;
    let owner_key = match cli.owner_key {
        Some(path) => path,
        None => {
            let authority = dllm_daemon::default_authority_key_path()?;
            let legacy = dllm_daemon::default_owner_key_path()?;
            dllm_daemon::migrate_legacy_authority_key(&authority, &legacy)?;
            authority
        }
    };
    let node_key = resolve_path(cli.node_key, dllm_daemon::default_node_key_path)?;
    let transport_key = resolve_path(cli.transport_key, dllm_daemon::default_transport_key_path)?;
    let management_token = cli
        .management_token
        .clone()
        .or_else(dllm_daemon::local_config::read_management_token);
    let client = Client::new();
    match cli.command {
        Command::Init => {
            let existed = node_key.exists();
            dllm_daemon::load_or_create_node_key(&node_key)?;
            println!(
                "{} node identity {}",
                if existed { "using" } else { "created" },
                node_key.display()
            );
            let transport = dllm_transport::peer::load_or_create_identity(&transport_key)?;
            println!("transport identity {}", transport.public().to_peer_id());
        }
        Command::InitTransport => {
            let key = dllm_transport::peer::load_or_create_identity(&transport_key)?;
            println!("{}", key.public().to_peer_id());
        }
        Command::Create { name } => {
            let store = NetworkStore::create(name);
            store.save_owner_key(&owner_key)?;
            store.save(&state)?;
            println!("created network {}", store.state.state.network_id);
        }
        Command::Status { json } => {
            let state = request_json(auth(
                client.get(format!("{}/v1/status", cli.daemon)),
                &management_token,
            ))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                let network = &state["network"]["state"];
                let member_count = network["members"].as_array().map_or(0, Vec::len);
                let fwd_count = network["forwarding_policy"].as_array().map_or(0, Vec::len);
                let binding_count = network["transport_bindings"].as_array().map_or(0, Vec::len);
                let budget_count = network["resource_budgets"].as_array().map_or(0, Vec::len);
                let ban_count = network["banned"].as_array().map_or(0, Vec::len);
                let mut expiring_soon = 0u64;
                if let Some(bindings) = network["transport_bindings"].as_array() {
                    let now = dllm_protocol::now_unix();
                    for binding in bindings {
                        if let Some(exp) = binding["expires_at_unix"].as_u64() {
                            if exp > now && exp < now + 86400 {
                                expiring_soon += 1;
                            }
                        }
                    }
                }
                println!(
                    "network {} generation {} members {}",
                    network["name"], network["generation"], member_count
                );
                println!(
                    "forwarding {}  bindings {} ({} expire <24h)  budgets {}  bans {}",
                    fwd_count, binding_count, expiring_soon, budget_count, ban_count
                );
            }
        }
        Command::Model { command } => match command {
            ModelCommand::Add {
                source,
                name,
                node_key,
            } => {
                let (model_id, source) = model_add_source(&source, name)?;
                if node_key.is_some() && source["type"] == "local" {
                    return Err(
                        "a local GGUF path cannot be installed on a remote node; use a Hugging Face repo"
                            .into(),
                    );
                }
                let node_pubkey = node_key.map(read_key).transpose()?;
                eprintln!("Adding {model_id}. This may take several minutes.");
                let response = request_json(auth(
                    client
                        .post(format!("{}/v1/management/models", cli.daemon))
                        .timeout(Duration::from_secs(24 * 60 * 60))
                        .json(&json!({
                            "model_id": model_id,
                            "source": source,
                            "node_pubkey": node_pubkey,
                        })),
                    &management_token,
                ))?;
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            ModelCommand::List => {
                let response = request_json(auth(
                    client.get(format!("{}/v1/management/models", cli.daemon)),
                    &management_token,
                ))?;
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            ModelCommand::Status { model } => {
                let response = request_json(auth(
                    client.get(format!("{}/v1/management/models/{model}", cli.daemon)),
                    &management_token,
                ))?;
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            ModelCommand::Remove { model } => {
                auth(
                    client.delete(format!("{}/v1/management/models/{model}", cli.daemon)),
                    &management_token,
                )
                .send()?
                .error_for_status()?;
                println!("removed {model}");
            }
        },
        Command::Invite {
            expires_at_unix,
            output,
        } => {
            let token = request_json(auth(
                client
                    .post(format!("{}/v1/invitations", cli.daemon))
                    .json(&json!({ "expires_at_unix": expires_at_unix })),
                &management_token,
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
            let node_pubkey = NetworkStore::load_owner_key(&node_key)?
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
                &management_token,
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
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/transport-bindings", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "transport_peer_id": transport_peer_id,
                        "binding_generation": binding_generation,
                        "expires_at_unix": expires_at_unix
                    })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RevokeTransport { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/transport-bindings/revoke", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::SetForwarder {
            max_reservations,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/forwarding-policy", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_reservations": max_reservations
                    })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RemoveForwarder { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/forwarding-policy", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_reservations": null
                    })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Assign {
            model,
            owner,
            node_key,
        } => {
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = assignment_request(
                &client,
                &cli.daemon,
                &management_token,
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
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = assignment_request(
                &client,
                &cli.daemon,
                &management_token,
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
                &management_token,
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
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Credentials => {
            let response = request_json(auth(
                client.get(format!("{}/v1/management/credentials", cli.daemon)),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::InferencePolicy => {
            let response = request_json(auth(
                client.get(format!("{}/v1/inference-policy", cli.daemon)),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::CreateCredential { label, role } => {
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/management/credentials", cli.daemon))
                    .json(&json!({ "label": label, "role": role })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RevokeCredential { credential_id } => {
            request_empty(auth(
                client.delete(format!(
                    "{}/v1/management/credentials/{credential_id}",
                    cli.daemon
                )),
                &management_token,
            ))?;
            println!("revoked credential {credential_id}");
        }
        Command::Drain { placement_id } => {
            let response = request_json(auth(
                client.post(format!("{}/v1/placements/{placement_id}/drain", cli.daemon)),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Resume { placement_id } => {
            let response = request_json(auth(
                client.delete(format!("{}/v1/placements/{placement_id}/drain", cli.daemon)),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::Backup {
            output,
            passphrase_file,
        } => {
            let passphrase = read_passphrase(&passphrase_file)?;
            backup::create_backup(
                &state,
                &owner_key,
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
                &state,
                &owner_key,
                cli.credentials_path.as_deref(),
                &passphrase,
            )?;
            println!("restored control plane from {}", input.display());
        }
        Command::Onboard {
            authority_url,
            timeout,
        } => {
            let timeout = timeout.unwrap_or(300);
            request_json(auth(
                client
                    .post(format!("{}/v1/onboarding/start", cli.daemon))
                    .json(&json!({ "authority_url": authority_url })),
                &management_token,
            ))?;
            println!("access request submitted, waiting for authority approval");
            let start = std::time::Instant::now();
            let deadline = std::time::Duration::from_secs(timeout);
            loop {
                if start.elapsed() > deadline {
                    return Err("timed out waiting for approval".into());
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
                let status: Result<Value, _> = request_json(auth(
                    client
                        .get(format!("{}/v1/onboarding/status", cli.daemon))
                        .header("Accept", "application/json"),
                    &management_token,
                ));
                if let Ok(status) = status {
                    match status["state"].as_str() {
                        Some("active") => {
                            println!("onboarding active");
                            return Ok(());
                        }
                        Some("failed") => {
                            return Err(status["detail"]
                                .as_str()
                                .unwrap_or("onboarding failed")
                                .into());
                        }
                        _ => {}
                    }
                }
            }
        }
        Command::OnboardingStatus => {
            let response = request_json(auth(
                client.get(format!("{}/v1/onboarding/status", cli.daemon)),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RequestAccess {
            authority_url,
            note,
        } => {
            let node_pubkey = read_key(node_key.clone())?;
            let node_pubkey_arr: [u8; 32] = node_pubkey
                .clone()
                .try_into()
                .map_err(|_| "node key must be 32 bytes")?;
            let request = AccessRequest {
                node_pubkey: node_pubkey_arr,
                requested_endpoint: cli.node_endpoint.clone(),
                note: note.unwrap_or_default(),
                requested_at_unix: now_unix(),
                transport_peer_id: Some(
                    dllm_transport::peer::load_or_create_identity(&transport_key)?
                        .public()
                        .to_peer_id()
                        .to_string(),
                ),
            };
            let node_signing_key = NetworkStore::load_owner_key(&node_key)?;
            let signed = SignedAccessRequest::sign(request, &node_signing_key);
            let response = request_json(auth(
                client
                    .post(format!("{authority_url}/v1/access-requests"))
                    .json(&json!({ "request": signed })),
                &None,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::ListAccessRequests { json } => {
            let response = request_json(auth(
                client.get(format!("{}/v1/access-requests", cli.daemon)),
                &management_token,
            ))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                let requests = response.as_array().map_or(&[] as &[Value], Vec::as_slice);
                if requests.is_empty() {
                    println!("no pending access requests");
                } else {
                    println!(
                        "{:<20}  {:<30}  {:<12}  note",
                        "node key", "endpoint", "age"
                    );
                    println!("{}", "-".repeat(80));
                    let now = dllm_protocol::now_unix();
                    for req in requests {
                        let pk = req["node_pubkey"]
                            .as_array()
                            .map(|a| format_hex(a))
                            .unwrap_or_else(|| "unknown".into());
                        let ep = req["requested_endpoint"].as_str().unwrap_or("(none)");
                        let ts = req["requested_at_unix"].as_u64().unwrap_or(0);
                        let age = if ts > 0 && ts < now {
                            format_age(now - ts)
                        } else {
                            "just now".into()
                        };
                        let note = req["note"].as_str().unwrap_or("");
                        println!("{pk:<20}  {ep:<30}  {age:<12}  {note}");
                    }
                }
            }
        }
        Command::ApproveAccess { node_key, endpoint } => {
            let node_pubkey = read_public_key_arg(node_key)?;
            let mut body = json!({ "node_pubkey": node_pubkey });
            if let Some(ep) = endpoint {
                body["endpoint"] = json!(ep);
            }
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/access-requests/approve", cli.daemon))
                    .json(&body),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::DenyAccess { node_key } => {
            let node_pubkey = read_public_key_arg(node_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/access-requests/deny", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &management_token,
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
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/resource-budgets", cli.daemon))
                    .json(&json!({
                        "node_pubkey": node_pubkey,
                        "max_in_flight": max_in_flight,
                        "max_requests_per_window": max_per_window,
                        "window_seconds": window_seconds
                    })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::RemoveBudget { owner, node_key } => {
            let node_pubkey = assignment_key(owner, node_key, &owner_key)?;
            let response = request_json(auth(
                client
                    .delete(format!("{}/v1/resource-budgets", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::BanNode {
            node_key,
            reason,
            owner,
        } => {
            let node_pubkey = assignment_key(owner, Some(node_key), &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/moderation/bans", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey, "reason": reason })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::UnbanNode { node_key, owner } => {
            let node_pubkey = assignment_key(owner, Some(node_key), &owner_key)?;
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/moderation/bans", cli.daemon))
                    .json(&json!({ "node_pubkey": node_pubkey })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::ReportAbuse {
            subject_key,
            category,
            note,
        } => {
            let reporter_pubkey = NetworkStore::load_owner_key(&node_key)?
                .verifying_key()
                .to_bytes();
            let subject_pubkey = NetworkStore::load_owner_key(&subject_key)?
                .verifying_key()
                .to_bytes();
            let response = request_json(auth(
                client
                    .post(format!("{}/v1/abuse-reports", cli.daemon))
                    .json(&json!({
                        "report": {
                            "reporter_pubkey": reporter_pubkey,
                            "subject_pubkey": subject_pubkey,
                            "category": category,
                            "note": note,
                            "reported_at_unix": now_unix(),
                        },
                    })),
                &management_token,
            ))?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Command::ListAbuseReports { json } => {
            let reports = request_json(auth(
                client
                    .get(format!("{}/v1/abuse-reports", cli.daemon))
                    .header("Accept", "application/json"),
                &management_token,
            ))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                let items = reports.as_array().map_or(&[] as &[Value], Vec::as_slice);
                if items.is_empty() {
                    println!("no abuse reports");
                } else {
                    println!(
                        "{:<20}  {:<20}  {:<16}  {:<12}  note",
                        "reporter", "subject", "category", "age"
                    );
                    println!("{}", "-".repeat(90));
                    let now = dllm_protocol::now_unix();
                    for r in items {
                        let reporter = r["reporter_pubkey"]
                            .as_array()
                            .map(|a| format_hex(a))
                            .unwrap_or_else(|| "unknown".into());
                        let subject = r["subject_pubkey"]
                            .as_array()
                            .map(|a| format_hex(a))
                            .unwrap_or_else(|| "unknown".into());
                        let cat = r["category"].as_str().unwrap_or("");
                        let ts = r["reported_at_unix"].as_u64().unwrap_or(0);
                        let age = if ts > 0 && ts < now {
                            format_age(now - ts)
                        } else {
                            "just now".into()
                        };
                        let note = r["note"].as_str().unwrap_or("");
                        println!("{reporter:<20}  {subject:<20}  {cat:<16}  {age:<12}  {note}");
                    }
                }
            }
        }
        Command::AuditLog { since, limit, json } => {
            let mut url = format!("{}/v1/audit-log", cli.daemon);
            let mut sep = "?";
            if let Some(since) = since {
                url.push_str(&format!("{sep}since={since}"));
                sep = "&";
            }
            if let Some(limit) = limit {
                url.push_str(&format!("{sep}limit={limit}"));
            }
            let entries = request_json(auth(
                client.get(url).header("Accept", "application/json"),
                &management_token,
            ))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                let items = entries.as_array().map_or(&[] as &[Value], Vec::as_slice);
                if items.is_empty() {
                    println!("no audit entries");
                } else {
                    println!(
                        "{:<12}  {:<20}  {:<24}  {:<24}  outcome",
                        "timestamp", "actor", "action", "target"
                    );
                    println!("{}", "-".repeat(100));
                    for e in items {
                        let ts = e["timestamp_unix"].as_u64().unwrap_or(0);
                        let ts_str = if ts > 0 { format!("{ts}") } else { "?".into() };
                        let actor = e["actor"].as_str().unwrap_or("");
                        let action = e["action"].as_str().unwrap_or("");
                        let target = e["target"].as_str().unwrap_or("");
                        let outcome = e["outcome"].as_str().unwrap_or("");
                        println!(
                            "{ts_str:<12}  {actor:<20}  {action:<24}  {target:<24}  {outcome}"
                        );
                    }
                }
            }
        }
        Command::PeerStatus { json } => {
            let status: Value = request_json(auth(
                client.get(format!("{}/v1/peer-network/status", cli.daemon)),
                &management_token,
            ))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                let enabled = status["enabled"].as_bool().unwrap_or(false);
                if !enabled {
                    println!("peer network disabled");
                    return Ok(());
                }
                let pid = status["peer_id"].as_str().unwrap_or("unknown");
                let disc = status["discovery_mode"].as_str().unwrap_or("unknown");
                let dht = if status["dht_hosting"].as_bool().unwrap_or(false) {
                    "server"
                } else {
                    "client"
                };
                let fwd = if status["forwarding_enabled"].as_bool().unwrap_or(false) {
                    "eligible"
                } else {
                    "ineligible"
                };
                let boot_count = status["bootstrap_peers"].as_array().map_or(0, Vec::len);
                let disc_count = status["discovered_providers"]
                    .as_array()
                    .map_or(0, Vec::len);
                let fwd_peer = status["selected_forwarder"]
                    .as_str()
                    .unwrap_or("none, direct");
                let path = status["path"].as_str().unwrap_or("unknown");
                let reserved = if status["reservation_active"].as_bool().unwrap_or(false) {
                    "active"
                } else {
                    "none"
                };
                let failed = status["failed_connections"].as_u64().unwrap_or(0);
                let reselections = status["reselections"].as_u64().unwrap_or(0);
                let active_in = status["active_inbound_streams"].as_u64().unwrap_or(0);
                let active_out = status["active_outbound_streams"].as_u64().unwrap_or(0);
                let rejected = status["rejected_streams"].as_u64().unwrap_or(0);
                let cancelled = status["cancelled_streams"].as_u64().unwrap_or(0);
                let deadlines = status["deadline_expirations"].as_u64().unwrap_or(0);
                let proto_fail = status["protocol_failures"].as_u64().unwrap_or(0);
                let auth_fail = status["auth_failures"].as_u64().unwrap_or(0);
                let last_err = status["last_error"].as_str().unwrap_or("none");
                let published = if status["published_discovery"].as_bool().unwrap_or(false) {
                    "yes"
                } else {
                    "no"
                };

                println!("peer id              {pid}");
                println!("discovery            {disc} (forwarding published: {published})");
                println!("dht hosting          {dht}");
                println!("forwarding           {fwd}");
                println!("bootstrap peers      {boot_count}");
                println!("discovered peers     {disc_count}");
                println!("selected forwarder   {fwd_peer}");
                println!("path                 {path}");
                println!("reservation          {reserved}");
                println!();
                println!("streams              inbound {active_in}  outbound {active_out}");
                println!("rejected/cancelled   {rejected} / {cancelled}");
                println!("deadline expirations {deadlines}");
                println!("protocol failures    {proto_fail}");
                println!("auth failures        {auth_fail}");
                println!("failed connections   {failed}");
                println!("reselections         {reselections}");
                println!("last error           {last_err}");
            }
        }
        Command::TransferOwner {
            new_owner_key,
            old_owner_endpoint,
        } => {
            let mut store = NetworkStore::load(&state, &owner_key)?;
            let new_owner_key = NetworkStore::load_owner_key(new_owner_key)?;
            store.transfer_owner(new_owner_key, old_owner_endpoint)?;
            store.save(&state)?;
            store.save_owner_key(&owner_key)?;
            println!(
                "transferred authority at generation {}",
                store.state.state.generation
            );
        }
    }
    Ok(())
}

fn resolve_path(
    explicit: Option<PathBuf>,
    default: impl FnOnce() -> std::io::Result<PathBuf>,
) -> std::io::Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path),
        None => default(),
    }
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
        _ => Err("select exactly one assignment target: --authority or NODE_KEY".into()),
    }
}

fn model_add_source(
    source: &str,
    name: Option<String>,
) -> Result<(String, Value), Box<dyn std::error::Error>> {
    let path = PathBuf::from(source);
    if path.exists() {
        let id = name
            .or_else(|| {
                path.file_stem()
                    .map(|value| value.to_string_lossy().into_owned())
            })
            .ok_or("local model path has no file name; pass --name")?;
        return Ok((id, json!({ "type": "local", "path": path })));
    }
    if !source.contains('/') {
        return Err("model source must be a Hugging Face model ID like Qwen/Qwen2.5-0.5B-Instruct or a local GGUF path".into());
    }
    let repo = if source.to_ascii_uppercase().ends_with("-GGUF") {
        source.to_owned()
    } else {
        format!("{source}-GGUF")
    };
    let id = name.unwrap_or_else(|| {
        source
            .rsplit('/')
            .next()
            .unwrap_or(source)
            .trim_end_matches("-GGUF")
            .to_ascii_lowercase()
    });
    Ok((
        id,
        json!({ "type": "hugging_face", "repo": repo, "file": null }),
    ))
}

fn read_key(path: PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(NetworkStore::load_owner_key(path)?
        .verifying_key()
        .to_bytes()
        .to_vec())
}

fn read_public_key_arg(value: PathBuf) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if value.exists() {
        return read_key(value);
    }
    let value = value
        .to_str()
        .ok_or("node public key must be UTF-8 or an existing key path")?;
    if value.len() != 64 {
        return Err(
            "node public key must be 64 hexadecimal characters or an existing key path".into(),
        );
    }
    let bytes = hex::decode(value)?;
    let _: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "node public key must contain 32 bytes")?;
    Ok(bytes)
}

fn request_json(
    builder: reqwest::blocking::RequestBuilder,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = builder.send()?;
    if !response.status().is_success() {
        let status = response.status();
        let message = response.text()?;
        return Err(format!("daemon returned {status}: {message}").into());
    }
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

fn format_hex(bytes: &[serde_json::Value]) -> String {
    bytes
        .iter()
        .take(4)
        .map(|v| format!("{:02x}", v.as_u64().unwrap_or(0)))
        .collect::<Vec<_>>()
        .join("")
}

fn format_age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::{model_add_source, resolve_path};
    use std::path::PathBuf;

    #[test]
    fn explicit_path_wins_over_default() {
        let resolved = resolve_path(Some(PathBuf::from("explicit")), || {
            Ok(PathBuf::from("default"))
        })
        .unwrap();
        assert_eq!(resolved, PathBuf::from("explicit"));
    }

    #[test]
    fn falls_back_to_default_when_unset() {
        let resolved = resolve_path(None, || Ok(PathBuf::from("default"))).unwrap();
        assert_eq!(resolved, PathBuf::from("default"));
    }

    #[test]
    fn hugging_face_model_id_resolves_to_gguf_repo() {
        let (id, source) = model_add_source("Qwen/Qwen2.5-0.5B-Instruct", None).unwrap();
        assert_eq!(id, "qwen2.5-0.5b-instruct");
        assert_eq!(
            source["repo"],
            serde_json::json!("Qwen/Qwen2.5-0.5B-Instruct-GGUF")
        );
    }

    #[test]
    fn explicit_repo_derives_model_name() {
        let (id, _) = model_add_source("Qwen/Qwen2.5-0.5B-Instruct-GGUF", None).unwrap();
        assert_eq!(id, "qwen2.5-0.5b-instruct");
    }

    #[test]
    fn embedding_model_uses_same_hugging_face_format() {
        let (id, source) = model_add_source("Qwen/Qwen3-Embedding-0.6B", None).unwrap();
        assert_eq!(id, "qwen3-embedding-0.6b");
        assert_eq!(
            source["repo"],
            serde_json::json!("Qwen/Qwen3-Embedding-0.6B-GGUF")
        );
    }
}
