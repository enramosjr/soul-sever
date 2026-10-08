//! NAT-PMP, then UPnP, for the listen port.
//!
//! An explicit `upnp_gateway` talks only to that NAT-PMP endpoint. An empty
//! gateway uses the default route, and a failed map falls through to UPnP.
//! Either failure is an event. The server socket stays up.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use super::SessionEvent;

const NATPMP_PORT: u16 = 5351;
const TCP_OPCODE: u8 = 2;
const TCP_RESPONSE: u8 = 130;
/// Requested mapping lifetime. The renewal wait is half of the granted value.
const LIFETIME: u32 = 7200;
const WAIT: Duration = Duration::from_millis(500);
const UPNP_RENEW: Duration = Duration::from_secs(3600);

pub(crate) async fn open_mapping(
    gateway: Option<SocketAddr>,
    port: u16,
    events: mpsc::Sender<SessionEvent>,
    flag: watch::Receiver<bool>,
) {
    match gateway {
        Some(gateway) => maintain(gateway, port, false, events, flag).await,
        None => {
            let discovered = default_gateway().map(|ip| SocketAddr::from((ip, NATPMP_PORT)));
            match discovered {
                Some(gateway) => maintain(gateway, port, true, events, flag).await,
                None => upnp_until_shutdown(port, events, flag).await,
            }
        }
    }
}

async fn maintain(
    gateway: SocketAddr,
    port: u16,
    fallback_upnp: bool,
    events: mpsc::Sender<SessionEvent>,
    mut flag: watch::Receiver<bool>,
) {
    loop {
        match map_once(gateway, port).await {
            Ok(grant) => {
                let _ = events
                    .send(SessionEvent::PortMapped {
                        external: grant.external,
                    })
                    .await;
                let half = u64::from(grant.lifetime / 2).max(1);
                if wait_or_stop(&mut flag, Duration::from_secs(half)).await {
                    break;
                }
            }
            Err(message) => {
                if fallback_upnp && upnp_once(port, &events).await {
                    if wait_or_stop(&mut flag, UPNP_RENEW).await {
                        break;
                    }
                    continue;
                }
                let _ = events.send(SessionEvent::PortMapFailed { message }).await;
                break;
            }
        }
    }
}

async fn upnp_until_shutdown(
    port: u16,
    events: mpsc::Sender<SessionEvent>,
    mut flag: watch::Receiver<bool>,
) {
    loop {
        if !upnp_once(port, &events).await {
            break;
        }
        if wait_or_stop(&mut flag, UPNP_RENEW).await {
            break;
        }
    }
}

/// `true` when the caller should stop.
async fn wait_or_stop(flag: &mut watch::Receiver<bool>, pause: Duration) -> bool {
    tokio::select! {
        changed = flag.changed() => changed.is_ok(),
        _ = tokio::time::sleep(pause) => false,
    }
}

async fn upnp_once(port: u16, events: &mpsc::Sender<SessionEvent>) -> bool {
    match discover_and_add(port).await {
        Ok(()) => {
            let _ = events
                .send(SessionEvent::PortMapped { external: port })
                .await;
            true
        }
        Err(message) => {
            let _ = events.send(SessionEvent::PortMapFailed { message }).await;
            false
        }
    }
}

struct Grant {
    external: u16,
    lifetime: u32,
}

async fn map_once(gateway: SocketAddr, port: u16) -> Result<Grant, String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|err| err.to_string())?;
    sock.connect(gateway).await.map_err(|err| err.to_string())?;
    sock.send(&request_bytes(port, port, LIFETIME))
        .await
        .map_err(|err| err.to_string())?;
    let mut buf = [0u8; 64];
    let count = timeout(WAIT, sock.recv(&mut buf))
        .await
        .map_err(|_| "nat-pmp timed out".to_owned())?
        .map_err(|err| err.to_string())?;
    parse_grant(&buf[..count])
}

fn request_bytes(internal: u16, external: u16, lifetime: u32) -> [u8; 12] {
    let mut bytes = [0u8; 12];
    bytes[1] = TCP_OPCODE;
    bytes[4..6].copy_from_slice(&internal.to_be_bytes());
    bytes[6..8].copy_from_slice(&external.to_be_bytes());
    bytes[8..12].copy_from_slice(&lifetime.to_be_bytes());
    bytes
}

