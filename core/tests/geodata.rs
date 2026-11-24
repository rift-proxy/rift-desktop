//! The geosite and geoip databases: reading the file, resolving the lists a profile asks for,
//! installing a download, and fetching it with its checksum.
use proptest::prelude::*;
use rift_core::{
    failure::FailureCode,
    geodata::{Fetched, GeoCache, GeoSource, HttpGeoSource, Kind, SiteList, MAX_DB_BYTES},
    routing::{Route, Routing},
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

mod common;
use common::*;

fn profile(sites: &[&str], ips: &[&str]) -> Routing {
    let mut direct: Vec<String> = sites.iter().map(|name| format!("geosite:{name}")).collect();
    direct.extend(ips.iter().map(|code| format!("geoip:{code}")));
    Routing {
        order: [Route::Block, Route::Direct, Route::Proxy],
        block: vec![],
        direct,
        proxy: vec![],
    }
}
fn cache_with(sites: Option<Vec<u8>>, ips: Option<Vec<u8>>) -> (GeoCache, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cache = GeoCache::open(dir.path().join("geodata"));
    if let Some(bytes) = sites {
        cache.install(Kind::Geosite, &bytes).unwrap();
    }
    if let Some(bytes) = ips {
        cache.install(Kind::Geoip, &bytes).unwrap();
    }
    (cache, dir)
}

// ---- reading ---------------------------------------------------------------------------------

#[test]
fn the_lists_a_profile_asks_for_are_read_by_kind() {
    let (cache, _dir) = cache_with(Some(geosite_file()), Some(geoip_file()));
    let set = cache.resolve(&profile(
        &["category-ru", "geolocation-!cn"],
        &["ru", "private"],
    ));
    assert_eq!(
        set.sites["category-ru"],
        SiteList {
            suffix: vec!["yandex.ru".into(), "vk.com".into()],
            full: vec!["www.example.ru".into()],
            keyword: vec!["bank".into()],
            regex: vec![r"^ad[0-9]+\.".into()],
        }
    );
    assert_eq!(set.sites["geolocation-!cn"].suffix, ["example.org"]);
    assert_eq!(set.ips["ru"], ["5.8.0.0/13", "2a00:1450::/32"]);
    assert_eq!(set.ips["private"], ["10.0.0.0/8", "192.168.0.0/16"]);
    assert!(
        !set.sites.contains_key("google"),
        "only what the profile asked for"
    );
}

#[test]
fn names_are_matched_without_regard_to_case_and_unknown_ones_are_absent() {
    let (cache, _dir) = cache_with(Some(geosite_file()), Some(geoip_file()));
    let set = cache.resolve(&profile(&["Category-RU", "no-such-list"], &["RU", "xx"]));
    assert!(set.sites.contains_key("category-ru") || set.sites.contains_key("Category-RU"));
    assert!(!set.sites.contains_key("no-such-list"));
    assert!(!set.ips.contains_key("xx"));
    assert_eq!(cache.knows(Kind::Geosite, "google"), Some(true));
    assert_eq!(cache.knows(Kind::Geosite, "nope"), Some(false));
    assert_eq!(
        cache.unknown_names(&profile(&["google", "nope"], &["ru", "xx"])),
        ["geosite:nope", "geoip:xx"]
    );
}

#[test]
fn without_a_database_nothing_resolves_and_nothing_is_unknown() {
    let (cache, _dir) = cache_with(None, None);
    let profile = profile(&["category-ru"], &["ru"]);
    let set = cache.resolve(&profile);
    assert!(set.sites.is_empty() && set.ips.is_empty());
    assert_eq!(
        cache.knows(Kind::Geosite, "category-ru"),
        None,
        "no database to ask"
    );
    assert!(
        cache.unknown_names(&profile).is_empty(),
        "nothing can be called unknown yet"
    );
    assert!(!cache.has(Kind::Geosite) && cache.info(Kind::Geoip).is_none());
}

#[test]
fn a_list_that_is_applied_is_read_but_one_that_is_not_is_not() {
    let (cache, _dir) = cache_with(Some(geosite_file()), None);
    let routing = Routing {
        order: [Route::Block, Route::Direct, Route::Proxy],
        proxy: vec!["geosite:google".into()],
        direct: vec!["geosite:category-ru".into()],
        block: vec![],
    };
    let set = cache.resolve(&routing);
    assert!(set.sites.contains_key("category-ru"));
    assert!(
        !set.sites.contains_key("google"),
        "the proxy list is the rest here"
    );
}

#[test]
fn damaged_values_are_skipped_not_trusted() {
    let entries = file(&[
        site(
            "MIXED",
            &[
                (2, "good.example"),
                (2, "bad domain.example"),
                (2, ""),
                (3, "UPPER.Example"),
                (9, "unknown-kind.example"),
            ],
        ),
        ip_entry(
            "BAD",
            &[
                cidr(&[1, 2, 3], 8),
                cidr(&[10, 0, 0, 0], 33),
                cidr(&[10, 0, 0, 0], 8),
            ],
        ),
    ]);
    let (cache, _dir) = cache_with(Some(entries.clone()), Some(entries));
    let set = cache.resolve(&profile(&["mixed"], &["bad"]));
    assert_eq!(set.sites["mixed"].suffix, ["good.example"]);
    assert_eq!(set.sites["mixed"].full, ["upper.example"]);
    assert_eq!(
        set.ips["bad"],
        ["10.0.0.0/8"],
        "a range that is not one is dropped"
    );
}

#[test]
fn info_reports_the_file_that_is_there() {
    let (cache, _dir) = cache_with(Some(geosite_file()), None);
    let info = cache.info(Kind::Geosite).unwrap();
    assert_eq!(info.bytes, geosite_file().len() as u64);
    assert!(info.updated_at > 1_700_000_000);
    assert!(cache.info(Kind::Geoip).is_none());
}

#[test]
fn the_time_a_database_was_last_confirmed_is_kept_beside_the_file() {
    let (cache, dir) = cache_with(Some(geosite_file()), None);
    assert_eq!(cache.checked_at(Kind::Geosite), None, "never confirmed");
    cache.set_checked_at(Kind::Geosite, 1_800_000_000).unwrap();
    assert_eq!(cache.checked_at(Kind::Geosite), Some(1_800_000_000));
    // Another cache over the folder, as after a restart, reads the same.
    let again = GeoCache::open(dir.path().join("geodata"));
    assert_eq!(again.checked_at(Kind::Geosite), Some(1_800_000_000));
    assert_eq!(again.checked_at(Kind::Geoip), None, "per database");
    // Garbage is no time at all, not a reason to fail.
    std::fs::write(
        dir.path().join("geodata").join("geosite.dat.checked"),
        "soon",
    )
    .unwrap();
    assert_eq!(again.checked_at(Kind::Geosite), None);
}

// ---- installing ------------------------------------------------------------------------------

#[test]
fn a_file_that_is_not_a_database_is_refused_and_the_old_one_stays() {
    let (cache, _dir) = cache_with(Some(geosite_file()), None);
    for bad in [
        Vec::new(),
        b"not a database".to_vec(),
        vec![0x0a, 0xff, 0xff, 0xff, 0xff, 0x0f], // a length past the end
        vec![0x0a, 0x80],                         // a varint that never ends
        vec![0x0b],                               // a wire type nobody uses
        file(&[bytes_field(2, b"no code in this entry")]), // an entry without a code
        vec![0; MAX_DB_BYTES + 1],
    ] {
        let error = cache.install(Kind::Geosite, &bad).unwrap_err();
        assert!(
            matches!(error.failure_code(), Some(FailureCode::GeodataInvalid)),
            "{bad:?}"
        );
    }
    assert_eq!(
        cache.knows(Kind::Geosite, "google"),
        Some(true),
        "the database in place is untouched"
    );
}

#[test]
fn a_new_database_is_noticed_by_every_cache_over_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("geodata");
    let writer = GeoCache::open(folder.clone());
    let reader = GeoCache::open(folder);
    writer.install(Kind::Geosite, &geosite_file()).unwrap();
    assert_eq!(reader.knows(Kind::Geosite, "google"), Some(true));
    // Replaced by a database with other lists: the reader reads the new one at once.
    writer
        .install(
            Kind::Geosite,
            &file(&[site("NEWLIST", &[(2, "new.example")])]),
        )
        .unwrap();
    assert_eq!(reader.knows(Kind::Geosite, "google"), Some(false));
    assert_eq!(reader.knows(Kind::Geosite, "newlist"), Some(true));
    assert_eq!(
        reader.resolve(&profile(&["newlist"], &[])).sites["newlist"].suffix,
        ["new.example"]
    );
}

