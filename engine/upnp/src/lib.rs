use anyhow::{Context, bail};
use bstr::BStr;
use futures::{StreamExt, TryFutureExt, stream::FuturesUnordered};
use librqbit_dualstack_sockets::{BindDevice, UdpSocket};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use reqwest::Client;
use serde_derive::Deserialize;
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tracing::{Instrument, Span, debug, debug_span, trace, warn};
use url::Url;

const SERVICE_TYPE_WAN_IP_CONNECTION_V1: &str = "urn:schemas-upnp-org:service:WANIPConnection:1";
const SERVICE_TYPE_WAN_IP_CONNECTION_V2: &str = "urn:schemas-upnp-org:service:WANIPConnection:2";

/// IGDv1 and IGDv2 routers: some (e.g. OpenWrt builds) offer only
/// WANIPConnection:2, which takes the same AddPortMapping request.
fn is_wan_ip_connection_service(service_type: &str) -> bool {
    service_type == SERVICE_TYPE_WAN_IP_CONNECTION_V1
        || service_type == SERVICE_TYPE_WAN_IP_CONNECTION_V2
}
const SSDP_MULTICAST_IP: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900));
pub const SSDP_SEARCH_WAN_IPCONNECTION_ST: &str = "urn:schemas-upnp-org:service:WANIPConnection:1";
pub const SSDP_SEARCH_WAN_IPCONNECTION_V2_ST: &str =
    "urn:schemas-upnp-org:service:WANIPConnection:2";
pub const SSDP_SEARCH_ROOT_ST: &str = "upnp:rootdevice";

pub fn make_ssdp_search_request(kind: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
            Host: 239.255.255.250:1900\r\n\
            Man: \"ssdp:discover\"\r\n\
            MX: 3\r\n\
            ST: {kind}\r\n\
            \r\n"
    )
}

pub fn get_local_ip_relative_to(
    local_dest: SocketAddr,
    interfaces: &[NetworkInterface],
) -> anyhow::Result<IpAddr> {
    fn masked_v4(ip: Ipv4Addr, mask: Ipv4Addr) -> u32 {
        ip.to_bits() & mask.to_bits()
    }

    fn masked_v6(ip: Ipv6Addr, mask: Ipv6Addr) -> u128 {
        ip.to_bits() & mask.to_bits()
    }

    for i in interfaces {
        for addr in i.addr.iter() {
            match (local_dest, addr.ip(), addr.netmask()) {
                // We are connecting to ourselves, return itself.
                (l, a, _) if l.ip() == a => return Ok(addr.ip()),
                // IPv4 masks match.
                (SocketAddr::V4(l), IpAddr::V4(a), Some(IpAddr::V4(m)))
                    if masked_v4(*l.ip(), m) == masked_v4(a, m) =>
                {
                    return Ok(addr.ip());
                }
                // Return IPv6 link-local addresses when source is link-local address and there's a scope_id set.
                (SocketAddr::V6(l), IpAddr::V6(a), _)
                    if l.ip().is_unicast_link_local() && l.scope_id() > 0 =>
                {
                    if a.is_unicast_link_local() && l.scope_id() == i.index {
                        return Ok(addr.ip());
                    }
                }
                // If V6 masks match, return.
                (SocketAddr::V6(l), IpAddr::V6(a), Some(IpAddr::V6(m)))
                    if masked_v6(*l.ip(), m) == masked_v6(a, m) =>
                {
                    return Ok(addr.ip());
                }
                // For IPv6 fallback to returning a random (first encountered) IPv6 address.
                (SocketAddr::V6(_), IpAddr::V6(_), None) => return Ok(addr.ip()),
                _ => continue,
            }
        }
    }
    bail!("couldn't find a local ip address")
}

/// Transport protocol of a port mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UpnpProtocol {
    Tcp,
    Udp,
}

impl UpnpProtocol {
    fn as_str(self) -> &'static str {
        match self {
            UpnpProtocol::Tcp => "TCP",
            UpnpProtocol::Udp => "UDP",
        }
    }
}

/// Result of one `AddPortMapping` request (reported to the status callback).
#[derive(Clone, Debug)]
pub struct UpnpMappingEvent {
    pub protocol: UpnpProtocol,
    pub port: u16,
    pub local_ip: IpAddr,
    pub lease_duration: Duration,
    /// `Err` holds a short error description.
    pub result: Result<(), String>,
}

pub type UpnpStatusCallback = std::sync::Arc<dyn Fn(UpnpMappingEvent) + Send + Sync>;

/// Mapping descriptions are shown by routers; keep them short and plain.
fn sanitize_description(d: &str) -> String {
    d.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(64)
        .collect()
}