fn parse_grant(bytes: &[u8]) -> Result<Grant, String> {
    if bytes.len() < 16 {
        return Err("nat-pmp reply is short".to_owned());
    }
    if bytes[0] != 0 || bytes[1] != TCP_RESPONSE {
        return Err("nat-pmp reply is not a tcp map".to_owned());
    }
    let result = u16::from_be_bytes([bytes[2], bytes[3]]);
    if result != 0 {
        return Err(format!("nat-pmp result {result}"));
    }
    Ok(Grant {
        external: u16::from_be_bytes([bytes[10], bytes[11]]),
        lifetime: u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
    })
}

pub(crate) fn gateway_from_route_table(text: &str) -> Option<Ipv4Addr> {
    for line in text.lines().skip(1) {
        let mut columns = line.split_whitespace();
        let _iface = columns.next()?;
        let destination = columns.next()?;
        let gateway = columns.next()?;
        if destination == "00000000" {
            return parse_route_address(gateway);
        }
    }
    None
}

fn default_gateway() -> Option<Ipv4Addr> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    gateway_from_route_table(&text)
}

fn parse_route_address(hex_bytes: &str) -> Option<Ipv4Addr> {
    if hex_bytes.len() != 8 {
        return None;
    }
    let value = u32::from_str_radix(hex_bytes, 16).ok()?;
    Some(Ipv4Addr::from(value.to_le_bytes()))
}

async fn discover_and_add(port: u16) -> Result<(), String> {
    let location = ssdp_location().await?;
    let description = http(&location, &get_request(&location)?).await?;
    let control = control_url(&description, &origin(&location)?)
        .ok_or_else(|| "upnp description has no control url".to_owned())?;
    let local = local_ip().unwrap_or(Ipv4Addr::LOCALHOST);
    add_upnp_mapping(&control, &local.to_string(), port).await
}

fn local_ip() -> Option<Ipv4Addr> {
    default_gateway().map(|gateway| {
        std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .and_then(|sock| {
                sock.connect((gateway, NATPMP_PORT))?;
                match sock.local_addr()?.ip() {
                    std::net::IpAddr::V4(ip) => Ok(ip),
                    std::net::IpAddr::V6(_) => Ok(Ipv4Addr::LOCALHOST),
                }
            })
            .unwrap_or(Ipv4Addr::LOCALHOST)
    })
}

const SEARCH: &str = "\
M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: 1\r\n\
ST: urn:schemas-upnp-org:service:WANIPConnection:1\r\n\
\r\n";

async fn ssdp_location() -> Result<String, String> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .map_err(|err| err.to_string())?;
    sock.send_to(SEARCH.as_bytes(), "239.255.255.250:1900")
        .await
        .map_err(|err| err.to_string())?;
    let mut buf = [0u8; 2048];
    let count = timeout(WAIT, sock.recv(&mut buf))
        .await
        .map_err(|_| "upnp discovery timed out".to_owned())?
        .map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&buf[..count]);
    text.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_owned())
        })
        .ok_or_else(|| "upnp discovery has no location".to_owned())
}

pub(crate) fn control_url(xml: &str, origin: &str) -> Option<String> {
    let marker = xml
        .find("WANIPConnection")
        .or_else(|| xml.find("WANPPPConnection"))?;
    let slice = &xml[marker..];
    let start = slice.find("<controlURL>")? + "<controlURL>".len();
    let end = slice[start..].find("</controlURL>")? + start;
    let value = slice[start..end].trim();
    if value.starts_with("http://") {
        Some(value.to_owned())
    } else if value.starts_with('/') {
        Some(format!("{origin}{value}"))
    } else {
        Some(format!("{origin}/{value}"))
    }
}

pub(crate) async fn add_upnp_mapping(
    control: &str,
    local_ip: &str,
    port: u16,
) -> Result<(), String> {
    let body = soap_add(local_ip, port);
    let (host, http_port, path) = split_http(control)?;
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}:{http_port}\r\n\
         Content-Type: text/xml; charset=\"utf-8\"\r\n\
         SOAPAction: \"urn:schemas-upnp-org:service:WANIPConnection:1#AddPortMapping\"\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let response = http(&format!("http://{host}:{http_port}{path}"), &request).await?;
    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        Err(response
            .lines()
            .next()
            .unwrap_or("upnp map failed")
            .to_owned())
    }
}

