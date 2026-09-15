//! TEMPORARY diagnosis probe (not part of the product; delete after diagnosis).
//!
//! Reproduces `RemoteIrohControllerTransport::connect`'s endpoint setup and dial
//! against a real host so the failure can be observed with iroh internals
//! visible:
//!
//! - same `presets::N0` + `RelayMode::Custom(host relay)` endpoint
//! - same `MdnsAddressLookup` LAN discovery
//! - same ALPN and `endpoint.connect(addr, ALPN)` call
//!
//! Env:
//!   PROBE_MODE      dial | mdns        (default: dial)
//!   PROBE_NODE      host node id, hex  (required for dial)
//!   PROBE_RELAY     relay url          (optional)
//!   PROBE_ADDRS     comma-separated sockaddrs (optional)
//!   PROBE_TIMEOUT_S per-dial timeout   (default 4)
//!   PROBE_MDNS      0 to disable the LAN mDNS lookup
//!   PROBE_HOLD_S    seconds to hold the endpoint after a successful dial

use codux_remote_transport as _codux_remote_transport;
use iroh::{Endpoint, EndpointAddr, RelayMap, RelayMode, TransportAddr, Watcher, endpoint::presets};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use std::str::FromStr;
use std::time::{Duration, Instant};

const ALPN: &[u8] = b"/codux/remote/1";


/// The node this probe dials: an explicit `PROBE_NODE`, a freshly minted key
/// nobody serves (`PROBE_BOGUS=1`, for a dial that must time out), or the saved
/// host's real node id.

/// The saved ticket carries its own node identity, so it is dropped whenever the
/// probe dials a node other than the saved host's.
fn dial_ticket(transport: &serde_json::Value) -> String {
    if !env("PROBE_NODE").is_empty() || env("PROBE_BOGUS") == "1" {
        return String::new();
    }
    transport["ticket"].as_str().unwrap_or_default().to_string()
}

