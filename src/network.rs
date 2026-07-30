use std::net::{IpAddr, Ipv6Addr};

use tokio::net::lookup_host;
use url::Url;

use crate::error::{EasyMusicError, Result};

pub fn validate_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(EasyMusicError::source(format!(
                "unsupported URL scheme {scheme:?}; only http and https are allowed"
            )));
        }
    }
    if url.host_str().is_none() {
        return Err(EasyMusicError::source("audio URL has no host"));
    }
    Ok(url)
}

pub(crate) async fn reject_private_host(url: &url::Url) -> Result<()> {
    let host = url
        .host_str()
        .ok_or_else(|| EasyMusicError::source("audio URL has no host"))?;
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(EasyMusicError::source(
            "private or loopback audio URLs require --allow-private-network",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(EasyMusicError::source(
                "private or loopback audio URLs require --allow-private-network",
            ));
        }
        return Ok(());
    }

    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = lookup_host((host, port))
        .await
        .map_err(|error| EasyMusicError::source(format!("cannot resolve audio host: {error}")))?;
    for address in addresses {
        if is_private_ip(address.ip()) {
            return Err(EasyMusicError::source(
                "audio host resolves to a private or loopback address; use --allow-private-network to override",
            ));
        }
    }
    Ok(())
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || is_ipv6_documentation(ip)
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private_ip(IpAddr::V4(mapped)))
        }
    }
}

fn is_ipv6_documentation(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn rejects_private_addresses() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
    }

    #[test]
    fn rejects_non_http_urls() {
        let error = validate_http_url("file:///etc/passwd").unwrap_err();
        assert!(error.message.contains("only http and https"));
    }
}