#[test]
fn a_file_damaged_on_disk_counts_as_no_database() {
    let (cache, dir) = cache_with(Some(geosite_file()), None);
    assert!(cache.has(Kind::Geosite));
    std::fs::write(dir.path().join("geodata").join("geosite.dat"), b"garbage").unwrap();
    assert!(!cache.has(Kind::Geosite));
    assert!(cache.resolve(&profile(&["google"], &[])).sites.is_empty());
    assert!(cache.unknown_names(&profile(&["google"], &[])).is_empty());
}

proptest! {
    #[test]
    fn any_bytes_are_read_or_refused_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let dir = tempfile::tempdir().unwrap();
        let cache = GeoCache::open(dir.path().join("geodata"));
        if cache.install(Kind::Geosite, &bytes).is_ok() {
            let _ = cache.resolve(&profile(&["x", "google"], &[]));
        }
        let _ = cache.install(Kind::Geoip, &bytes);
    }
}

// ---- downloading -----------------------------------------------------------------------------

type Routes = HashMap<&'static str, (u16, Vec<u8>)>;
/// Serves fixed answers by path on a loopback port; returns its base URL.
async fn serve(routes: Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let routes = Arc::new(routes);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&head);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = routes
                    .get(path.as_str())
                    .cloned()
                    .unwrap_or((404, Vec::new()));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}
