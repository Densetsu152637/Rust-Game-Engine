//! JSON configuration of the reference `engine-relay-server` binary.
use crate::{
    Authorizer, ChannelId, Error, HmacCredentials, RelayLimits, RelayServer, Result,
    StaticCredentials, credential::unix_now,
};
use serde::Deserialize;
use std::{net::SocketAddr, time::Instant};

/// Top-level server file. Unknown fields are rejected. Without a file the server binds
/// `127.0.0.1:0`, uses default limits and refuses every allocation (discovery only).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    #[serde(default)]
    pub limits: RelayLimits,
    #[serde(default)]
    pub auth: RelayAuthConfig,
    /// Seconds between nonsecret statistics lines on stderr; 0 disables them.
    #[serde(default = "default_stats_interval")]
    pub stats_interval_secs: u64,
}
fn default_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}
fn default_stats_interval() -> u64 {
    60
}
impl Default for RelayServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            limits: RelayLimits::default(),
            auth: RelayAuthConfig::default(),
            stats_interval_secs: default_stats_interval(),
        }
    }
}

/// Credential mode.
///
/// `Debug` is written by hand and redacts every secret: the inline HMAC secret and the static
/// token list print as placeholders. The environment variable *name* is not a secret.
#[derive(Clone, Default, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelayAuthConfig {
    /// Every allocation is refused.
    #[default]
    Disabled,
    /// A fixed token list (each token at least 16 bytes).
    Static {
        credentials: Vec<StaticCredentialConfig>,
    },
    /// Stateless HMAC credentials ([`HmacCredentials`]); the secret is hex, read from the
    /// environment variable named by `secret_env` (preferred) or given inline as `secret_hex`.
    Hmac {
        #[serde(default)]
        secret_env: Option<String>,
        #[serde(default)]
        secret_hex: Option<String>,
    },
}

impl std::fmt::Debug for RelayAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("Disabled"),
            Self::Static { credentials } => f
                .debug_struct("Static")
                .field("credentials", &format!("[{} redacted]", credentials.len()))
                .finish(),
            Self::Hmac {
                secret_env,
                secret_hex,
            } => f
                .debug_struct("Hmac")
                .field("secret_env", secret_env)
                .field("secret_hex", &secret_hex.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticCredentialConfig {
    pub token: String,
    /// 32 hex digits.
    pub channel: String,
    pub party: crate::Party,
}
impl std::fmt::Debug for StaticCredentialConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StaticCredentialConfig(..)")
    }
}

/// Decodes lowercase or uppercase hex.
pub(crate) fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let bytes = text.as_bytes();
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[high, low]| Some(digit(high)? << 4 | digit(low)?))
        .collect()
}

