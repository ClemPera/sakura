use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use crate::{Result, YeelightError};

const MULTICAST_ADDR: &str = "239.255.255.250:1982";

// Must use double quotes around the MAN value as per the spec.
const SEARCH_MSG: &str = "M-SEARCH * HTTP/1.1\r\n\
                           HOST: 239.255.255.250:1982\r\n\
                           MAN: \"ssdp:discover\"\r\n\
                           ST: wifi_bulb\r\n\
                           \r\n";

/// Information returned by a Yeelight device during discovery.
#[derive(Debug, Clone)]
pub struct DiscoveredDevice {
    /// TCP address to pass to [`YeelightClient::connect`].
    pub addr: SocketAddr,
    pub id: Option<String>,
    pub model: Option<String>,
    pub fw_ver: Option<String>,
    pub power: Option<bool>,
    pub brightness: Option<u8>,
    pub name: Option<String>,
    /// List of method names supported by this device.
    pub supported_methods: Vec<String>,
}

/// Send an SSDP M-SEARCH broadcast and collect all responding Yeelight
/// devices within `timeout`.
///
/// Returns an empty `Vec` (not an error) if no devices responded.
///
/// # ESP32 note
/// `join_multicast_v4` is not required for *sending* to the multicast
/// address — a plain unicast-capable UDP socket is enough.
pub fn discover(timeout: Duration) -> Result<Vec<DiscoveredDevice>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(timeout))?;
    socket.send_to(SEARCH_MSG.as_bytes(), MULTICAST_ADDR)?;

    let mut devices: Vec<DiscoveredDevice> = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        match socket.recv_from(&mut buf) {
            Ok((n, _src)) => {
                let raw = std::str::from_utf8(&buf[..n]).unwrap_or("");
                if let Some(dev) = parse_response(raw) {
                    // De-duplicate by address in case we receive multiple ads.
                    if !devices.iter().any(|d| d.addr == dev.addr) {
                        devices.push(dev);
                    }
                }
            }
            // Timeout = we have collected all responses in the window.
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(e) => return Err(YeelightError::Io(e)),
        }
    }

    Ok(devices)
}

// ------------------------------------------------------------------ internals

fn parse_response(response: &str) -> Option<DiscoveredDevice> {
    // First line must be exactly "HTTP/1.1 200 OK"
    if response.lines().next()?.trim() != "HTTP/1.1 200 OK" {
        return None;
    }

    let mut location: Option<String> = None;
    let mut id: Option<String> = None;
    let mut model: Option<String> = None;
    let mut fw_ver: Option<String> = None;
    let mut power: Option<bool> = None;
    let mut brightness: Option<u8> = None;
    let mut name: Option<String> = None;
    let mut supported_methods: Vec<String> = Vec::new();

    for line in response.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Split on the first ':' only (value may itself contain colons, e.g. the Location URI).
        let (key, value) = line.split_once(':')?;
        let key = key.trim().to_lowercase();
        let value = value.trim();

        match key.as_str() {
            "location" => location = Some(value.to_string()),
            "id"       => id       = Some(value.to_string()),
            "model"    => model    = Some(value.to_string()),
            "fw_ver"   => fw_ver   = Some(value.to_string()),
            "power"    => power    = Some(value == "on"),
            "bright"   => brightness = value.parse().ok(),
            "name"     => name     = Some(value.to_string()),
            "support"  => {
                supported_methods =
                    value.split_whitespace().map(str::to_string).collect();
            }
            _ => {}
        }
    }

    // Location is "yeelight://ip:port" — strip scheme, parse remainder as SocketAddr.
    let location = location?;
    let addr_str = location.strip_prefix("yeelight://")?;
    let addr: SocketAddr = addr_str.parse().ok()?;

    Some(DiscoveredDevice {
        addr,
        id,
        model,
        fw_ver,
        power,
        brightness,
        name,
        supported_methods,
    })
}
