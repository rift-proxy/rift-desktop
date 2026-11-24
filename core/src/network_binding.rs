//! Resolve the physical interface before creating our own TUN. Names stay in process/config;
//! errors/diagnostics never include adapter names, addresses or underlying system messages.
use crate::text::t;
use crate::{failure::FailureCode, Error, Result};

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 1024
        || name.chars().any(char::is_control)
        || name.eq_ignore_ascii_case("Rift")
    {
        return Err(unavailable());
    }
    Ok(())
}
fn unavailable() -> Error {
    Error::failure(
        FailureCode::XrayStartExit,
        t("core.network_binding.interface_failed"),
    )
}
#[cfg(windows)]
pub(crate) fn physical_interface() -> Result<String> {
    let interface = netdev::get_default_interface().map_err(|_| unavailable())?;
    if interface.is_loopback() || !interface.is_up() {
        return Err(unavailable());
    }
    // Go net.InterfaceByName on Windows uses FriendlyName, not AdapterName GUID.
    let name = interface.friendly_name.unwrap_or(interface.name);
    validate_name(&name)?;
    Ok(name)
}
#[cfg(not(windows))]
pub(crate) fn physical_interface() -> Result<String> {
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_tun_and_invalid_interfaces_cannot_become_a_transport_binding() {
        for name in ["", "Rift", "rift", "eth\n0"] {
            assert!(validate_name(name).is_err());
        }
        assert!(validate_name(&"a".repeat(1025)).is_err());
        for name in ["Ethernet", "Wi-Fi", "Сеть 2", "enp4s0"] {
            assert!(validate_name(name).is_ok());
        }
    }
}