fn sum(data: &[u8], name: &str) -> Vec<u8> {
    format!("{:x}  {name}\n", Sha256::digest(data)).into_bytes()
}

#[tokio::test]
async fn a_download_with_the_right_checksum_is_returned() {
    let data = geosite_file();
    let base = serve(HashMap::from([
        ("/geosite.dat.sha256sum", (200, sum(&data, "geosite.dat"))),
        ("/geosite.dat", (200, data.clone())),
    ]))
    .await;
    let fetched = HttpGeoSource::for_tests(base)
        .fetch(Kind::Geosite, None, None)
        .await
        .unwrap();
    assert_eq!(fetched, Fetched::Latest(data));
}

#[tokio::test]
async fn a_checksum_that_does_not_match_is_refused() {
    let data = geosite_file();
    let other = geoip_file();
    let base = serve(HashMap::from([
        ("/geoip.dat.sha256sum", (200, sum(&other, "geoip.dat"))),
        ("/geoip.dat", (200, data)),
    ]))
    .await;
    let error = HttpGeoSource::for_tests(base)
        .fetch(Kind::Geoip, None, None)
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::GeodataInvalid));
}

#[tokio::test]
async fn a_missing_or_malformed_checksum_and_server_errors_are_refused() {
    let data = geosite_file();
    for (routes, expected) in [
        // No checksum file: nothing is trusted.
        (
            HashMap::from([("/geosite.dat", (200, data.clone()))]),
            FailureCode::GeodataDownload,
        ),
        // A checksum that is not a digest.
        (
            HashMap::from([
                ("/geosite.dat.sha256sum", (200, b"not a digest".to_vec())),
                ("/geosite.dat", (200, data.clone())),
            ]),
            FailureCode::GeodataInvalid,
        ),
        // The file itself fails.
        (
            HashMap::from([
                ("/geosite.dat.sha256sum", (200, sum(&data, "geosite.dat"))),
                ("/geosite.dat", (503, Vec::new())),
            ]),
            FailureCode::GeodataDownload,
        ),
    ] {
        let base = serve(routes).await;
        let error = HttpGeoSource::for_tests(base)
            .fetch(Kind::Geosite, None, None)
            .await
            .unwrap_err();
        assert_eq!(error.failure_code(), Some(expected));
    }
}

#[tokio::test]
async fn a_file_over_the_limit_is_refused_by_its_declared_size() {
    let big = vec![0u8; MAX_DB_BYTES + 1];
    let base = serve(HashMap::from([
        ("/geosite.dat.sha256sum", (200, sum(&big, "geosite.dat"))),
        ("/geosite.dat", (200, big)),
    ]))
    .await;
    let error = HttpGeoSource::for_tests(base)
        .fetch(Kind::Geosite, None, None)
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::GeodataInvalid));
}

#[tokio::test]
async fn a_source_that_wants_https_refuses_a_plain_http_address() {
    let data = geosite_file();
    let base = serve(HashMap::from([
        ("/geosite.dat.sha256sum", (200, sum(&data, "geosite.dat"))),
        ("/geosite.dat", (200, data)),
    ]))
    .await;
    let error = HttpGeoSource::with_base(base, true)
        .fetch(Kind::Geosite, None, None)
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::GeodataDownload));
}

