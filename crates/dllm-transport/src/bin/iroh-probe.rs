use dllm_transport::DLLM_ALPN;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey};
use std::{env, net::SocketAddr, str::FromStr, time::Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("id") if args.len() == 2 => {
            println!("{}", secret(&args[1])?.public());
            Ok(())
        }
        Some("server") if args.len() == 4 => {
            run_server(relay(&args[1])?, secret(&args[2])?, endpoint(&args[3])?).await
        }
        Some("client") if args.len() == 5 || args.len() == 6 => {
            run_client(
                relay(&args[1])?,
                secret(&args[2])?,
                endpoint(&args[3])?,
                args[4].as_bytes(),
                args.get(5).map(|value| value.parse()).transpose()?,
            )
            .await
        }
        _ => Err("usage: iroh-probe id SECRET | server RELAY_URL SECRET ALLOWED_ID | client RELAY_URL SECRET PEER_ID MESSAGE [DIRECT_ADDR]".into()),
    }
}

async fn run_server(
    relay_url: RelayUrl,
    secret_key: SecretKey,
    allowed: EndpointId,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = endpoint_builder(relay_url, secret_key)
        .bind_addr("0.0.0.0:7444")?
        .alpns(vec![DLLM_ALPN.to_vec()])
        .bind()
        .await?;
    tokio::time::timeout(Duration::from_secs(20), endpoint.online()).await?;
    println!("ready endpoint_id={}", endpoint.id());
    loop {
        let incoming = endpoint.accept().await.ok_or("endpoint closed")?;
        let connection = incoming.await?;
        let remote = connection.remote_id();
        if remote != allowed {
            println!("rejected endpoint_id={remote}");
            connection.close(403u32.into(), b"endpoint is not an authorized DLLM member");
            continue;
        }
        let (mut send, mut receive) = connection.accept_bi().await?;
        let request = receive.read_to_end(1024 * 1024).await?;
        send.write_all(&request).await?;
        send.finish()?;
        println!(
            "accepted endpoint_id={remote} bytes={} path={}",
            request.len(),
            path_report(&endpoint, remote).await
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_client(
    relay_url: RelayUrl,
    secret_key: SecretKey,
    peer: EndpointId,
    message: &[u8],
    direct_addr: Option<SocketAddr>,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = endpoint_builder(relay_url.clone(), secret_key)
        .bind()
        .await?;
    tokio::time::timeout(Duration::from_secs(20), endpoint.online()).await?;
    let started = std::time::Instant::now();
    let mut peer_addr = EndpointAddr::new(peer).with_relay_url(relay_url);
    if let Some(direct_addr) = direct_addr {
        peer_addr = peer_addr.with_ip_addr(direct_addr);
    }
    let connection = endpoint.connect(peer_addr, DLLM_ALPN).await?;
    let connected = started.elapsed();
    let (mut send, mut receive) = connection.open_bi().await?;
    send.write_all(message).await?;
    send.finish()?;
    let response = receive.read_to_end(1024 * 1024).await?;
    if response != message {
        return Err("echo response did not match request".into());
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    println!(
        "ok local_id={} peer_id={peer} bytes={} connect_ms={} total_ms={} path={}",
        endpoint.id(),
        response.len(),
        connected.as_millis(),
        started.elapsed().as_millis(),
        path_report(&endpoint, peer).await
    );
    endpoint.close().await;
    Ok(())
}

fn endpoint_builder(relay_url: RelayUrl, secret_key: SecretKey) -> iroh::endpoint::Builder {
    Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(secret_key)
        .relay_mode(RelayMode::Custom(RelayMap::from(relay_url)))
}

async fn path_report(endpoint: &Endpoint, peer: EndpointId) -> String {
    let Some(info) = endpoint.remote_info(peer).await else {
        return "unknown".into();
    };
    info.addrs()
        .map(|info| format!("{:?}:{:?}", info.addr(), info.usage()))
        .collect::<Vec<_>>()
        .join(",")
}

fn relay(value: &str) -> Result<RelayUrl, Box<dyn std::error::Error>> {
    Ok(RelayUrl::from_str(value)?)
}

fn secret(value: &str) -> Result<SecretKey, Box<dyn std::error::Error>> {
    Ok(SecretKey::from_str(value)?)
}

fn endpoint(value: &str) -> Result<EndpointId, Box<dyn std::error::Error>> {
    Ok(EndpointId::from_str(value)?)
}
