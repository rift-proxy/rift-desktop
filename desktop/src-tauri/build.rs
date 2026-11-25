use std::path::Path;

/// The twelve leading digits of the explicit full build commit, if valid.
fn short_commit(value: Option<&str>) -> Option<String> {
    let commit = value?.trim();
    (commit.len() == 40
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| commit[..12].to_owned())
}

fn const_decl(name: &str, value: Option<&str>) -> String {
    let value = match value {
        Some(value) => format!("Some({value:?})"),
        None => "None".to_owned(),
    };
    format!("const {name}: Option<&str> = {value};\n")
}

fn write_const(out_dir: &Path, file_name: &str, const_name: &str, value: Option<&str>) {
    std::fs::write(out_dir.join(file_name), const_decl(const_name, value))
        .unwrap_or_else(|_| panic!("cannot write generated {file_name}"));
}

/// The single updater feed; absent or explicitly blank means disabled.
fn update_endpoint(value: Option<String>) -> Result<Option<String>, &'static str> {
    let Some(endpoint) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let valid = url::Url::parse(endpoint.trim()).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path().rsplit('/').next() == Some("latest.json")
    });
    if !valid {
        return Err("RIFT_UPDATE_ENDPOINT must be HTTPS and end in latest.json");
    }
    Ok(Some(endpoint.trim().to_owned()))
}

fn native_link_arg(target_env: Option<&str>, debug: Option<&str>) -> Option<&'static str> {
    (target_env == Some("msvc") && debug == Some("false")).then_some("/DEBUG:NONE")
}

#[cfg(not(test))]
fn main() {
    if let Some(argument) = native_link_arg(
        std::env::var("CARGO_CFG_TARGET_ENV").ok().as_deref(),
        std::env::var("DEBUG").ok().as_deref(),
    ) {
        println!("cargo:rustc-link-arg={argument}");
    }
    println!("cargo:rerun-if-env-changed=RIFT_UPDATE_ENDPOINT");
    println!("cargo:rerun-if-env-changed=RIFT_BUILD_COMMIT");
    let endpoint = update_endpoint(std::env::var("RIFT_UPDATE_ENDPOINT").ok())
        .unwrap_or_else(|message| panic!("{message}"));
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR is not set");
    let out_dir = Path::new(&out_dir);
    write_const(
        out_dir,
        "update_endpoint.rs",
        "RIFT_UPDATE_ENDPOINT",
        endpoint.as_deref(),
    );
    write_const(
        out_dir,
        "build_commit.rs",
        "RIFT_BUILD_COMMIT",
        short_commit(std::env::var("RIFT_BUILD_COMMIT").ok().as_deref()).as_deref(),
    );
    tauri_build::build()
}

#[cfg(test)]
mod tests {
    use super::{const_decl, native_link_arg, short_commit, update_endpoint};

    #[test]
    fn msvc_release_disables_pdb_without_changing_debug_or_non_msvc_builds() {
        assert_eq!(
            native_link_arg(Some("msvc"), Some("false")),
            Some("/DEBUG:NONE")
        );
        for (target_env, debug) in [
            (Some("msvc"), Some("true")),
            (Some("gnu"), Some("false")),
            (Some("musl"), Some("false")),
            (None, Some("false")),
            (Some("msvc"), None),
        ] {
            assert_eq!(native_link_arg(target_env, debug), None);
        }
    }

    #[test]
    fn updater_is_off_without_an_explicit_endpoint() {
        assert_eq!(update_endpoint(None).unwrap(), None);
        assert_eq!(update_endpoint(Some(String::new())).unwrap(), None);
        assert_eq!(update_endpoint(Some("  ".into())).unwrap(), None);
    }

    #[test]
    fn updater_accepts_only_the_single_https_feed() {
        assert_eq!(
            update_endpoint(Some(" https://updates.example/rift/latest.json ".into()))
                .unwrap()
                .as_deref(),
            Some("https://updates.example/rift/latest.json")
        );
        for endpoint in [
            "http://updates.example/latest.json",
            "https://updates.example/other.json",
            "https://user@updates.example/latest.json",
            "https://user:secret@updates.example/latest.json",
            "https://updates.example/latest.json?token=secret",
            "https://updates.example/latest.json#fragment",
            "not-a-url",
        ] {
            assert!(update_endpoint(Some(endpoint.into())).is_err());
        }
    }

    #[test]
    fn only_a_full_commit_names_a_build() {
        let full = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(short_commit(Some(full)).as_deref(), Some("0123456789ab"));
        assert_eq!(
            short_commit(Some(&format!(" {full}\n"))).as_deref(),
            Some("0123456789ab")
        );
        for bad in [
            "",
            "0123456789ab",
            "ZZZZ456789abcdef0123456789abcdef01234567",
        ] {
            assert_eq!(short_commit(Some(bad)), None, "{bad}");
        }
        assert_eq!(short_commit(None), None);
    }

    #[test]
    fn generated_source_escapes_values_without_a_cargo_directive() {
        let source = const_decl(
            "RIFT_UPDATE_ENDPOINT",
            Some("https://example/quoted\"value"),
        );
        assert_eq!(
            source,
            "const RIFT_UPDATE_ENDPOINT: Option<&str> = Some(\"https://example/quoted\\\"value\");\n"
        );
        assert!(!source.contains("cargo:rustc-env"));
        assert_eq!(
            const_decl("RIFT_UPDATE_ENDPOINT", None),
            "const RIFT_UPDATE_ENDPOINT: Option<&str> = None;\n"
        );
    }
}
