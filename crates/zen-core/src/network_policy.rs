//! Network egress policy (FR-036).
//!
//! Domain/IP allowlist enforced at the tool layer, blocking SSRF vectors
//! (cloud metadata endpoints, loopback, RFC1918 private ranges) before any
//! HTTP request leaves the process.

use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// Hosts explicitly allowed even though they would otherwise be denied
    /// (e.g. `localhost:11434` for a local Ollama provider).
    pub allow_hosts: Vec<String>,
    /// When true, private-range denial is skipped entirely (DangerFullAccess parity).
    pub allow_private_ranges: bool,
}

impl NetworkPolicy {
    pub fn with_allow_hosts(allow_hosts: Vec<String>) -> Self {
        Self {
            allow_hosts,
            allow_private_ranges: false,
        }
    }

    /// Validate a URL string before any request is made.
    ///
    /// Returns `Err(reason)` when the target is blocked by policy.
    pub fn validate_url(&self, url: &str) -> Result<(), String> {
        let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;

        let host = match parsed.host() {
            Some(h) => h,
            None => return Err("URL has no host".to_string()),
        };

        match host {
            url::Host::Domain(domain) => {
                if self.host_allowed(domain) {
                    return Ok(());
                }
                // Hostname-based private detection isn't possible without DNS
                // resolution; block only the well-known metadata aliases.
                let lowered = domain.to_lowercase();
                const BLOCKED_HOSTS: &[&str] = &["metadata.google.internal", "metadata.goog"];
                if !self.allow_private_ranges && BLOCKED_HOSTS.contains(&lowered.as_str()) {
                    return Err(format!(
                        "blocked by network policy: {domain} is a metadata endpoint"
                    ));
                }
                Ok(())
            }
            ip @ (url::Host::Ipv4(_) | url::Host::Ipv6(_)) => {
                let ip_addr = match ip {
                    url::Host::Ipv4(v4) => IpAddr::V4(v4),
                    url::Host::Ipv6(v6) => IpAddr::V6(v6),
                    url::Host::Domain(_) => unreachable!(),
                };
                if self.host_allowed(&ip_addr.to_string()) {
                    return Ok(());
                }
                if !self.allow_private_ranges && Self::is_blocked_ip(ip_addr) {
                    return Err(format!(
                        "blocked by network policy: {ip_addr} is a non-public address"
                    ));
                }
                Ok(())
            }
        }
    }

    /// Is this host in the allowlist (exact match or wildcard suffix)?
    pub fn host_allowed(&self, host: &str) -> bool {
        let lowered = host.to_lowercase();
        self.allow_hosts
            .iter()
            .any(|allowed| match allowed.strip_prefix("*.") {
                // Dot-boundary match: `*.example.com` must NOT match sibling
                // registrable domains like `evil-example.com` or
                // `xexample.com`, only true subdomains and the apex itself.
                Some(suffix) => lowered == suffix || lowered.ends_with(&format!(".{suffix}")),
                None => lowered == allowed.to_lowercase(),
            })
    }

    fn is_blocked_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_private()
                    || v4.is_unspecified()
                    || v4.octets()[0] == 169 && v4.octets()[1] == 254 // link-local (169.254/16) incl. cloud metadata
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    // IPv4-mapped (::ffff:a.b.c.d)
                    || matches!(v6.to_ipv4_mapped(), Some(v4) if Self::is_blocked_ip(IpAddr::V4(v4)))
                    // IPv6 link-local fe80::/10 (SLAC neighbors, IPv6 metadata)
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // Unique-local fc00::/7 — includes AWS IMDS IPv6 fd00:ec2::254
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
            }
        }
    }

    /// Load policy from the 5-layer config, defaulting to the built-in defaults
    /// plus the well-known local provider hosts.
    pub fn from_config_dir(_config_dir: &PathBuf) -> Self {
        // Config plumbing is additive (T080 wires [sandbox.network_policy]);
        // until then seed the default allowlist with the local LLM provider.
        Self::with_allow_hosts(vec!["localhost".to_string(), "127.0.0.1".to_string()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> NetworkPolicy {
        NetworkPolicy::with_allow_hosts(vec!["localhost".into(), "127.0.0.1".into()])
    }

    #[test]
    fn metadata_endpoint_blocked() {
        let err = policy()
            .validate_url("http://169.254.169.254/latest/meta-data/")
            .unwrap_err();
        assert!(err.contains("blocked by network policy"), "{err}");
    }

    #[test]
    fn loopback_allowed_when_in_allowlist() {
        assert!(
            policy()
                .validate_url("http://localhost:11434/api/tags")
                .is_ok()
        );
    }

    #[test]
    fn loopback_ip_allowed_when_in_allowlist() {
        assert!(
            policy()
                .validate_url("http://127.0.0.1:8080/health")
                .is_ok()
        );
    }

    #[test]
    fn public_host_allowed() {
        assert!(
            policy()
                .validate_url("https://example.com/index.html")
                .is_ok()
        );
    }

    #[test]
    fn rfc1918_blocked() {
        for url in [
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
        ] {
            let err = policy().validate_url(url).unwrap_err();
            assert!(err.contains("blocked"), "{url}: {err}");
        }
    }

    #[test]
    fn ipv6_mapped_blocked() {
        let err = policy()
            .validate_url("http://[::ffff:169.254.169.254]/latest/")
            .unwrap_err();
        assert!(err.contains("blocked"), "{err}");
    }

    #[test]
    fn metadata_hostname_blocked() {
        let err = policy()
            .validate_url("http://metadata.google.internal/computeMetadata/")
            .unwrap_err();
        assert!(err.contains("metadata"), "{err}");
    }

    #[test]
    fn allow_private_ranges_permits_everything() {
        let p = NetworkPolicy {
            allow_hosts: vec![],
            allow_private_ranges: true,
        };
        assert!(p.validate_url("http://169.254.169.254/").is_ok());
        assert!(p.validate_url("http://10.0.0.1/").is_ok());
    }

    #[test]
    fn wildcard_allowlist() {
        let p = NetworkPolicy::with_allow_hosts(vec!["*.internal.example".into()]);
        assert!(p.host_allowed("api.internal.example"));
        assert!(p.host_allowed("internal.example"));
        assert!(p.host_allowed("API.Internal.Example"));
        // Sibling registrable domains must NOT match the wildcard.
        assert!(!p.host_allowed("evil-internal.example"));
        assert!(!p.host_allowed("xinternal.example"));
        assert!(!p.host_allowed("evil.internal.example.attacker.com"));
        assert!(p.validate_url("http://api.internal.example/v1").is_ok());
    }

    #[test]
    fn ipv6_link_local_and_ula_blocked() {
        for url in [
            "http://[fe80::1]/",
            "http://[fd00:ec2::254]/latest/meta-data/", // AWS IMDS IPv6
            "http://[fc00::1]/",
        ] {
            let err = policy().validate_url(url).unwrap_err();
            assert!(err.contains("blocked"), "{url}: {err}");
        }
    }

    #[test]
    fn invalid_url_rejected() {
        assert!(policy().validate_url("not-a-url").is_err());
    }
}
