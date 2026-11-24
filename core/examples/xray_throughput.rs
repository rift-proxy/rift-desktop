//! Offline loopback TCP/UDP throughput of the real two-core runtime. No user profile or network.
use rift_core::{
    dns::Chain,
    model::Store,
    parser::parse_uri,
    runtime::{Runtime, Sidecar},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    process::Command,
    time::{sleep, timeout},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ID: &str = "11111111-1111-4111-8111-111111111111";
const SECRET: &str = "synthetic-throughput-api-secret-01234567890123456789";
fn invalid() -> std::io::Error {
    std::io::Error::other("synthetic throughput verification failed")
}
fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
fn core(name: &str) -> Result<(PathBuf, &'static str)> {
    let (triple, suffix, hash) = match (cfg!(windows), name) {
        (true, "sing-box") => (
            "x86_64-pc-windows-msvc",
            ".exe",
            "b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909",
        ),
        (true, "xray") => (
            "x86_64-pc-windows-msvc",
            ".exe",
            "c77a00eb84fb93ae044c2d771a1cabcc033b81e487253fbc35b12df6879c1e15",
        ),
        (false, "sing-box") => (
            "x86_64-unknown-linux-gnu",
            "",
            "9334a1c1fe97234911afaedd37226667ebdb3dab2abe383f2c4ae1387706dad9",
        ),
        (false, "xray") => (
            "x86_64-unknown-linux-gnu",
            "",
            "a27e3f95ab6ce270956d0cecd32eda4686a811afc71c64a6a734a3dede14883e",
        ),
        _ => return Err(invalid().into()),
    };
    // nosemgrep: rust.lang.security.args-os.args-os -- Only explicit argv[1]; the selected file is digest-verified before execution, never trust argv[0].
    let path = match std::env::args_os().nth(1) {
        Some(dir) => PathBuf::from(dir).join(format!("{name}{suffix}")),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../desktop/src-tauri/binaries/{name}-{triple}{suffix}"
        )),
    };
    if format!("{:x}", Sha256::digest(std::fs::read(&path)?)) != hash {
        return Err(invalid().into());
    }
    Ok((path, hash))
}
async fn socks(proxy: u16, command: u8, destination: u16) -> Result<(TcpStream, [u8; 10])> {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, proxy)).await?;
    stream.write_all(&[5, 1, 0]).await?;
    let mut auth = [0; 2];
    stream.read_exact(&mut auth).await?;
    if auth != [5, 0] {
        return Err(invalid().into());
    }
    let mut request = vec![5, command, 0, 1, 127, 0, 0, 1];
    request.extend(destination.to_be_bytes());
    stream.write_all(&request).await?;
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).await?;
    if reply[..4] != [5, 0, 0, 1] {
        return Err(invalid().into());
    }
    Ok((stream, reply))
}
async fn tcp_sample(stream: TcpStream) -> Result<Value> {
    // Pipeline writes and reads: stop-and-wait would measure delayed ACK latency, not bulk rate.
    let (mut read, mut write) = stream.into_split();
    let started = Instant::now();
    let sent = async {
        let mut bytes = vec![b'q'; 65536];
        for index in 0u32..256 {
            bytes[..4].copy_from_slice(&index.to_be_bytes());
            write.write_all(&bytes).await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let received = async {
        let mut bytes = vec![0; 65536];
        for index in 0u32..256 {
            read.read_exact(&mut bytes).await?;
            if bytes[..4] != index.to_be_bytes() || bytes[4..].iter().any(|byte| *byte != b'q') {
                return Err(invalid());
            }
        }
        Ok::<(), std::io::Error>(())
    };
    timeout(Duration::from_secs(15), async {
        tokio::try_join!(sent, received)
    })
    .await??;
    let seconds = started.elapsed().as_secs_f64();
    Ok(
        json!({"payload_bytes_each_direction":16777216,"elapsed_ms":seconds*1000.0,"one_direction_mbps":16777216.0*8.0/seconds/1e6}),
    )
}
async fn udp_sample(destination: u16, proxy: Option<u16>) -> Result<Value> {
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let (control, relay) = if let Some(proxy) = proxy {
        let (stream, reply) = socks(proxy, 3, 0).await?;
        (
            Some(stream),
            std::net::SocketAddr::from((
                [reply[4], reply[5], reply[6], reply[7]],
                u16::from_be_bytes([reply[8], reply[9]]),
            )),
        )
    } else {
        (
            None,
            std::net::SocketAddr::from(([127, 0, 0, 1], destination)),
        )
    };
    let mut body = vec![b'u'; 1200];
    let mut received = [0; 2048];
    let started = Instant::now();
    for index in 0u32..1024 {
        body[..4].copy_from_slice(&index.to_be_bytes());
        let mut request = if control.is_some() {
            vec![0, 0, 0, 1, 127, 0, 0, 1]
        } else {
            Vec::new()
        };
        if control.is_some() {
            request.extend(destination.to_be_bytes());
        }
        request.extend_from_slice(&body);
        socket.send_to(&request, relay).await?;
        let (size, from) =
            timeout(Duration::from_secs(3), socket.recv_from(&mut received)).await??;
        if from != relay || received[..size] != request {
            return Err(invalid().into());
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    Ok(
        json!({"packets_each_direction":1024,"payload_bytes_per_packet":1200,"elapsed_ms":seconds*1000.0,"round_trips_per_second":1024.0/seconds}),
    )
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let (sb, sb_hash) = core("sing-box")?;
    let (xr, xr_hash) = core("xray")?;
    let tcp = TcpListener::bind("127.0.0.1:0").await?;
    let tcp_port = tcp.local_addr()?.port();
    let tcp_task = tokio::spawn(async move {
        while let Ok((stream, _)) = tcp.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.into_split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let udp = UdpSocket::bind("127.0.0.1:0").await?;
    let udp_port = udp.local_addr()?.port();
    let udp_task = tokio::spawn(async move {
        let mut bytes = [0; 2048];
        while let Ok((n, from)) = udp.recv_from(&mut bytes).await {
            let _ = udp.send_to(&bytes[..n], from).await;
        }
    });
    let remote_port = port()?;
    let mut remote = Command::new(&xr)
        .args(["run", "-c", "stdin:", "-format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let config = json!({"log":{"loglevel":"none"},"inbounds":[{"listen":"127.0.0.1","port":remote_port,"protocol":"vless","settings":{"clients":[{"id":ID}],"decryption":"none"}}],"outbounds":[{"protocol":"freedom","settings":{"finalRules":[{"action":"allow","ip":["127.0.0.0/8"]},{"action":"block"}]}}]});
    remote
        .stdin
        .take()
        .ok_or_else(invalid)?
        .write_all(&serde_json::to_vec(&config)?)
        .await?;
    timeout(Duration::from_secs(5), async {
        loop {
            if TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, remote_port))
                .await
                .is_ok()
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let dir = tempfile::tempdir()?;
    let mut store = Store::default();
    store.settings.api_port = port()?;
    loop {
        store.settings.proxy_port = port()?;
        if store.settings.api_port != store.settings.proxy_port {
            break;
        }
    }
    store.routing.direct.clear();
    store.servers.push(parse_uri(
        &format!("vless://{ID}@127.0.0.1:{remote_port}?security=none&type=tcp"),
        None,
    )?);
    store.selected = Some(store.servers[0].id.clone());
    let mut runtime = Sidecar::new(sb, dir.path().into())
        .with_xray_binary(xr)
        .with_chain(|| Chain::new(Vec::new()));
    runtime.start(&store, SECRET).await?;
    let mut samples = Vec::new();
    for run in 1..=3 {
        let direct =
            tcp_sample(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, tcp_port)).await?)
                .await?;
        let (stream, _) = socks(store.settings.proxy_port, 1, tcp_port).await?;
        let proxied = tcp_sample(stream).await?;
        let direct_udp = udp_sample(udp_port, None).await?;
        let proxied_udp = udp_sample(udp_port, Some(store.settings.proxy_port)).await?;
        samples.push(json!({"run":run,"direct_tcp":direct,"two_core_tcp":proxied,"direct_udp":direct_udp,"two_core_udp":proxied_udp}));
    }
    runtime.stop().await?;
    remote.kill().await?;
    remote.wait().await?;
    tcp_task.abort();
    udp_task.abort();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schema":1,"platform":std::env::consts::OS,"scope":"offline loopback core throughput; not Tauri/TUN or internet performance","sing_box_sha256":sb_hash,"xray_sha256":xr_hash,"samples":samples})
        )?
    );
    Ok(())
}