impl RelayServerConfig {
    /// Parses JSON. Error messages carry only the error class and position, never input
    /// text, so a malformed file cannot leak a secret into logs.
    pub fn from_json(text: &str) -> Result<Self> {
        serde_json::from_str(text).map_err(|_| {
            Error::Invalid("relay server config is not valid JSON for the expected schema")
        })
    }
    /// Line and column of a JSON error, for operator diagnostics without content.
    pub fn json_error_position(text: &str) -> Option<(usize, usize)> {
        serde_json::from_str::<Self>(text)
            .err()
            .map(|e| (e.line(), e.column()))
    }
    /// The native authorizer for the configured mode. `env` resolves `secret_env`.
    pub fn authorizer(&self, env: impl Fn(&str) -> Option<String>) -> Result<Box<Authorizer>> {
        Ok(match &self.auth {
            RelayAuthConfig::Disabled => Box::new(|_: &[u8], _| None),
            RelayAuthConfig::Static { credentials } => {
                let mut list = StaticCredentials::new();
                for entry in credentials {
                    let channel: [u8; 16] = decode_hex(&entry.channel)
                        .and_then(|b| b.try_into().ok())
                        .ok_or(Error::Invalid(
                            "static credential channel must be 32 hex digits",
                        ))?;
                    list.add(entry.token.as_bytes(), ChannelId(channel), entry.party)?;
                }
                Box::new(list.into_authorizer())
            }
            RelayAuthConfig::Hmac {
                secret_env,
                secret_hex,
            } => {
                let hex = match (secret_env, secret_hex) {
                    (Some(name), None) => env(name).ok_or(Error::Invalid(
                        "relay secret environment variable is not set",
                    ))?,
                    (None, Some(hex)) => hex.clone(),
                    _ => {
                        return Err(Error::Invalid(
                            "set exactly one of secret_env or secret_hex",
                        ));
                    }
                };
                let secret = decode_hex(hex.trim())
                    .ok_or(Error::Invalid("relay HMAC secret must be hex"))?;
                let credentials = HmacCredentials::new(&secret)?;
                Box::new(move |credential: &[u8], _| credentials.verify(credential, unix_now()))
            }
        })
    }
    pub fn server(&self, env: impl Fn(&str) -> Option<String>) -> Result<RelayServer> {
        let authorize = self.authorizer(env)?;
        RelayServer::new(Instant::now(), self.limits.clone(), move |c, a| {
            authorize(c, a)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Security finding L1: `derive(Debug)` printed the HMAC secret into logs and panics.
    #[test]
    fn debug_output_never_contains_the_hmac_secret() {
        let secret = "ab".repeat(32);
        let text = format!(r#"{{"auth":{{"mode":"hmac","secret_hex":"{secret}"}}}}"#);
        let config = RelayServerConfig::from_json(&text).unwrap();
        for rendered in [
            format!("{config:?}"),
            format!("{config:#?}"),
            format!("{:?}", config.auth),
        ] {
            assert!(!rendered.contains(&secret), "{rendered}");
            assert!(!rendered.contains("abab"), "{rendered}");
        }
        // The non-secret shape stays visible for diagnostics.
        assert!(format!("{:?}", config.auth).contains("Hmac"));
        let named =
            RelayServerConfig::from_json(r#"{"auth":{"mode":"hmac","secret_env":"RELAY_SECRET"}}"#)
                .unwrap();
        assert!(format!("{named:?}").contains("RELAY_SECRET"));
    }

    #[test]
    fn configs_parse_validate_and_fail_closed() {
        let config = RelayServerConfig::from_json("{}").unwrap();
        assert_eq!(config.bind, default_bind());
        let auth = config.authorizer(|_| None).unwrap();
        assert!(auth(b"anything at all!", default_bind()).is_none());

        let text = r#"{"bind":"127.0.0.1:0","limits":{"max_allocations":4},
            "auth":{"mode":"static","credentials":[
              {"token":"0123456789abcdef","channel":"000102030405060708090a0b0c0d0e0f","party":"a"}]}}"#;
        let config = RelayServerConfig::from_json(text).unwrap();
        assert_eq!(config.limits.max_allocations, 4);
        let auth = config.authorizer(|_| None).unwrap();
        let grant = auth(b"0123456789abcdef", default_bind()).unwrap();
        assert_eq!(grant.channel.0[15], 15);
        assert!(config.server(|_| None).is_ok());

        let secret = "ab".repeat(32);
        let text = r#"{"auth":{"mode":"hmac","secret_env":"RELAY_SECRET"}}"#;
        let config = RelayServerConfig::from_json(text).unwrap();
        assert!(config.authorizer(|_| None).is_err(), "missing env var");
        let auth = config
            .authorizer(|name| (name == "RELAY_SECRET").then(|| secret.clone()))
            .unwrap();
        let issuer = HmacCredentials::new(&decode_hex(&secret).unwrap()).unwrap();
        let token = issuer.mint(ChannelId([9; 16]), crate::Party::A, unix_now() + 60);
        assert!(auth(&token, default_bind()).is_some());

        for bad in [
            r#"{"unknown":1}"#,
            r#"{"limits":{"max_allocations":4,"typo":1}}"#,
            r#"{"auth":{"mode":"static","credentials":[],"extra":true}}"#,
            r#"{"auth":{"mode":"telepathy"}}"#,
            r#"{"auth":{"mode":"hmac","secret_hex":"zz"#,
        ] {
            assert!(RelayServerConfig::from_json(bad).is_err(), "{bad}");
        }
        let both = r#"{"auth":{"mode":"hmac","secret_env":"X","secret_hex":"00"}}"#;
        assert!(
            RelayServerConfig::from_json(both)
                .unwrap()
                .authorizer(|_| None)
                .is_err()
        );
        let invalid_limits = r#"{"limits":{"max_payload_bytes":10}}"#;
        assert!(
            RelayServerConfig::from_json(invalid_limits)
                .unwrap()
                .server(|_| None)
                .is_err()
        );
        assert_eq!(decode_hex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(decode_hex("abc"), None);
    }
}
