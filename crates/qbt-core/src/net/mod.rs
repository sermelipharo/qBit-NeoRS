//! Networking helpers shared by subsystems: HTTP client with proxy and
//! cookies, network interfaces, SMTP notifications, dynamic DNS, GeoIP.

pub mod cookies;
pub mod dyndns;
pub mod favicon;
pub mod geoip;
pub mod http;
pub mod interfaces;
pub mod portcheck;
pub mod smtp;

#[cfg(test)]
pub(crate) mod testutil;