async fn forward_port_proto(
    control_url: Url,
    service_type: &str,
    local_ip: IpAddr,
    protocol: UpnpProtocol,
    port: u16,
    lease_duration: Duration,
    description: &str,
) -> anyhow::Result<()> {
    let proto = protocol.as_str();
    let description = sanitize_description(description);
    let request_body = format!(
        r#"
        <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
            s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
            <s:Body>
                <u:AddPortMapping xmlns:u="{service_type}">
                    <NewRemoteHost></NewRemoteHost>
                    <NewExternalPort>{port}</NewExternalPort>
                    <NewProtocol>{proto}</NewProtocol>
                    <NewInternalPort>{port}</NewInternalPort>
                    <NewInternalClient>{local_ip}</NewInternalClient>
                    <NewEnabled>1</NewEnabled>
                    <NewPortMappingDescription>{description}</NewPortMappingDescription>
                    <NewLeaseDuration>{}</NewLeaseDuration>
                </u:AddPortMapping>
            </s:Body>
        </s:Envelope>
    "#,
        lease_duration.as_secs()
    );

    let url = control_url;

    let client = reqwest::Client::builder()
        .timeout(DESCRIPTION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .post(url.clone())
        .header("Content-Type", "text/xml")
        .header("SOAPAction", format!("\"{service_type}#AddPortMapping\""))
        .body(request_body)
        .send()
        .await
        .context("error sending")?;

    let status = response.status();

    // The body is not needed (and not bounded): only the status counts.
    drop(response);
    trace!(status = %status, "AddPortMapping response");
    if !status.is_success() {
        bail!("failed port forwarding: {}", status);
    } else {
        debug!(%local_ip, port, "successfully port forwarded");
    }
    Ok(())
}

/// A `GetExternalIPAddress` answer is a few hundred bytes.
const MAX_SOAP_RESPONSE_SIZE: usize = 16 * 1024;

/// Receives the gateway's external address after each
/// `GetExternalIPAddress` request (`Err` holds a short description).
pub type UpnpExternalIpCallback = std::sync::Arc<dyn Fn(Result<IpAddr, String>) + Send + Sync>;

/// Asks the gateway for its external (WAN) address.
async fn get_external_ip(control_url: Url, service_type: &str) -> anyhow::Result<IpAddr> {
    let request_body = format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:GetExternalIPAddress xmlns:u="{service_type}"></u:GetExternalIPAddress></s:Body>
</s:Envelope>"#
    );
    let client = reqwest::Client::builder()
        .timeout(DESCRIPTION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .context("failed to build HTTP client")?;
    let mut response = client
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{service_type}#GetExternalIPAddress\""),
        )
        .body(request_body)
        .send()
        .await
        .context("error sending")?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read response body")?
    {
        if body.len() + chunk.len() > MAX_SOAP_RESPONSE_SIZE {
            bail!("GetExternalIPAddress response too large");
        }
        body.extend_from_slice(&chunk);
    }
    // SOAP faults come with status 500; the parser reports their message.
    match parse_external_ip_response(&String::from_utf8_lossy(&body)) {
        Err(e) if !status.is_success() => Err(e.context(format!("status {status}"))),
        r => r,
    }
}