fn dial_node_id(transport: &serde_json::Value) -> String {
    let explicit = env("PROBE_NODE");
    if !explicit.is_empty() {
        return explicit;
    }
    if env("PROBE_BOGUS") == "1" {
        let id = iroh::SecretKey::generate().public().to_string();
        println!("bogus node: {id}");
        return id;
    }
    transport["nodeId"].as_str().unwrap_or_default().to_string()
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn enable_local_discovery(endpoint: &Endpoint, enabled: bool) {
    if !enabled {
        println!("mdns: disabled by env");
        return;
    }
    let lookup = match endpoint.address_lookup() {
        Ok(lookup) => lookup,
        Err(error) => {
            println!("mdns: unavailable error={error}");
            return;
        }
    };
    match MdnsAddressLookup::builder().build(endpoint.id()) {
        Ok(mdns) => {
            lookup.add(mdns);
            println!("mdns: enabled");
        }
        Err(error) => println!("mdns: init failed error={error}"),
    }
}

async fn build_endpoint(relay: Option<&str>) -> Result<Endpoint, Box<dyn std::error::Error>> {
    let mut builder = Endpoint::builder(presets::N0);
    if let Some(relay) = relay {
        let relay_url = iroh::RelayUrl::from_str(relay)?;
        builder = builder.relay_mode(RelayMode::Custom(RelayMap::from(relay_url)));
        println!("relay: custom {relay}");
    } else {
        println!("relay: n0 default");
    }
    Ok(builder.alpns(vec![ALPN.to_vec()]).bind().await?)
}

fn selected_path(connection: &iroh::endpoint::Connection) -> String {
    connection
        .paths()
        .iter()
        .filter(|path| path.is_selected())
        .map(|path| format!("{:?}", path.remote_addr()))
        .collect::<Vec<_>>()
        .join(",")
}

fn print_paths(connection: &iroh::endpoint::Connection) {
    for path in connection.paths().iter() {
        println!(
            "path: selected={} remote_addr={:?}",
            path.is_selected(),
            path.remote_addr()
        );
    }
}

fn dial_addr(node: &str, relay: Option<&str>, addrs: &[String]) -> Result<EndpointAddr, String> {
    let node_id = iroh::EndpointId::from_str(node).map_err(|e| format!("bad node id: {e}"))?;
    let mut addr = EndpointAddr::new(node_id);
    if let Some(relay) = relay {
        let relay_url = iroh::RelayUrl::from_str(relay).map_err(|e| format!("bad relay: {e}"))?;
        addr = addr.with_relay_url(relay_url);
    }
    let parsed: Vec<TransportAddr> = addrs
        .iter()
        .filter_map(|value| value.parse::<std::net::SocketAddr>().ok())
        .map(TransportAddr::Ip)
        .collect();
    Ok(addr.with_addrs(parsed))
}

async fn dial_mode() -> Result<(), Box<dyn std::error::Error>> {
    let node = env("PROBE_NODE");
    let relay = env("PROBE_RELAY");
    let relay = (!relay.is_empty()).then_some(relay);
    let addrs: Vec<String> = env("PROBE_ADDRS")
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    let timeout = Duration::from_secs(env("PROBE_TIMEOUT_S").parse().unwrap_or(4));
    let mdns = env("PROBE_MDNS") != "0";
    let hold = Duration::from_secs(env("PROBE_HOLD_S").parse().unwrap_or(0));

    let endpoint = build_endpoint(relay.as_deref()).await?;
    println!("endpoint: {}", endpoint.id());
    enable_local_discovery(&endpoint, mdns);

    let started = Instant::now();
    let online = tokio::time::timeout(Duration::from_secs(12), endpoint.online()).await;
    println!(
        "online: {:?} elapsed_ms={}",
        online,
        started.elapsed().as_millis()
    );
    let home = endpoint
        .home_relay_status()
        .get()
        .into_iter()
        .map(|status| format!("{}({})", status.url(), status.is_connected()))
        .collect::<Vec<_>>();
    println!("home_relays: {home:?}");

    let addr = dial_addr(&node, relay.as_deref(), &addrs)?;
    println!("dial target: {addr:?}");
    let started = Instant::now();
    match tokio::time::timeout(timeout, endpoint.connect(addr, ALPN)).await {
        Ok(Ok(connection)) => {
            println!(
                "dial: OK elapsed_ms={} remote_addr={:?}",
                started.elapsed().as_millis(),
                selected_path(&connection)
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
            print_paths(&connection);
            if !hold.is_zero() {
                println!("holding for {:?}...", hold);
                tokio::time::sleep(hold).await;
                print_paths(&connection);
            }
            connection.close(0u32.into(), b"probe done");
        }
        Ok(Err(error)) => println!(
            "dial: ERR elapsed_ms={} error={error}",
            started.elapsed().as_millis()
        ),
        Err(_) => println!(
            "dial: TIMEOUT elapsed_ms={}",
            started.elapsed().as_millis()
        ),
    }
    endpoint.close().await;
    Ok(())
}

async fn mdns_mode() -> Result<(), Box<dyn std::error::Error>> {
    let hold = Duration::from_secs(env("PROBE_HOLD_S").parse().unwrap_or(30));
    let first = build_endpoint(None).await?;
    enable_local_discovery(&first, true);
    let peer = env("PROBE_RESOLVE");
    let second = if peer.is_empty() {
        let second = build_endpoint(None).await?;
        enable_local_discovery(&second, true);
        Some(second)
    } else {
        println!("resolving remote node {peer} (no local second endpoint)");
        None
    };
    let target = match second.as_ref() {
        Some(second) => second.id(),
        None => iroh::EndpointId::from_str(&peer)?,
    };
    println!("resolve: {} -> {}", first.id(), target);
    for second_tick in 0..hold.as_secs() {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let lookup = first.address_lookup()?;
        let mut found = Vec::new();
        {
            use futures_util::StreamExt;
            let mut stream = std::pin::pin!(lookup.resolve(target));
            while let Ok(Some(item)) = tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
                found.push(format!("{item:?}"));
            }
        }
        println!("t={}s found={found:?}", second_tick + 1);
    }
    first.close().await;
    if let Some(second) = second {
        second.close().await;
    }
    Ok(())
}


/// Mimics the desktop controller's reconnect loop on ONE pooled endpoint:
/// dial with the product's 4s/6s/8s schedule, hold the connection, watch it
/// die, then dial again — without ever rebuilding the endpoint.
async fn loop_mode() -> Result<(), Box<dyn std::error::Error>> {
    let node = env("PROBE_NODE");
    let relay = env("PROBE_RELAY");
    let relay = (!relay.is_empty()).then_some(relay);
    let addrs: Vec<String> = env("PROBE_ADDRS")
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    let dials: usize = env("PROBE_DIALS").parse().unwrap_or(5);
    let hold = Duration::from_secs(env("PROBE_HOLD_S").parse().unwrap_or(60));
    let fresh_each = env("PROBE_FRESH") == "1";

    let mut endpoint = build_endpoint(relay.as_deref()).await?;
    println!("endpoint: {}", endpoint.id());
    enable_local_discovery(&endpoint, env("PROBE_MDNS") != "0");
    let addr = dial_addr(&node, relay.as_deref(), &addrs)?;
    println!("dial target: {addr:?}");
    println!("self addr: {:?}", endpoint.addr());

    for round in 1..=dials {
        if fresh_each && round > 1 {
            endpoint.close().await;
            endpoint = build_endpoint(relay.as_deref()).await?;
            enable_local_discovery(&endpoint, env("PROBE_MDNS") != "0");
        }
        let mut connection = None;
        for attempt in 0..3u32 {
            let timeout = Duration::from_secs(4 + 2 * attempt as u64);
            let started = Instant::now();
            match tokio::time::timeout(timeout, endpoint.connect(addr.clone(), ALPN)).await {
                Ok(Ok(established)) => {
                    println!(
                        "round={round} dial=OK attempt={} elapsed_ms={}",
                        attempt + 1,
                        started.elapsed().as_millis()
                    );
                    connection = Some(established);
                    break;
                }
                Ok(Err(error)) => println!(
                    "round={round} dial=ERR attempt={} elapsed_ms={} error={error}",
                    attempt + 1,
                    started.elapsed().as_millis()
                ),
                Err(_) => println!(
                    "round={round} dial=TIMEOUT attempt={} elapsed_ms={}",
                    attempt + 1,
                    started.elapsed().as_millis()
                ),
            }
        }
        let Some(connection) = connection else {
            println!("round={round} verdict=FAILED_ALL_DIALS");
            continue;
        };
        for path in connection.paths().iter() {
            println!("round={round} path selected={} remote={:?}", path.is_selected(), path.remote_addr());
        }
        let node_id = iroh::EndpointId::from_str(&node)?;
        if let Some(info) = endpoint.remote_info(node_id).await {
            let addrs: Vec<String> = info
                .addrs()
                .map(|entry| format!("{:?}/{:?}", entry.addr(), entry.usage()))
                .collect();
            println!("round={round} peer_info: {addrs:?}");
        } else {
            println!("round={round} peer_info: none");
        }
        let node_id = iroh::EndpointId::from_str(&node)?;
        for tick in 0..(hold.as_secs() / 5).max(1) {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let mut paths = Vec::new();
            for path in connection.paths().iter() {
                paths.push(format!(
                    "{}:{:?}",
                    if path.is_selected() { "SELECTED" } else { "open" },
                    path.remote_addr()
                ));
            }
            let peer = match endpoint.remote_info(node_id).await {
                Some(info) => info
                    .addrs()
                    .map(|entry| format!("{:?}", entry.addr()))
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            };
            println!("round={round} t={}s paths={paths:?} peer_addrs={peer:?}", (tick + 1) * 5);
        }
        let closed = connection.closed();
        let watched = tokio::time::timeout(Duration::from_secs(1), closed).await;
        match watched {
            Ok(result) => println!("round={round} closed reason={result:?} after_ms<{}", hold.as_millis()),
            Err(_) => println!("round={round} still_open after {}s", hold.as_secs()),
        }
        let stats = connection.stats();
        println!(
            "round={round} stats udp_tx={} udp_rx={} lost={} rtt={:?}",
            stats.udp_tx.datagrams, stats.udp_rx.datagrams, stats.lost_packets, stats.lost_bytes
        );
        connection.close(0u32.into(), b"probe round done");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    endpoint.close().await;
    Ok(())
}


/// Drives the PRODUCT's controller entry point (`RemoteTransportFactory::
/// connect_controller`) with the desktop's own saved host record, then drops and
/// reconnects — the same loop the desktop runs.
async fn product_mode() -> Result<(), Box<dyn std::error::Error>> {
    use codux_remote_transport::{
        RemoteControllerTransportConfig, RemoteTransportCandidate, RemoteTransportFactory,
        RemoteTransportStateHandler,
    };
    use std::sync::Arc;

    let path = std::env::var("PROBE_HOSTS_JSON").unwrap_or_else(|_| {
        "/Users/mac/Library/Application Support/Codux/remote-controllers.json".to_string()
    });
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let hosts = value.as_array().ok_or("expected an array of saved hosts")?;
    let host = hosts
        .iter()
        .find(|host| {
            host.get("hostId").and_then(|v| v.as_str()) == Some(env("PROBE_HOST_ID").as_str())
        })
        .or_else(|| hosts.first())
        .ok_or("no saved host")?;
    let transport = &host["transports"][0];
    let config = RemoteControllerTransportConfig {
        relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
        host_id: host["hostId"].as_str().unwrap_or_default().to_string(),
        device_id: host["deviceId"].as_str().unwrap_or_default().to_string(),
        device_token: host["deviceToken"].as_str().unwrap_or_default().to_string(),
        transports: vec![RemoteTransportCandidate {
            kind: "iroh".to_string(),
            url: String::new(),
            node_id: dial_node_id(transport),
            relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
            ticket: dial_ticket(transport),
            relay_authentication: transport["relayAuthentication"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }],
        direct_addrs: transport["directAddrs"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };
    println!(
        "host={} device={} relay={} addrs={:?}",
        config.host_id, config.device_id, config.relay_url, config.direct_addrs
    );

    let rounds: usize = env("PROBE_DIALS").parse().unwrap_or(6);
    let hold = Duration::from_secs(env("PROBE_HOLD_S").parse().unwrap_or(30));
    let gap = Duration::from_secs(env("PROBE_GAP_S").parse().unwrap_or(2));

    for round in 1..=rounds {
        let on_log = Arc::new(|message: String| println!("[log] {message}"));
        let on_state: RemoteTransportStateHandler =
            Arc::new(|device: String, state: String| println!("[state] {device} {state}"));
        let on_message = Arc::new(|_source: String, data: Vec<u8>| {
            println!("[message] {} bytes", data.len());
        });
        let started = Instant::now();
        match RemoteTransportFactory::connect_controller(&config, on_message, on_state, Some(on_log))
            .await
        {
            Ok(transport) => {
                println!(
                    "round={round} connect=OK elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                tokio::time::sleep(hold).await;
                println!("round={round} closing after {}s", hold.as_secs());
                transport.shutdown().await;
            }
            Err(error) => println!(
                "round={round} connect=ERR elapsed_ms={} error={error}",
                started.elapsed().as_millis()
            ),
        }
        tokio::time::sleep(gap).await;
    }
    Ok(())
}


/// Connects through the PRODUCT's controller entry point (retrying patiently),
/// then asks the HOST, over the web tunnel, for its own uptime via
/// `http://127.0.0.1:8765/health` — first-hand evidence about the host side.
async fn tunnel_mode() -> Result<(), Box<dyn std::error::Error>> {
    use codux_remote_transport::{
        RemoteControllerTransportConfig, RemoteTransportCandidate, RemoteTransportFactory,
        RemoteTransportStateHandler, WebTunnelTcpConnectRequest,
    };
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let path = std::env::var("PROBE_HOSTS_JSON").unwrap_or_else(|_| {
        "/Users/mac/Library/Application Support/Codux/remote-controllers.json".to_string()
    });
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let hosts = value.as_array().ok_or("expected an array of saved hosts")?;
    let host = hosts.first().ok_or("no saved host")?;
    let transport = &host["transports"][0];
    let device_id = host["deviceId"].as_str().unwrap_or_default().to_string();
    let device_token = host["deviceToken"].as_str().unwrap_or_default().to_string();
    let config = RemoteControllerTransportConfig {
        relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
        host_id: host["hostId"].as_str().unwrap_or_default().to_string(),
        device_id: device_id.clone(),
        device_token: device_token.clone(),
        transports: vec![RemoteTransportCandidate {
            kind: "iroh".to_string(),
            url: String::new(),
            node_id: dial_node_id(transport),
            relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
            ticket: dial_ticket(transport),
            relay_authentication: transport["relayAuthentication"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }],
        direct_addrs: transport["directAddrs"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };

    let patience_min: u64 = env("PROBE_PATIENCE_MIN").parse().unwrap_or(20);
    let deadline = Instant::now() + Duration::from_secs(patience_min * 60);
    let mut attempt = 0;
    let live = loop {
        attempt += 1;
        if Instant::now() > deadline {
            println!("gave up after {attempt} attempts");
            return Ok(());
        }
        let on_state: RemoteTransportStateHandler =
            Arc::new(|device: String, state: String| println!("[state] {device} {state}"));
        let started = Instant::now();
        match RemoteTransportFactory::connect_controller(
            &config,
            Arc::new(|_source: String, _data: Vec<u8>| {}),
            on_state,
            Some(Arc::new(|message: String| println!("[log] {message}"))),
        )
        .await
        {
            Ok(transport) => {
                println!(
                    "attempt={attempt} connect=OK elapsed_ms={}",
                    started.elapsed().as_millis()
                );
                break transport;
            }
            Err(error) => println!(
                "attempt={attempt} connect=ERR elapsed_ms={} error={error}",
                started.elapsed().as_millis()
            ),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };

    for path in ["/ping", "/health"] {
        match live
            .web_tunnel_tcp_connect(WebTunnelTcpConnectRequest {
                device_id: device_id.clone(),
                device_token: device_token.clone(),
                host: "127.0.0.1".to_string(),
                port: 8765,
            })
            .await
        {
            Ok(mut stream) => {
                let request = format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8765\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(request.as_bytes()).await;
                let mut raw = Vec::new();
                let read = tokio::time::timeout(
                    Duration::from_secs(20),
                    stream.read_to_end(&mut raw),
                )
                .await;
                let text = String::from_utf8_lossy(&raw).to_string();
                println!(
                    "tunnel {path} read={:?} bytes={} body_head={:?}",
                    read.map(|r| r.is_ok()),
                    raw.len(),
                    text.chars().take(600).collect::<String>()
                );
            }
            Err(error) => println!("tunnel {path} ERR {error}"),
        }
    }

    let hold = Duration::from_secs(env("PROBE_HOLD_S").parse().unwrap_or(30));
    println!("holding {}s to watch the link", hold.as_secs());
    tokio::time::sleep(hold).await;
    live.shutdown().await;
    Ok(())
}


/// Control experiment: two endpoints on THIS machine, both registered at the
/// given relay, dialing each other through it. Isolates "our leg + relay" from
/// "the remote host's leg".
async fn relaypair_mode() -> Result<(), Box<dyn std::error::Error>> {
    let relay = env("PROBE_RELAY");
    let relay = (!relay.is_empty()).then_some(relay);
    let dials: usize = env("PROBE_DIALS").parse().unwrap_or(6);

    let a = build_endpoint(relay.as_deref()).await?;
    let b = build_endpoint(relay.as_deref()).await?;
    println!("pair: a={} b={}", a.id(), b.id());
    for endpoint in [&a, &b] {
        let online = tokio::time::timeout(Duration::from_secs(12), endpoint.online()).await;
        let home = endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .map(|status| format!("{}({})", status.url(), status.is_connected()))
            .collect::<Vec<_>>();
        println!("online={online:?} home={home:?}");
    }
    let mut addr = EndpointAddr::new(b.id());
    if let Some(relay) = relay.as_deref() {
        addr = addr.with_relay_url(iroh::RelayUrl::from_str(relay)?);
    }
    println!("dial target: {addr:?}");
    for round in 1..=dials {
        let mut live = None;
        for attempt in 0..3u32 {
            let timeout = Duration::from_secs(4 + 2 * attempt as u64);
            let started = Instant::now();
            match tokio::time::timeout(timeout, a.connect(addr.clone(), ALPN)).await {
                Ok(Ok(connection)) => {
                    println!(
                        "round={round} dial=OK attempt={} elapsed_ms={} path={:?}",
                        attempt + 1,
                        started.elapsed().as_millis(),
                        connection
                            .paths()
                            .iter()
                            .filter(|path| path.is_selected())
                            .map(|path| format!("{:?}", path.remote_addr()))
                            .collect::<Vec<_>>()
                    );
                    live = Some(connection);
                    break;
                }
                Ok(Err(error)) => println!("round={round} dial=ERR attempt={} {error}", attempt + 1),
                Err(_) => println!(
                    "round={round} dial=TIMEOUT attempt={} elapsed_ms={}",
                    attempt + 1,
                    started.elapsed().as_millis()
                ),
            }
        }
        match live {
            Some(connection) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                connection.close(0u32.into(), b"done");
            }
            None => println!("round={round} verdict=FAILED_ALL_DIALS"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    a.close().await;
    b.close().await;
    Ok(())
}


/// Watches reachability over time on ONE long-lived endpoint: a single 8s dial
/// every `PROBE_INTERVAL_S`, printing the endpoint's own relay state each round.
/// Distinguishes "our leg is down" from "the peer is unreachable".
async fn watch_mode() -> Result<(), Box<dyn std::error::Error>> {
    let node = env("PROBE_NODE");
    let relay = env("PROBE_RELAY");
    let relay = (!relay.is_empty()).then_some(relay);
    let addrs: Vec<String> = env("PROBE_ADDRS")
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    let rounds: usize = env("PROBE_DIALS").parse().unwrap_or(40);
    let interval = Duration::from_secs(env("PROBE_INTERVAL_S").parse().unwrap_or(10));
    let timeout = Duration::from_secs(env("PROBE_TIMEOUT_S").parse().unwrap_or(8));
    let node_id = iroh::EndpointId::from_str(&node)?;
    let endpoint = build_endpoint(relay.as_deref()).await?;
    enable_local_discovery(&endpoint, env("PROBE_MDNS") != "0");
    let addr = dial_addr(&node, relay.as_deref(), &addrs)?;
    // Judge the peer, not this endpoint: wait for our own relay registration
    // first, so a slow registration cannot be reported as "host unreachable".
    let online = tokio::time::timeout(Duration::from_secs(15), endpoint.online()).await;
    println!("online={:?}", online.is_ok());
    let mut ok = 0;
    for round in 1..=rounds {
        let home = endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .map(|status| format!("{}({})", status.url(), status.is_connected()))
            .collect::<Vec<_>>();
        let paths: Vec<String> = endpoint
            .remote_info(node_id)
            .await
            .map(|info| {
                info.addrs()
                    .map(|entry| format!("{:?}/{:?}", entry.addr(), entry.usage()))
                    .collect()
            })
            .unwrap_or_default();
        let started = Instant::now();
        let outcome = match tokio::time::timeout(timeout, endpoint.connect(addr.clone(), ALPN)).await
        {
            Ok(Ok(connection)) => {
                let selected = connection
                    .paths()
                    .iter()
                    .filter(|path| path.is_selected())
                    .map(|path| format!("{:?}", path.remote_addr()))
                    .collect::<Vec<_>>();
                connection.close(0u32.into(), b"watch done");
                ok += 1;
                format!("OK path={selected:?}")
            }
            Ok(Err(error)) => format!("ERR {error}"),
            Err(_) => "TIMEOUT".to_string(),
        };
        println!(
            "t={round} elapsed_ms={} home={home:?} peer_addrs={paths:?} dial={outcome}",
            started.elapsed().as_millis()
        );
        tokio::time::sleep(interval).await;
    }
    println!("summary: ok={ok}/{rounds}");
    endpoint.close().await;
    Ok(())
}


/// Connects through the PRODUCT's controller entry point, then asks the HOST for
/// its own files over the codux protocol (`file.list` / `file.read`) — the
/// host's agent log is the one place its relay behaviour is recorded.
async fn readfile_mode() -> Result<(), Box<dyn std::error::Error>> {
    use codux_remote_transport::{
        RemoteControllerTransportConfig, RemoteTransportCandidate, RemoteTransportFactory,
        RemoteTransportStateHandler,
    };
    use std::sync::{Arc, Mutex};

    let path = std::env::var("PROBE_HOSTS_JSON").unwrap_or_else(|_| {
        "/Users/mac/Library/Application Support/Codux/remote-controllers.json".to_string()
    });
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let hosts = value.as_array().ok_or("expected an array of saved hosts")?;
    let host = hosts.first().ok_or("no saved host")?;
    let transport = &host["transports"][0];
    let device_id = host["deviceId"].as_str().unwrap_or_default().to_string();
    let config = RemoteControllerTransportConfig {
        relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
        host_id: host["hostId"].as_str().unwrap_or_default().to_string(),
        device_id: device_id.clone(),
        device_token: host["deviceToken"].as_str().unwrap_or_default().to_string(),
        transports: vec![RemoteTransportCandidate {
            kind: "iroh".to_string(),
            url: String::new(),
            node_id: dial_node_id(transport),
            relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
            ticket: dial_ticket(transport),
            relay_authentication: transport["relayAuthentication"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }],
        direct_addrs: transport["directAddrs"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };

    let inbox: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(env("PROBE_PATIENCE_MIN").parse().unwrap_or(10) * 60);
    let live = loop {
        if Instant::now() > deadline {
            println!("gave up connecting");
            return Ok(());
        }
        let on_state: RemoteTransportStateHandler =
            Arc::new(|_device: String, _state: String| {});
        let sink = Arc::clone(&inbox);
        match RemoteTransportFactory::connect_controller(
            &config,
            Arc::new(move |_source: String, data: Vec<u8>| {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
                    sink.lock().unwrap().push(value);
                }
            }),
            on_state,
            None,
        )
        .await
        {
            Ok(transport) => break transport,
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    };
    println!("connected");

    let mut next_id = 1u64;
    let mut request = |kind: &str, payload: serde_json::Value| -> Result<serde_json::Value, String> {
        let id = next_id.to_string();
        next_id += 1;
        let envelope = serde_json::json!({
            "type": kind,
            "deviceId": device_id,
            "requestId": id,
            "payload": payload,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
        if !live.send(bytes, None) {
            return Err(format!("send failed for {kind}"));
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() > deadline {
                return Err(format!("timeout waiting for {kind}"));
            }
            let found = {
                let mut inbox = inbox.lock().unwrap();
                inbox
                    .iter()
                    .position(|message| message.get("requestId").and_then(|v| v.as_str()) == Some(id.as_str()))
                    .map(|index| inbox.remove(index))
            };
            if let Some(message) = found {
                return Ok(message);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    for dir in env("PROBE_LIST_DIRS").split(';').filter(|value| !value.trim().is_empty()) {
        match request("file.list", serde_json::json!({ "path": dir.trim() })) {
            Ok(reply) => {
                let entries = reply
                    .pointer("/payload/entries")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let filter = env("PROBE_LIST_FILTER");
                println!("file.list {dir} ({} entries, filter={filter:?})", entries.len());
                for entry in entries {
                    let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                    if filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase()) {
                        println!(
                            "  {} size={} dir={}",
                            entry.get("path").and_then(|v| v.as_str()).unwrap_or_default(),
                            entry.get("size").and_then(|v| v.as_u64()).unwrap_or_default(),
                            entry.get("isDirectory").and_then(|v| v.as_bool()).unwrap_or_default()
                        );
                    }
                }
            }
            Err(error) => println!("file.list {dir} -> ERR {error}"),
        }
    }

    for kind in env("PROBE_REQUESTS").split(';').filter(|value| !value.trim().is_empty()) {
        let kind = kind.trim();
        match request(kind, serde_json::json!({})) {
            Ok(reply) => println!(
                "request {kind} -> {}",
                serde_json::to_string(&reply).unwrap_or_default().chars().take(2500).collect::<String>()
            ),
            Err(error) => println!("request {kind} -> ERR {error}"),
        }
    }

    let target = env("PROBE_READ_PATH");
    if env("PROBE_BLOB") == "1" && !target.is_empty() {
        match request("file.readBlob", serde_json::json!({ "path": target })) {
            Ok(reply) => {
                let raw = serde_json::to_string(&reply).unwrap_or_default();
                println!("readBlob head: {}", raw.chars().take(300).collect::<String>());
                if let Some(ticket) = reply.pointer("/payload/ticket").and_then(|v| v.as_str()) {
                    let started = Instant::now();
                    match live.fetch_blob(ticket).await {
                        Ok(bytes) => {
                            println!(
                                "blob fetched bytes={} elapsed_ms={}",
                                bytes.len(),
                                started.elapsed().as_millis()
                            );
                            let text = String::from_utf8_lossy(&bytes).to_string();
                            let tail: String = text.chars().rev().take(6000).collect::<Vec<_>>().into_iter().rev().collect();
                            println!("--- tail ---\n{tail}");
                        }
                        Err(error) => println!("fetch_blob ERR {error}"),
                    }
                }
            }
            Err(error) => println!("readBlob ERR {error}"),
        }
    }
    if env("PROBE_BLOB") != "1" && !target.is_empty() {
        match request("file.read", serde_json::json!({ "path": target })) {
            Ok(reply) => {
                let raw = serde_json::to_string(&reply).unwrap_or_default();
                println!("raw reply head: {}", raw.chars().take(500).collect::<String>());
                let content = reply
                    .get("payload")
                    .and_then(|payload| payload.get("content"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                println!("file.read {target} -> {} chars", content.len());
                let tail: String = content.chars().rev().take(6000).collect::<Vec<_>>().into_iter().rev().collect();
                println!("--- tail ---\n{tail}");
            }
            Err(error) => println!("file.read {target} -> ERR {error}"),
        }
    }
    live.shutdown().await;
    Ok(())
}


/// Connects through the PRODUCT's controller entry point and drives a shell on
/// the HOST through the product's own terminal protocol — the only way to read
/// the host's view (its 72MB agent log is over the file.read cap).
async fn hostlog_mode() -> Result<(), Box<dyn std::error::Error>> {
    use codux_remote_transport::{
        RemoteControllerTransportConfig, RemoteTransportCandidate, RemoteTransportFactory,
        RemoteTransportStateHandler,
    };
    use std::sync::{Arc, Mutex};

    let path = std::env::var("PROBE_HOSTS_JSON").unwrap_or_else(|_| {
        "/Users/mac/Library/Application Support/Codux/remote-controllers.json".to_string()
    });
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let hosts = value.as_array().ok_or("expected an array of saved hosts")?;
    let host = hosts.first().ok_or("no saved host")?;
    let transport = &host["transports"][0];
    let device_id = host["deviceId"].as_str().unwrap_or_default().to_string();
    let config = RemoteControllerTransportConfig {
        relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
        host_id: host["hostId"].as_str().unwrap_or_default().to_string(),
        device_id: device_id.clone(),
        device_token: host["deviceToken"].as_str().unwrap_or_default().to_string(),
        transports: vec![RemoteTransportCandidate {
            kind: "iroh".to_string(),
            url: String::new(),
            node_id: dial_node_id(transport),
            relay_url: transport["relayUrl"].as_str().unwrap_or_default().to_string(),
            ticket: dial_ticket(transport),
            relay_authentication: transport["relayAuthentication"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        }],
        direct_addrs: transport["directAddrs"]
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    };

    let inbox: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(env("PROBE_PATIENCE_MIN").parse().unwrap_or(10) * 60);
    let live = loop {
        if Instant::now() > deadline {
            println!("gave up connecting");
            return Ok(());
        }
        let on_state: RemoteTransportStateHandler = Arc::new(|_device: String, _state: String| {});
        let sink = Arc::clone(&inbox);
        match RemoteTransportFactory::connect_controller(
            &config,
            Arc::new(move |_source: String, data: Vec<u8>| {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) {
                    sink.lock().unwrap().push(value);
                }
            }),
            on_state,
            None,
        )
        .await
        {
            Ok(transport) => break transport,
            Err(_) => tokio::time::sleep(Duration::from_secs(3)).await,
        }
    };
    println!("connected");

    let mut next_id = 1u64;
    let mut request = |kind: &str, payload: serde_json::Value| -> Result<serde_json::Value, String> {
        let id = next_id.to_string();
        next_id += 1;
        let envelope = serde_json::json!({
            "type": kind,
            "deviceId": device_id,
            "requestId": id,
            "payload": payload,
        });
        let bytes = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
        if !live.send(bytes, None) {
            return Err(format!("send failed for {kind}"));
        }
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if Instant::now() > deadline {
                return Err(format!("timeout waiting for {kind}"));
            }
            let found = {
                let mut inbox = inbox.lock().unwrap();
                inbox
                    .iter()
                    .position(|message| {
                        message.get("requestId").and_then(|v| v.as_str()) == Some(id.as_str())
                    })
                    .map(|index| inbox.remove(index))
            };
            if let Some(message) = found {
                return Ok(message);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };

    let terminal_id = format!("probe-{}", std::process::id());
    let created = request(
        "terminal.create",
        serde_json::json!({
            "cwd": "/Users/mac",
            "command": "/bin/zsh",
            "cols": 200,
            "rows": 60,
            "terminalId": terminal_id,
            "title": "probe",
        }),
    )?;
    let session = created
        .pointer("/payload/id")
        .or_else(|| created.pointer("/payload/sessionId"))
        .or_else(|| created.pointer("/sessionId"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    println!("terminal={session}");

    // Write the command's output to a host file and read that file back: the
    // PTY's streamed screen is viewport-shaped and came back empty, while
    // `file.read` returns the text verbatim (and the tail is far under the
    // 2MB cap, even though the log itself is not).
    let outfile = "/tmp/codux-probe-host.log";
    let command = env("PROBE_HOST_CMD");
    let command = if command.is_empty() {
        "tail -n 120 /Users/mac/.codux-agent/codux.log".to_string()
    } else {
        command
    };
    let command = format!("{command} > {outfile} 2>&1");
    // Let the PTY's shell come up before typing into it: input sent into a
    // still-starting shell is echoed but never executed.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let input = serde_json::json!({
        "type": "terminal.input",
        "deviceId": device_id,
        "sessionId": session,
        "payload": { "sessionId": session, "data": format!("{command}\n") },
    });
    let _ = live.send(serde_json::to_vec(&input)?, None);
    // The command writes to `outfile`; wait for it, then fetch the file itself.
    tokio::time::sleep(Duration::from_secs(6)).await;
    match request("file.read", serde_json::json!({ "path": outfile })) {
        Ok(reply) => {
            let text = reply
                .pointer("/payload/content")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            println!("--- host file {outfile} ({} chars) ---\n{text}", text.len());
        }
        Err(error) => println!("file.read {outfile} -> ERR {error}"),
    }
    let buffer_request = serde_json::json!({
        "type": "terminal.buffer",
        "deviceId": device_id,
        "sessionId": session,
        "payload": { "sessionId": session, "offset": 0, "maxChars": 200000, "tail": true },
    });
    let _ = live.send(serde_json::to_vec(&buffer_request)?, None);

    let wait = Duration::from_secs(env("PROBE_READ_S").parse().unwrap_or(8));
    let started = Instant::now();
    let mut output = String::new();
    while started.elapsed() < wait {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut inbox = inbox.lock().unwrap();
        let mut keep = Vec::new();
        for message in inbox.drain(..) {
            let is_output = message.get("type").and_then(|v| v.as_str()) == Some("terminal.output")
                && message.get("sessionId").and_then(|v| v.as_str()) == Some(session.as_str());
            if env("PROBE_DUMP_ALL") == "1" {
                let raw = serde_json::to_string(&message).unwrap_or_default();
                if raw.len() > 4000 && !raw.contains("terminal.output") {
                    println!("[msg] {}", raw.chars().take(300).collect::<String>());
                } else {
                    println!("[msg] {}", raw.chars().take(1200).collect::<String>());
                }
            }
            let is_buffer = message.get("type").and_then(|v| v.as_str()) == Some("terminal.buffer")
                && message.get("sessionId").and_then(|v| v.as_str()) == Some(session.as_str());
            if is_buffer {
                let text = message
                    .pointer("/payload/data")
                    .or_else(|| message.pointer("/payload/buffer"))
                    .or_else(|| message.pointer("/payload/text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                output.push_str(text);
                continue;
            }
            if is_output {
                if let Some(data) = message.pointer("/payload/data").and_then(|v| v.as_str()) {
                    output.push_str(data);
                }
            } else {
                keep.push(message);
            }
        }
        *inbox = keep;
    }

    let _ = request("terminal.close", serde_json::json!({ "sessionId": session }));
    println!("--- host output ({} chars) ---\n{}", output.len(), output);
    live.shutdown().await;
    Ok(())
}


/// Watches this endpoint's OWN relay connection over time: if the home relay
/// goes connected -> disconnected -> connected on a fixed cadence, the relay's
/// front (nginx idle timeout) or the relay server is dropping sessions, which
/// strands the peer on that relay for as long as it takes to re-register.
async fn relaywatch_mode() -> Result<(), Box<dyn std::error::Error>> {
    let relay = env("PROBE_RELAY");
    let relay = (!relay.is_empty()).then_some(relay);
    let seconds: u64 = env("PROBE_SECONDS").parse().unwrap_or(360);
    let endpoint = build_endpoint(relay.as_deref()).await?;
    println!("endpoint={} watching own relay for {seconds}s", endpoint.id());
    let mut previous = String::new();
    let mut drops = 0;
    for tick in 0..seconds {
        let state = endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .map(|status| format!("{}({})", status.url(), status.is_connected()))
            .collect::<Vec<_>>()
            .join(",");
        if state != previous {
            if previous.contains("(true)") && !state.contains("(true)") {
                drops += 1;
            }
            println!("t={tick}s home=[{state}] (change #{})", drops);
            previous = state;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    println!("summary: relay disconnects seen = {drops} in {seconds}s");
    endpoint.close().await;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env("PROBE_MODE").as_str() {
        "mdns" => mdns_mode().await,
        "loop" => loop_mode().await,
        "product" => product_mode().await,
        "tunnel" => tunnel_mode().await,
        "relaypair" => relaypair_mode().await,
        "watch" => watch_mode().await,
        "readfile" => readfile_mode().await,
        "hostlog" => hostlog_mode().await,
        "relaywatch" => relaywatch_mode().await,
        _ => dial_mode().await,
    }
}