/// Serves like `serve` and counts the requests by path.
async fn serve_counted(routes: Routes) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let routes = Arc::new(routes);
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (routes, log) = (routes.clone(), log.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&head);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                log.lock().unwrap().push(path.clone());
                let (status, body) = routes
                    .get(path.as_str())
                    .cloned()
                    .unwrap_or((404, Vec::new()));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.write_all(&body).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

#[tokio::test]
async fn the_installed_version_is_not_downloaded_again() {
    let data = geosite_file();
    let digest = format!("{:x}", Sha256::digest(&data));
    let (base, seen) = serve_counted(HashMap::from([
        ("/geosite.dat.sha256sum", (200, sum(&data, "geosite.dat"))),
        ("/geosite.dat", (200, data.clone())),
    ]))
    .await;
    let source = HttpGeoSource::for_tests(base);
    assert_eq!(
        source
            .fetch(Kind::Geosite, None, Some(&digest))
            .await
            .unwrap(),
        Fetched::Same
    );
    assert_eq!(
        *seen.lock().unwrap(),
        ["/geosite.dat.sha256sum"],
        "only the one-line checksum was asked for"
    );
    // The upper case of the same digest is the same version.
    assert_eq!(
        source
            .fetch(Kind::Geosite, None, Some(&digest.to_uppercase()))
            .await
            .unwrap(),
        Fetched::Same
    );
    // Another installed version is replaced.
    let old = format!("{:x}", Sha256::digest(b"an older database"));
    assert_eq!(
        source.fetch(Kind::Geosite, None, Some(&old)).await.unwrap(),
        Fetched::Latest(data)
    );
}

#[test]
fn the_digest_of_the_installed_database_is_that_of_its_file() {
    let directory = tempfile::tempdir().unwrap();
    let cache = GeoCache::open(directory.path().to_owned());
    assert_eq!(cache.digest(Kind::Geosite), None, "no database");
    let data = geosite_file();
    cache.install(Kind::Geosite, &data).unwrap();
    assert_eq!(
        cache.digest(Kind::Geosite),
        Some(format!("{:x}", Sha256::digest(&data)))
    );
    // A file that is not a database has no version.
    std::fs::write(cache.path(Kind::Geoip), b"junk").unwrap();
    assert_eq!(cache.digest(Kind::Geoip), None);
}

#[tokio::test]
async fn a_transfer_that_stalls_ends_and_a_slow_one_does_not() {
    let data = geosite_file();
    let sum_line = sum(&data, "geosite.dat");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (slow_data, stall) = (data.clone(), std::sync::atomic::AtomicBool::new(false));
    let stall = Arc::new(stall);
    let flag = stall.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (data, sum_line, stalls) = (slow_data.clone(), sum_line.clone(), flag.clone());
            tokio::spawn(async move {
                let mut head = [0; 1024];
                let n = socket.read(&mut head).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&head[..n]).to_string();
                let body = if head.contains(".sha256sum") {
                    sum_line
                } else {
                    data
                };
                let reply = format!(
                    "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                if head.contains(".sha256sum") {
                    let _ = socket.write_all(&body).await;
                    return;
                }
                // The database arrives in four pieces, each one before the stall limit.
                let quarter = body.len().div_ceil(4);
                for (index, piece) in body.chunks(quarter).enumerate() {
                    let _ = socket.write_all(piece).await;
                    if stalls.load(std::sync::atomic::Ordering::SeqCst) && index == 0 {
                        // Goes quiet after the first piece and does not come back.
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
            });
        }
    });
    let source = HttpGeoSource::for_tests(format!("http://127.0.0.1:{port}"))
        .with_stall(Duration::from_millis(600));
    // Takes 1.2 s, twice the stall limit altogether, but is never quiet for that long.
    assert_eq!(
        source.fetch(Kind::Geosite, None, None).await.unwrap(),
        Fetched::Latest(data)
    );
    stall.store(true, std::sync::atomic::Ordering::SeqCst);
    let started = std::time::Instant::now();
    let error = source.fetch(Kind::Geosite, None, None).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::GeodataDownload));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "ended by the stall limit, not by waiting for the whole transfer"
    );
}