/// Reads `NewExternalIPAddress` from a `GetExternalIPAddressResponse`
/// (any namespace prefixes), or the error of a SOAP fault.
pub fn parse_external_ip_response(xml: &str) -> anyhow::Result<IpAddr> {
    use quick_xml::events::Event;

    #[derive(PartialEq)]
    enum Field {
        None,
        Address,
        Fault,
    }
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut field = Field::None;
    let mut address: Option<String> = None;
    let mut fault: Option<String> = None;
    let mut depth = 0usize;
    loop {
        let text = match reader.read_event().context("bad XML")? {
            Event::Start(e) => {
                depth += 1;
                if depth > MAX_DESCRIPTION_DEPTH {
                    bail!("XML nested too deeply");
                }
                field = match e.local_name().as_ref() {
                    b"NewExternalIPAddress" => {
                        address.get_or_insert_with(String::new);
                        Field::Address
                    }
                    b"faultstring" | b"errorDescription" => Field::Fault,
                    _ => Field::None,
                };
                continue;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                field = Field::None;
                continue;
            }
            Event::Empty(e) if e.local_name().as_ref() == b"NewExternalIPAddress" => {
                address = Some(String::new());
                continue;
            }
            Event::Text(t) if field != Field::None => t.decode().context("bad text")?.into_owned(),
            Event::CData(t) if field != Field::None => t.decode().context("bad text")?.into_owned(),
            Event::Eof => break,
            _ => continue,
        };
        let slot = if field == Field::Address {
            &mut address
        } else {
            &mut fault
        };
        let s = slot.get_or_insert_with(String::new);
        if s.len() + text.len() <= 256 {
            s.push_str(&text);
        }
    }
    match address {
        Some(a) => {
            let a = a.trim();
            if a.is_empty() {
                bail!("the gateway has no external address");
            }
            a.parse()
                .with_context(|| format!("bad external address {a:?}"))
        }
        None => match fault {
            Some(f) => bail!("GetExternalIPAddress failed: {}", f.trim()),
            None => bail!("no NewExternalIPAddress in the response"),
        },
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RootDesc {
    #[serde(rename = "device")]
    pub devices: Vec<Device>,
}

#[derive(Default, Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DeviceList {
    #[serde(rename = "device")]
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Device {
    #[serde(rename = "deviceType")]
    pub device_type: String,
    #[serde(rename = "friendlyName", default)]
    pub friendly_name: String,
    #[serde(rename = "serviceList", default)]
    pub service_list: ServiceList,
    #[serde(rename = "deviceList", default)]
    pub device_list: DeviceList,
}

impl Device {
    pub fn iter_services(
        &self,
        parent: Span,
    ) -> Box<dyn Iterator<Item = (tracing::Span, &Service)> + '_> {
        let self_span = self.span(parent);
        let services = self.service_list.services.iter().map({
            let self_span = self_span.clone();
            move |s| (s.span(self_span.clone()), s)
        });
        Box::new(services.chain(self.device_list.devices.iter().flat_map({
            let self_span = self_span.clone();
            move |d| d.iter_services(self_span.clone())
        })))
    }

    pub fn span(&self, parent: tracing::Span) -> tracing::Span {
        debug_span!(parent: parent, "device", device = self.name())
    }
}

impl Device {
    pub fn name(&self) -> &str {
        if self.friendly_name.is_empty() {
            return &self.device_type;
        }
        &self.friendly_name
    }
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq, Eq)]
pub struct ServiceList {
    #[serde(rename = "service", default)]
    pub services: Vec<Service>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Service {
    #[serde(rename = "serviceType")]
    pub service_type: String,
    #[serde(rename = "controlURL")]
    pub control_url: String,
    #[serde(rename = "SCPDURL")]
    pub scpd_url: String,
    #[serde(rename = "eventSubURL", default)]
    pub event_sub_url: Option<String>,
}

impl Service {
    pub fn span(&self, parent: tracing::Span) -> tracing::Span {
        debug_span!(parent: parent, "service", url = self.control_url)
    }
}

#[derive(Debug)]
struct UpnpEndpoint {
    discover_response: UpnpDiscoverResponse,
    data: RootDesc,
    nics: Vec<NetworkInterface>,
}

impl UpnpEndpoint {
    fn location(&self) -> &Url {
        &self.discover_response.location
    }

    fn span(&self) -> tracing::Span {
        debug_span!("upnp_endpoint", location = %self.location())
    }

    fn iter_services(&self) -> impl Iterator<Item = (tracing::Span, &Service)> + '_ {
        let self_span = self.span();
        self.data
            .devices
            .iter()
            .flat_map(move |d| d.iter_services(self_span.clone()))
    }

    fn my_local_ip(&self) -> anyhow::Result<IpAddr> {
        let received_from = self.discover_response.received_from;
        let local_ip = get_local_ip_relative_to(received_from, &self.nics)
            .with_context(|| format!("can't determine local IP relative to {received_from}"))?;
        Ok(local_ip)
    }

