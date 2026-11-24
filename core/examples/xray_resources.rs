//! Linux-only offline measurements of the real two-core Proxy runtime, never Tauri/Windows.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("xray_resources requires Linux /proc; it cannot verify Windows budgets");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run().await
}

#[cfg(target_os = "linux")]
mod linux {
    use rift_core::{
        dns::Chain,
        model::{Mode, Store},
        parser::parse_uri,
        runtime::{Runtime, Sidecar},
    };
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use std::{
        io,
        path::PathBuf,
        time::{Duration, Instant},
    };

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
    const SECRET: &str = "synthetic-resource-api-secret-01234567890123456789";
    const SB_SHA: &str = "9334a1c1fe97234911afaedd37226667ebdb3dab2abe383f2c4ae1387706dad9";
    const XR_SHA: &str = "a27e3f95ab6ce270956d0cecd32eda4686a811afc71c64a6a734a3dede14883e";

    #[derive(Serialize)]
    struct Usage {
        rss_bytes: u64,
        cpu_ticks: u64,
    }
    #[derive(Serialize)]
    struct Sample {
        candidates: usize,
        run: usize,
        startup_ms: f64,
        idle_interval_ms: f64,
        sing_box: Usage,
        xray: Usage,
        harness: Usage,
    }

    fn binary(name: &str, expected: &str) -> Result<PathBuf> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../desktop/src-tauri/binaries/{name}-x86_64-unknown-linux-gnu"
        ));
        if format!("{:x}", Sha256::digest(std::fs::read(&path)?)) != expected {
            return Err(io::Error::other("unverified resource measurement binary").into());
        }
        Ok(path)
    }

    fn free_ports() -> Result<(u16, u16)> {
        // Keep both reservations alive until both distinct ports are known. Runtime owns
        // binding them afterwards; a conflicting process makes the run fail, never skip.
        let a = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let b = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        Ok((a.local_addr()?.port(), b.local_addr()?.port()))
    }

    fn usage(pid: u32) -> Result<Usage> {
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
        let rss = status
            .lines()
            .find_map(|line| line.strip_prefix("VmRSS:"))
            .ok_or_else(|| io::Error::other("missing child RSS"))?;
        let mut rss = rss.split_whitespace();
        let kb = rss
            .next()
            .ok_or_else(|| io::Error::other("invalid child RSS"))?
            .parse::<u64>()?;
        if rss.next() != Some("kB") {
            return Err(io::Error::other("unknown RSS unit").into());
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields: Vec<&str> = stat
            .rsplit_once(") ")
            .ok_or_else(|| io::Error::other("invalid child stat"))?
            .1
            .split_whitespace()
            .collect();
        let ticks = |index: usize| -> Result<u64> {
            Ok(fields
                .get(index)
                .ok_or_else(|| io::Error::other("missing CPU ticks"))?
                .parse()?)
        };
        Ok(Usage {
            rss_bytes: kb
                .checked_mul(1024)
                .ok_or_else(|| io::Error::other("invalid RSS size"))?,
            cpu_ticks: ticks(11)? + ticks(12)?,
        })
    }

    async fn sample(sb: PathBuf, xr: PathBuf, candidates: usize, run: usize) -> Result<Sample> {
        let (api_port, proxy_port) = free_ports()?;
        let mut store = Store::default();
        store.settings.mode = Mode::Proxy;
        store.settings.api_port = api_port;
        store.settings.proxy_port = proxy_port;
        store.servers = (0..candidates)
            .map(|i| {
                parse_uri(
                    &format!(
                        "vless://11111111-1111-4111-8111-111111111111@127.0.0.1:{}",
                        10000 + i
                    ),
                    None,
                )
            })
            .collect::<rift_core::Result<Vec<_>>>()?;
        let directory = tempfile::tempdir()?;
        let mut runtime = Sidecar::new(sb, directory.path().into())
            .with_xray_binary(xr)
            .with_chain(|| Chain::new(vec![]));
        let started = Instant::now();
        runtime.start(&store, SECRET).await?;
        let startup_ms = started.elapsed().as_secs_f64() * 1000.0;
        let measured = async {
            let sb_pid = runtime
                .pid()
                .ok_or_else(|| io::Error::other("missing sing-box child"))?;
            let xr_pid = runtime
                .xray_pid()
                .ok_or_else(|| io::Error::other("missing Xray child"))?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let sb_before = usage(sb_pid)?;
            let xr_before = usage(xr_pid)?;
            let harness_before = usage(std::process::id())?;
            let idle = Instant::now();
            tokio::time::sleep(Duration::from_secs(2)).await;
            let idle_interval_ms = idle.elapsed().as_secs_f64() * 1000.0;
            let delta = |pid, before: Usage| -> Result<Usage> {
                let mut after = usage(pid)?;
                after.cpu_ticks = after
                    .cpu_ticks
                    .checked_sub(before.cpu_ticks)
                    .ok_or_else(|| io::Error::other("CPU ticks went backwards"))?;
                Ok(after)
            };
            Ok::<_, Box<dyn std::error::Error>>(Sample {
                candidates,
                run,
                startup_ms,
                idle_interval_ms,
                sing_box: delta(sb_pid, sb_before)?,
                xray: delta(xr_pid, xr_before)?,
                harness: delta(std::process::id(), harness_before)?,
            })
        }
        .await;
        // This is also reached when /proc measurement fails. Stop waits for both owned
        // children; no process discovery, process-name matching or foreign-process kill.
        runtime.stop().await?;
        if runtime.pid().is_some() || runtime.xray_pid().is_some() {
            return Err(io::Error::other("owned children were not cleared").into());
        }
        measured
    }

    pub async fn run() -> Result<()> {
        let sb = binary("sing-box", SB_SHA)?;
        let xr = binary("xray", XR_SHA)?;
        let mut samples = Vec::new();
        for candidates in [1, 500, 2000] {
            for run in 1..=3 {
                samples.push(sample(sb.clone(), xr.clone(), candidates, run).await?);
            }
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema":1,
                "scope":"Linux offline Proxy gateway; no remote traffic, Tauri UI, Windows or TUN measurement",
                "sing_box_sha256":SB_SHA,"xray_sha256":XR_SHA,
                "cpu_ticks":"delta of Linux /proc utime+stime; divide by host CLK_TCK for CPU seconds",
                "settling_ms":200,"samples":samples,
                "cleanup":"Runtime::stop awaited for every successful start, including measurement errors"
            }))?
        );
        Ok(())
    }
}
