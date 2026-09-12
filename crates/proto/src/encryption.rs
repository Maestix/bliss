//! The `encryption=` parameter of a VLESS link.
//!
//! Classic VLESS has no encryption of its own — it leans entirely on the
//! transport below it, and the parameter reads `none`. Xray-core 25.x added
//! VLESS Encryption (upstream PR #5067): a hybrid ML-KEM-768 + X25519 layer
//! that sits *inside* VLESS, independent of whatever REALITY or TLS is doing
//! underneath, so that a future quantum adversary who recorded the session
//! still cannot replay or impersonate.
//!
//! Parameter shape:
//!
//! ```text
//! mlkem768x25519plus.<native|xorpub|random>.<0rtt|1rtt|ticket>.<key>[.<key-or-padding>…]
//! ```
//!
//! We parse it for validation and for what the UI shows, but we also keep the
//! string verbatim: through stage 1 the only consumer that acts on it is
//! xray-core, and it wants the original bytes.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::error::LinkError;

/// The only handshake defined so far. The name is deliberately open-ended
/// upstream, so treat an unknown one as "newer than us" rather than corrupt.
const HANDSHAKE: &str = "mlkem768x25519plus";

/// Length of an X25519 public key — the `Password` half of `xray x25519`.
const X25519_LEN: usize = 32;
/// Length of an ML-KEM-768 encapsulation key — the `Client` half of `xray mlkem768`.
const MLKEM768_LEN: usize = 1184;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encryption {
    /// `encryption=none`: no inner layer.
    None,
    /// `encryption=mlkem768x25519plus…`
    MlKem768X25519Plus(VlessEncryption),
}

#[derive(Clone, PartialEq, Eq)]
pub struct VlessEncryption {
    pub mode: AppearanceMode,
    pub rtt: RttMode,
    pub keys: Vec<EncryptionKey>,
    /// The parameter exactly as it appeared in the link.
    raw: String,
}

/// How the encrypted stream looks on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearanceMode {
    /// Raw format; headers look like TLS 1.3 records. Works with Vision.
    Native,
    /// Same, with the public-key part XORed so it carries no signature.
    XorPub,
    /// Fully random, like VMess/Shadowsocks. Works with Vision.
    Random,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RttMode {
    /// Start from the ticket step using a cached ticket. The server may still
    /// refuse, so a client must be able to fall back to the full handshake.
    Zero,
    /// Always run the full handshake.
    One,
    /// Inbound-style ticket lifetime, e.g. `600s` or `60-600s`. Seen on server
    /// configs rather than share links, but accepted here rather than rejected.
    Ticket(String),
}

#[derive(Clone, PartialEq, Eq)]
pub enum EncryptionKey {
    X25519([u8; X25519_LEN]),
    MlKem768(Box<[u8; MLKEM768_LEN]>),
    /// Neither length: the spec allows short strings as padding between keys.
    Padding(String),
}

impl Encryption {
    pub fn parse(raw: &str) -> Result<Self, LinkError> {
        if raw.is_empty() || raw == "none" {
            return Ok(Self::None);
        }

        let mut blocks = raw.split('.');

        let handshake = blocks.next().unwrap_or_default();
        if handshake != HANDSHAKE {
            return Err(LinkError::Encryption(format!(
                "unknown handshake {handshake:?}, this build only speaks {HANDSHAKE:?}"
            )));
        }

        let mode = match blocks.next() {
            Some("native") => AppearanceMode::Native,
            Some("xorpub") => AppearanceMode::XorPub,
            Some("random") => AppearanceMode::Random,
            other => {
                return Err(LinkError::BadValue {
                    param: "encryption.mode",
                    expected: "native, xorpub or random",
                    got: other.unwrap_or_default().to_owned(),
                });
            }
        };

        let rtt = match blocks.next() {
            Some("0rtt") => RttMode::Zero,
            Some("1rtt") => RttMode::One,
            Some(other) if !other.is_empty() => RttMode::Ticket(other.to_owned()),
            _ => {
                return Err(LinkError::Encryption(
                    "missing the 0rtt/1rtt block".to_owned(),
                ));
            }
        };

        let keys: Vec<EncryptionKey> = blocks.map(EncryptionKey::decode).collect();
        if !keys.iter().any(EncryptionKey::is_key) {
            return Err(LinkError::Encryption(format!(
                "no server key: expected a base64url block of {X25519_LEN} or {MLKEM768_LEN} bytes"
            )));
        }

        Ok(Self::MlKem768X25519Plus(VlessEncryption {
            mode,
            rtt,
            keys,
            raw: raw.to_owned(),
        }))
    }

    /// The value to hand to xray-core verbatim.
    pub fn as_param(&self) -> &str {
        match self {
            Self::None => "none",
            Self::MlKem768X25519Plus(e) => &e.raw,
        }
    }
}

impl VlessEncryption {
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl EncryptionKey {
    fn decode(block: &str) -> Self {
        match URL_SAFE_NO_PAD.decode(block) {
            Ok(bytes) if bytes.len() == X25519_LEN => {
                Self::X25519(bytes.try_into().expect("length checked"))
            }
            Ok(bytes) if bytes.len() == MLKEM768_LEN => {
                let boxed: Box<[u8]> = bytes.into_boxed_slice();
                Self::MlKem768(boxed.try_into().expect("length checked"))
            }
            _ => Self::Padding(block.to_owned()),
        }
    }

    fn is_key(&self) -> bool {
        !matches!(self, Self::Padding(_))
    }
}

impl std::fmt::Display for Encryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_param())
    }
}

// `raw` holds the server key in full, and the keys are bytes. Both are printed
// wherever a config is — the log pane, `bliss check`, an error report — so the
// derive is replaced by something that stays diagnosable without being usable.
// An ML-KEM key would otherwise put 1184 bytes into a log line.

impl std::fmt::Debug for VlessEncryption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VlessEncryption")
            .field("mode", &self.mode)
            .field("rtt", &self.rtt)
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::X25519(k) => write!(f, "X25519({})", crate::redact::b64(k)),
            Self::MlKem768(k) => write!(f, "MlKem768({})", crate::redact::b64(k.as_slice())),
            Self::Padding(s) => write!(f, "Padding({s:?})"),
        }
    }
}

// Display is the user-facing spelling: it matches the words in the link, so the
// CLI, the log pane and the GUI all say what the config says. Debug stays for
// diagnostics.

impl std::fmt::Display for AppearanceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Native => "native",
            Self::XorPub => "xorpub",
            Self::Random => "random",
        })
    }
}

impl std::fmt::Display for RttMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => f.write_str("0rtt"),
            Self::One => f.write_str("1rtt"),
            Self::Ticket(s) => f.write_str(s),
        }
    }
}