    /// `(span, control URL, service type)` of each WANIPConnection service.
    fn get_wan_ip_control_urls(
        &self,
    ) -> impl Iterator<Item = (tracing::Span, Url, &'static str)> + '_ {
        self.iter_services()
            .filter_map(|(span, s)| {
                let service_type = if s.service_type == SERVICE_TYPE_WAN_IP_CONNECTION_V2 {
                    SERVICE_TYPE_WAN_IP_CONNECTION_V2
                } else if is_wan_ip_connection_service(&s.service_type) {
                    SERVICE_TYPE_WAN_IP_CONNECTION_V1
                } else {
                    return None;
                };
                Some((
                    span,
                    self.discover_response.location.join(&s.control_url),
                    service_type,
                ))
            })
            .filter_map(|(span, url, service_type)| match url {
                // An absolute control URL must stay on the device.
                Ok(url) if location_matches_sender(&url, self.discover_response.received_from) => {
                    Some((span, url, service_type))
                }
                Ok(url) => {
                    debug!("control url {url} points away from the device");
                    None
                }
                Err(e) => {
                    debug!("bad control url: {e:#}");
                    None
                }
            })
    }
}

#[derive(Debug)]
pub struct UpnpDiscoverResponse {
    pub received_from: SocketAddr,
    pub location: Url,
}

/// Device descriptions come from any host on the LAN: bounded size, time and
/// nesting (the serde model is recursive).
const MAX_DESCRIPTION_SIZE: usize = 256 * 1024;
const MAX_DESCRIPTION_DEPTH: usize = 32;
const DESCRIPTION_TIMEOUT: Duration = Duration::from_secs(10);
/// WANIPConnection services managed at once (each keeps a task alive).
const MAX_SERVICE_TASKS: usize = 8;
/// Distinct SSDP locations taken from one discovery round.
const MAX_LOCATIONS_PER_DISCOVERY: usize = 16;
/// Device descriptions fetched at once (each up to 256 KiB for 10 s).
const MAX_ENDPOINT_FETCHES: usize = 8;

/// The description must come from the device that answered the search: an
/// http URL whose host is that device's IP, so a LAN host cannot point us
/// at localhost services or arbitrary servers.
fn location_matches_sender(location: &Url, received_from: SocketAddr) -> bool {
    let host_ip = match location.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip),
        _ => return false,
    };
    location.scheme() == "http" && host_ip.to_canonical() == received_from.ip().to_canonical()
}