fn soap_add(local_ip: &str, port: u16) -> String {
    format!(
        "<?xml version=\"1.0\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body>\
         <u:AddPortMapping xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">\
         <NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{port}</NewExternalPort>\
         <NewProtocol>TCP</NewProtocol>\
         <NewInternalPort>{port}</NewInternalPort>\
         <NewInternalClient>{local_ip}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>soul-sever</NewPortMappingDescription>\
         <NewLeaseDuration>{LIFETIME}</NewLeaseDuration>\
         </u:AddPortMapping>\
         </s:Body></s:Envelope>"
    )
}

fn get_request(url: &str) -> Result<String, String> {
    let (host, port, path) = split_http(url)?;
    Ok(format!(
        "GET {path} HTTP/1.0\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    ))
}

fn origin(url: &str) -> Result<String, String> {
    let (host, port, _) = split_http(url)?;
    Ok(format!("http://{host}:{port}"))
}

fn split_http(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "control url must be http".to_owned())?;
    let (hostport, path) = match rest.split_once('/') {
        Some((hostport, path)) => (hostport, format!("/{path}")),
        None => (rest, "/".to_owned()),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) => (
            host.to_owned(),
            port.parse::<u16>()
                .map_err(|_| "bad control port".to_owned())?,
        ),
        None => (hostport.to_owned(), 80),
    };
    if host.is_empty() {
        return Err("control url has no host".to_owned());
    }
    Ok((host, port, path))
}

async fn http(url: &str, request: &str) -> Result<String, String> {
    let (host, port, _) = split_http(url)?;
    let mut stream = timeout(WAIT, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "upnp connect timed out".to_owned())?
        .map_err(|err| err.to_string())?;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let count = timeout(WAIT, stream.read(&mut tmp))
            .await
            .map_err(|_| "upnp read timed out".to_owned())?
            .map_err(|err| err.to_string())?;
        if count == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..count]);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_route_gateway_is_little_endian() {
        let table = "\
Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
eth0 00000000 0101A8C0 0003 0 0 0 00000000 0 0 0
";
        assert_eq!(
            gateway_from_route_table(table),
            Some(Ipv4Addr::new(192, 168, 1, 1))
        );
    }

    #[tokio::test]
    async fn natpmp_map_request_for_2234_is_renewed() {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let gateway = sock.local_addr().unwrap();
        let (shutdown, flag) = watch::channel(false);
        let (tx, mut events) = mpsc::channel(4);
        let task = tokio::spawn(maintain(gateway, 2234, false, tx, flag));
        for _ in 0..2 {
            let mut buf = [0u8; 32];
            let (count, from) = timeout(Duration::from_secs(3), sock.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(count, 12);
            assert_eq!(buf[1], TCP_OPCODE);
            assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 2234);
            let mut reply = [0u8; 16];
            reply[1] = TCP_RESPONSE;
            reply[8..12].copy_from_slice(&buf[4..8]);
            reply[12..16].copy_from_slice(&2u32.to_be_bytes());
            sock.send_to(&reply, from).await.unwrap();
        }
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, SessionEvent::PortMapped { external: 2234 }));
        let _ = shutdown.send(true);
        let _ = timeout(Duration::from_secs(2), task).await;
    }

    #[tokio::test]
    async fn upnp_mapping_requests_port_2234() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let control = format!("http://127.0.0.1:{port}/ctl");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                let count = sock.read(&mut tmp).await.unwrap();
                if count == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..count]);
                if buf.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let header_end = buf
                        .windows(4)
                        .position(|bytes| bytes == b"\r\n\r\n")
                        .unwrap();
                    let header = String::from_utf8_lossy(&buf[..header_end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8(buf).unwrap()
        });
        add_upnp_mapping(&control, "127.0.0.1", 2234).await.unwrap();
        let request = server.await.unwrap();
        assert!(request.contains("<NewExternalPort>2234</NewExternalPort>"));
        assert!(request.contains("<NewInternalPort>2234</NewInternalPort>"));
        assert!(request.contains("<NewProtocol>TCP</NewProtocol>"));
    }
}