/// Rejects documents nested deeper than [`MAX_DESCRIPTION_DEPTH`] before
/// they reach the recursive deserializer (stack overflow = abort).
fn check_xml_depth(xml: &str) -> anyhow::Result<()> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    loop {
        match reader.read_event().context("bad XML")? {
            quick_xml::events::Event::Start(_) => {
                depth += 1;
                if depth > MAX_DESCRIPTION_DEPTH {
                    anyhow::bail!("XML nested too deeply");
                }
            }
            quick_xml::events::Event::End(_) => depth = depth.saturating_sub(1),
            quick_xml::events::Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

pub async fn discover_services(location: Url) -> anyhow::Result<RootDesc> {
    let client = Client::builder()
        .timeout(DESCRIPTION_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .context("failed to build HTTP client")?;
    let mut resp = client
        .get(location.clone())
        .send()
        .await
        .context("failed to send GET request")?;
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("failed to read response body")? {
        if body.len() + chunk.len() > MAX_DESCRIPTION_SIZE {
            anyhow::bail!("device description too large");
        }
        body.extend_from_slice(&chunk);
    }
    let response = String::from_utf8_lossy(&body);
    trace!("received from {location}: {response}");
    check_xml_depth(&response)?;
    let root_desc: RootDesc = quick_xml::de::from_str(&response)
        .context("failed to parse response body as xml")
        .inspect_err(|e| {
            debug!("failed to parse this XML: {response}. Error: {e:#}");
        })?;
    Ok(root_desc)
}

pub fn parse_upnp_discover_response(
    buf: &[u8],
    received_from: SocketAddr,
) -> anyhow::Result<UpnpDiscoverResponse> {
    let mut headers = [httparse::EMPTY_HEADER; 16];
    let mut resp = httparse::Response::new(&mut headers);
    resp.parse(buf).context("error parsing response")?;

    trace!(?resp, "parsed SSDP response");
    match resp.code {
        Some(200) => {}
        other => anyhow::bail!("bad response code {other:?}, expected 200"),
    }
    let mut location = None;
    for header in resp.headers {
        match header.name {
            "location" | "LOCATION" | "Location" => {
                location = Some(
                    std::str::from_utf8(header.value).context("bad utf-8 in location header")?,
                )
            }
            _ => continue,
        }
    }
    let location = location.context("missing location header")?;
    let location =
        Url::parse(location).with_context(|| format!("failed parsing location {location}"))?;
    if !location_matches_sender(&location, received_from) {
        anyhow::bail!("location {location} does not point at the responding device {received_from}");
    }
    Ok(UpnpDiscoverResponse {
        location,
        received_from,
    })
}

/// Whether a discovered location is new in this round and within the
/// per-round limit (records it).
fn accept_location(seen: &mut HashSet<Url>, location: &Url) -> bool {
    seen.len() < MAX_LOCATIONS_PER_DISCOVERY && seen.insert(location.clone())
}

pub async fn discover_once(
    tx: &UnboundedSender<UpnpDiscoverResponse>,
    kind: &str,
    timeout: Duration,
    bind_device: Option<&BindDevice>,
) -> anyhow::Result<()> {
    discover_once_kinds(tx, &[kind], timeout, bind_device).await
}

/// Like [`discover_once`], searching for several kinds from one socket, so a
/// device answering more than one search is reported once.
pub async fn discover_once_kinds(
    tx: &UnboundedSender<UpnpDiscoverResponse>,
    kinds: &[&str],
    timeout: Duration,
    bind_device: Option<&BindDevice>,
) -> anyhow::Result<()> {
    // TODO: do we need IPv6 support here? I can't test it, don't have the hardware for it (router / IPv6 provider).
    let socket = UdpSocket::bind_udp(
        (Ipv4Addr::UNSPECIFIED, 0).into(),
        librqbit_dualstack_sockets::BindOpts {
            device: bind_device,
            ..Default::default()
        },
    )?;

    for kind in kinds {
        let message = make_ssdp_search_request(kind);
        socket
            .send_to(message.as_bytes(), SSDP_MULTICAST_IP)
            .await
            .with_context(|| {
                format!("failed to send SSDP search request to {SSDP_MULTICAST_IP}")
            })?;
    }

    let mut buffer = [0; 2048];

    let timeout = tokio::time::sleep(timeout);
    let mut timed_out = false;
    tokio::pin!(timeout);

    let mut discovered = 0;
    // Any LAN host can answer (many times): each distinct location is
    // fetched once per round, and only the first few.
    let mut seen = HashSet::<Url>::new();

    while !timed_out {
        tokio::select! {
            _ = &mut timeout, if !timed_out => {
                timed_out = true;
            }
            Ok((len, addr)) = socket.recv_from(&mut buffer), if !timed_out => {
                let response = &buffer[..len];
                match parse_upnp_discover_response(response, addr) {
                    Ok(r) => {
                        if !accept_location(&mut seen, &r.location) {
                            continue;
                        }
                        tx.send(r)?;
                        discovered += 1;
                    },
                    Err(e) => warn!(response=?BStr::new(response), "failed to parse SSDP response: {e:#}"),
                };
            },
        }
    }

    debug!("discovered {discovered} endpoints");
    Ok(())
}

pub struct UpnpPortForwarderOptions {
    pub lease_duration: Duration,
    pub discover_interval: Duration,
    pub discover_timeout: Duration,
    /// `NewPortMappingDescription` (sanitized to ASCII, at most 64 characters).
    pub description: String,
}

impl Default for UpnpPortForwarderOptions {
    fn default() -> Self {
        Self {
            discover_interval: Duration::from_secs(60),
            discover_timeout: Duration::from_secs(10),
            lease_duration: Duration::from_secs(60),
            description: "rust UPnP".to_owned(),
        }
    }
}

pub struct UpnpPortForwarder {
    ports: Vec<(UpnpProtocol, u16)>,
    opts: UpnpPortForwarderOptions,
    bind_device: Option<BindDevice>,
    status: Option<UpnpStatusCallback>,
    external_ip: Option<UpnpExternalIpCallback>,
}

impl UpnpPortForwarder {
    pub fn new(
        ports: Vec<u16>,
        opts: Option<UpnpPortForwarderOptions>,
        bind_device: Option<BindDevice>,
    ) -> anyhow::Result<Self> {
        Self::new_with_protocols(
            ports.into_iter().map(|p| (UpnpProtocol::Tcp, p)).collect(),
            opts,
            bind_device,
        )
    }

    /// Maps each `(protocol, port)` (same internal and external port).
    pub fn new_with_protocols(
        ports: Vec<(UpnpProtocol, u16)>,
        opts: Option<UpnpPortForwarderOptions>,
        bind_device: Option<BindDevice>,
    ) -> anyhow::Result<Self> {
        if ports.is_empty() {
            bail!("empty ports")
        }
        Ok(Self {
            ports,
            opts: opts.unwrap_or_default(),
            bind_device,
            status: None,
            external_ip: None,
        })
    }

    /// Report the result of every mapping request.
    pub fn with_status_callback(mut self, cb: UpnpStatusCallback) -> Self {
        self.status = Some(cb);
        self
    }

    /// Also ask each gateway for its external address (when mappings are
    /// renewed) and report it.
    pub fn with_external_ip_callback(mut self, cb: UpnpExternalIpCallback) -> Self {
        self.external_ip = Some(cb);
        self
    }

    /// Queries the gateway's external address at the mapping renewal
    /// interval, while there is someone to tell.
    async fn manage_external_ip(&self, control_url: Url, service_type: &'static str) {
        let Some(cb) = &self.external_ip else { return };
        let mut interval = tokio::time::interval(self.opts.lease_duration / 2);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let result = get_external_ip(control_url.clone(), service_type).await;
            match &result {
                Ok(ip) => debug!(%ip, "gateway external address"),
                Err(e) => debug!("GetExternalIPAddress failed: {e:#}"),
            }
            cb(result.map_err(|e| format!("{e:#}")));
        }
    }

    async fn parse_endpoint(
        &self,
        discover_response: UpnpDiscoverResponse,
    ) -> anyhow::Result<UpnpEndpoint> {
        let services = discover_services(discover_response.location.clone()).await?;
        let nics = network_interface::NetworkInterface::show()
            .context("error listing network interfaces")?;
        Ok(UpnpEndpoint {
            discover_response,
            data: services,
            nics,
        })
    }

    async fn discover_once(
        &self,
        tx: &UnboundedSender<UpnpDiscoverResponse>,
    ) -> anyhow::Result<()> {
        discover_once_kinds(
            tx,
            &[
                SSDP_SEARCH_WAN_IPCONNECTION_ST,
                SSDP_SEARCH_WAN_IPCONNECTION_V2_ST,
            ],
            self.opts.discover_timeout,
            self.bind_device.as_ref(),
        )
        .await
    }

    async fn discovery(&self, tx: UnboundedSender<UpnpDiscoverResponse>) -> anyhow::Result<()> {
        let mut discover_interval = tokio::time::interval(self.opts.discover_interval);
        discover_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            discover_interval.tick().await;
            if let Err(e) = self.discover_once(&tx).await {
                warn!("failed to run SSDP/UPNP discovery: {e:#}");
            }
        }
    }

    async fn manage_port(
        &self,
        control_url: Url,
        service_type: &'static str,
        local_ip: IpAddr,
        protocol: UpnpProtocol,
        port: u16,
    ) -> ! {
        let lease_duration = self.opts.lease_duration;
        let mut interval = tokio::time::interval(lease_duration / 2);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;
            let result = forward_port_proto(
                control_url.clone(),
                service_type,
                local_ip,
                protocol,
                port,
                lease_duration,
                &self.opts.description,
            )
            .await;
            if let Err(e) = &result {
                warn!("failed to forward port: {e:#}");
            }
            if let Some(cb) = &self.status {
                cb(UpnpMappingEvent {
                    protocol,
                    port,
                    local_ip,
                    lease_duration,
                    result: result.map_err(|e| format!("{e:#}")),
                });
            }
        }
    }

    async fn manage_service(
        &self,
        control_url: Url,
        service_type: &'static str,
        local_ip: IpAddr,
    ) -> anyhow::Result<()> {
        let ports =
            futures::future::join_all(self.ports.iter().cloned().map(|(protocol, port)| {
                self.manage_port(control_url.clone(), service_type, local_ip, protocol, port)
                    .instrument(debug_span!("manage_port", port = port, ?protocol))
            }));
        let external_ip = self
            .manage_external_ip(control_url.clone(), service_type)
            .instrument(debug_span!("external_ip"));
        tokio::join!(ports, external_ip);
        Ok(())
    }

    pub async fn run_forever(self) -> ! {
        let (discover_tx, mut discover_rx) = unbounded_channel();
        let discovery = self.discovery(discover_tx);

        let mut spawned_tasks = HashSet::<Url>::new();

        let mut endpoints = FuturesUnordered::new();
        let mut service_managers = FuturesUnordered::new();

        tokio::pin!(discovery);

        loop {
            tokio::select! {
                _ = &mut discovery => {},
                r = discover_rx.recv() => {
                    let r = r.unwrap();
                    if endpoints.len() >= MAX_ENDPOINT_FETCHES {
                        debug!(location=%r.location, "too many UPnP descriptions being fetched, ignoring");
                        continue;
                    }
                    let location = r.location.clone();
                    endpoints.push(self.parse_endpoint(r).map_err(|e| {
                        debug!("error parsing endpoint: {e:#}");
                        e
                    }).instrument(debug_span!("parse endpoint", location=location.to_string())));
                },
                Some(Ok(endpoint)) = endpoints.next(), if !endpoints.is_empty() => {
                    let mut local_ip = None;
                    for (span, control_url, service_type) in endpoint.get_wan_ip_control_urls() {
                        if spawned_tasks.contains(&control_url) {
                            debug!("already spawned for {}", control_url);
                            continue;
                        }
                        if spawned_tasks.len() >= MAX_SERVICE_TASKS {
                            debug!("too many UPnP services, ignoring {}", control_url);
                            break;
                        }
                        let ip = match local_ip {
                            Some(ip) => ip,
                            None => {
                                match endpoint.my_local_ip() {
                                    Ok(ip) => {
                                        local_ip = Some(ip);
                                        ip
                                    },
                                    Err(e) => {
                                        warn!("failed to determine local IP for endpoint at {}: {:#}", endpoint.location(), e);
                                        break;
                                    }
                                }
                            }
                        };
                        spawned_tasks.insert(control_url.clone());
                        service_managers.push(
                            self.manage_service(control_url, service_type, ip)
                                .instrument(span),
                        )
                    }
                },
                _ = service_managers.next(), if !service_managers.is_empty() => {

                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use quick_xml::de::from_str;

    use crate::{Device, DeviceList, RootDesc, Service, ServiceList};

    #[test]
    fn location_must_point_at_the_responder() {
        let from: std::net::SocketAddr = "192.168.1.1:1900".parse().unwrap();
        let resp = |loc: &str| format!("HTTP/1.1 200 OK\r\nLOCATION: {loc}\r\n\r\n");
        let ok = crate::parse_upnp_discover_response(resp("http://192.168.1.1:5000/desc.xml").as_bytes(), from);
        assert!(ok.is_ok());
        for bad in [
            "http://127.0.0.1:8080/admin",
            "http://192.168.1.2:5000/desc.xml",
            "http://router.lan:5000/desc.xml",
            "https://evil.example/desc.xml",
            "file:///etc/passwd",
        ] {
            assert!(crate::parse_upnp_discover_response(resp(bad).as_bytes(), from).is_err(), "{bad}");
        }
        // Absolute control URLs elsewhere are refused too.
        let loc = url::Url::parse("http://192.168.1.1:5000/desc.xml").unwrap();
        assert!(crate::location_matches_sender(&loc.join("/ctl").unwrap(), from));
        assert!(!crate::location_matches_sender(&loc.join("http://127.0.0.1:631/x").unwrap(), from));
    }

    #[test]
    fn discovery_takes_each_location_once_and_a_bounded_number() {
        let mut seen = std::collections::HashSet::new();
        let loc = |i: usize| url::Url::parse(&format!("http://192.168.1.{}:5000/d.xml", i % 250 + 1)).unwrap();
        assert!(crate::accept_location(&mut seen, &loc(0)));
        // A host answering again and again is fetched once.
        for _ in 0..100 {
            assert!(!crate::accept_location(&mut seen, &loc(0)));
        }
        let accepted = (1..1000).filter(|i| crate::accept_location(&mut seen, &loc(*i))).count();
        assert_eq!(accepted + 1, crate::MAX_LOCATIONS_PER_DISCOVERY);
    }

    #[test]
    fn wan_ip_connection_v1_and_v2_are_both_used() {
        let v2 = include_str!("resources/test/devices-0.xml")
            .replace("WANIPConnection:1", "WANIPConnection:2");
        for (xml, expected) in [
            (
                include_str!("resources/test/devices-0.xml").to_owned(),
                crate::SERVICE_TYPE_WAN_IP_CONNECTION_V1,
            ),
            (v2, crate::SERVICE_TYPE_WAN_IP_CONNECTION_V2),
        ] {
            let endpoint = crate::UpnpEndpoint {
                discover_response: crate::parse_upnp_discover_response(
                    b"HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:5000/desc.xml\r\n\r\n",
                    "192.168.1.1:1900".parse().unwrap(),
                )
                .unwrap(),
                data: from_str::<RootDesc>(&xml).unwrap(),
                nics: Vec::new(),
            };
            let found: Vec<_> = endpoint
                .get_wan_ip_control_urls()
                .map(|(_, url, ty)| (url.to_string(), ty))
                .collect();
            assert_eq!(
                found,
                [(
                    "http://192.168.1.1:5000/upnp/control/WANIPConnection0".to_owned(),
                    expected
                )]
            );
        }
        assert!(!crate::is_wan_ip_connection_service(
            "urn:schemas-upnp-org:service:WANPPPConnection:1"
        ));
    }

    #[test]
    fn external_ip_responses_are_parsed() {
        use crate::parse_external_ip_response as parse;
        let ok = r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body><u:GetExternalIPAddressResponse xmlns:u="urn:schemas-upnp-org:service:WANIPConnection:1">
<NewExternalIPAddress>81.2.3.4</NewExternalIPAddress>
</u:GetExternalIPAddressResponse></s:Body></s:Envelope>"#;
        assert_eq!(parse(ok).unwrap(), "81.2.3.4".parse::<std::net::IpAddr>().unwrap());

        // Other prefixes, a prefixed element, whitespace, CDATA and IPv6.
        let odd = r#"<SOAP-ENV:Envelope xmlns:SOAP-ENV="http://schemas.xmlsoap.org/soap/envelope/">
  <SOAP-ENV:Body>
    <m:GetExternalIPAddressResponse xmlns:m="urn:schemas-upnp-org:service:WANIPConnection:2">
      <m:NewExternalIPAddress>
        2a01:4f9::1
      </m:NewExternalIPAddress>
    </m:GetExternalIPAddressResponse>
  </SOAP-ENV:Body>
</SOAP-ENV:Envelope>"#;
        assert_eq!(parse(odd).unwrap(), "2a01:4f9::1".parse::<std::net::IpAddr>().unwrap());
        let cdata = "<r><NewExternalIPAddress><![CDATA[5.6.7.8]]></NewExternalIPAddress></r>";
        assert_eq!(parse(cdata).unwrap(), "5.6.7.8".parse::<std::net::IpAddr>().unwrap());

        // No address yet (WAN down): empty or self-closing element.
        for empty in [
            "<r><NewExternalIPAddress></NewExternalIPAddress></r>",
            "<r><NewExternalIPAddress/></r>",
            "<r><NewExternalIPAddress>  </NewExternalIPAddress></r>",
        ] {
            let e = parse(empty).unwrap_err().to_string();
            assert!(e.contains("no external address"), "{e}");
        }

        // SOAP fault: the UPnP error description is reported.
        let fault = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault>
<faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>
<detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>501</errorCode>
<errorDescription>Action Failed</errorDescription></UPnPError></detail>
</s:Fault></s:Body></s:Envelope>"#;
        let e = parse(fault).unwrap_err().to_string();
        assert!(e.contains("Action Failed"), "{e}");

        for bad in [
            "",
            "not xml at all",
            "<r><NewExternalIPAddress>not-an-ip</NewExternalIPAddress></r>",
            "<r><NewExternalIPAddress>1.2.3.4.5</NewExternalIPAddress></r>",
            "<r><Other>1.2.3.4</Other></r>",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        let deep = format!("{}{}", "<a>".repeat(1000), "</a>".repeat(1000));
        assert!(parse(&deep).is_err());
        // An endless address is not collected without bound.
        let long = format!("<r><NewExternalIPAddress>{}</NewExternalIPAddress></r>", "1".repeat(100_000));
        assert!(parse(&long).is_err());
    }

    #[test]
    fn deeply_nested_descriptions_are_rejected() {
        let deep = format!(
            "<root>{}{}</root>",
            "<deviceList><device>".repeat(10_000),
            "</device></deviceList>".repeat(10_000)
        );
        assert!(crate::check_xml_depth(&deep).is_err());
        assert!(crate::check_xml_depth(include_str!("resources/test/devices-0.xml")).is_ok());
    }

    #[test]
    fn test_parse_root_desc() {
        let actual = from_str::<RootDesc>(include_str!("resources/test/devices-0.xml")).unwrap();
        let expected = RootDesc {
            devices: vec![Device {
                device_type: "urn:schemas-upnp-org:device:InternetGatewayDevice:1".into(),
                friendly_name: "ARRIS TG3492LG".into(),
                service_list: ServiceList {
                    services: vec![Service {
                        service_type: "urn:schemas-upnp-org:service:Layer3Forwarding:1".into(),
                        control_url: "/upnp/control/Layer3Forwarding".into(),
                        scpd_url: "/Layer3ForwardingSCPD.xml".into(),
                        event_sub_url: Some("/upnp/event/Layer3Forwarding".into()),
                    }],
                },
                device_list: DeviceList {
                    devices: vec![Device {
                        device_type: "urn:schemas-upnp-org:device:WANDevice:1".into(),
                        friendly_name: "WANDevice:1".into(),
                        service_list: ServiceList {
                            services: vec![Service {
                                service_type:
                                    "urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1".into(),
                                control_url: "/upnp/control/WANCommonInterfaceConfig0".into(),
                                scpd_url: "/WANCommonInterfaceConfigSCPD.xml".into(),
                                event_sub_url: Some("/upnp/event/WANCommonInterfaceConfig0".into()),
                            }],
                        },
                        device_list: DeviceList {
                            devices: vec![Device {
                                device_type: "urn:schemas-upnp-org:device:WANConnectionDevice:1"
                                    .into(),
                                friendly_name: "WANConnectionDevice:1".into(),
                                service_list: ServiceList {
                                    services: vec![Service {
                                        service_type:
                                            "urn:schemas-upnp-org:service:WANIPConnection:1".into(),
                                        control_url: "/upnp/control/WANIPConnection0".into(),
                                        scpd_url: "/WANIPConnectionServiceSCPD.xml".into(),
                                        event_sub_url: Some("/upnp/event/WANIPConnection0".into()),
                                    }],
                                },
                                device_list: DeviceList { devices: vec![] },
                            }],
                        },
                    }],
                },
            }],
        };
        assert_eq!(actual, expected);
    }
}
