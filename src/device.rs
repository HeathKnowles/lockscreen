use std::collections::HashMap;
use std::time::Instant;

use zbus::zvariant::{OwnedValue, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    NamePrefix(String),
    Address(String),
    ServiceUuid(String),
    Manufacturer(u16, Vec<u8>),
}

impl Matcher {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (kind, rest) = spec
            .split_once(':')
            .ok_or_else(|| format!("invalid --match `{spec}` (expected kind:value)"))?;
        let rest = rest.trim();
        if rest.is_empty() {
            return Err(format!("empty value in --match `{spec}`"));
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "name" | "alias" => Ok(Self::NamePrefix(rest.to_string())),
            "addr" | "address" => Ok(Self::Address(normalize_addr(rest)?)),
            "service" | "uuid" => {
                let uuid = normalize_uuid(rest);
                if !is_valid_uuid(&uuid) {
                    return Err(format!("invalid service uuid `{rest}`"));
                }
                if is_nil_uuid(&uuid) {
                    return Err(format!("`{rest}` is the nil UUID, not a usable service"));
                }
                Ok(Self::ServiceUuid(uuid))
            }
            "manufacturer" | "mfr" => {
                let (id_str, data_str) = match rest.split_once(':') {
                    Some((a, b)) => (a, b),
                    None => (rest, ""),
                };
                let id = parse_u16(id_str)
                    .ok_or_else(|| format!("invalid manufacturer id `{id_str}` in `{spec}`"))?;
                let data = parse_hex(data_str)
                    .map_err(|e| format!("invalid manufacturer data in `{spec}`: {e}"))?;
                Ok(Self::Manufacturer(id, data))
            }
            other => Err(format!(
                "unknown --match kind `{other}` (use name:, addr:, service:, manufacturer:)"
            )),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::NamePrefix(p) => format!("name prefix `{p}`"),
            Self::Address(a) => format!("address {a}"),
            Self::ServiceUuid(u) => format!("service {u}"),
            Self::Manufacturer(id, data) => {
                if data.is_empty() {
                    format!("manufacturer 0x{id:04x}")
                } else {
                    format!("manufacturer 0x{id:04x} data {}", hex_bytes(data))
                }
            }
        }
    }
}

fn normalize_addr(s: &str) -> Result<String, String> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return Err(format!("invalid bluetooth address `{s}`"));
    }
    let upper = hex.to_ascii_uppercase();
    Ok(upper
        .as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(":"))
}

fn normalize_uuid(s: &str) -> String {
    let raw: String = s
        .chars()
        .filter(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    let bare = raw.replace('-', "").to_ascii_lowercase();
    if bare.len() == 4 {
        format!("0000{bare}-0000-1000-8000-00805f9b34fb")
    } else if bare.len() == 8 {
        format!("{bare}-0000-1000-8000-00805f9b34fb")
    } else if bare.len() == 32 {
        let (a, rest) = bare.split_at(8);
        let (b, rest) = rest.split_at(4);
        let (c, rest) = rest.split_at(4);
        let (d, e) = rest.split_at(4);
        format!("{a}-{b}-{c}-{d}-{e}")
    } else {
        raw.to_ascii_lowercase()
    }
}

fn uuid_eq(a: &str, b: &str) -> bool {
    let strip = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    strip(a) == strip(b)
}

/// BlueZ reports `00000000-0000-0000-0000-000000000000` for devices with no usable
/// service data; it is not a real advertisement.
fn is_nil_uuid(u: &str) -> bool {
    let bare: String = u.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    bare.len() == 32 && bare.chars().all(|c| c == '0')
}

fn is_valid_uuid(u: &str) -> bool {
    u.chars().filter(|c| c.is_ascii_hexdigit()).count() == 32
}

fn parse_u16(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else if s.chars().any(|c| c.is_ascii_alphabetic()) {
        u16::from_str_radix(s, 16).ok()
    } else {
        s.parse().ok().or_else(|| u16::from_str_radix(s, 16).ok())
    }
}

fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd number of hex digits `{s}`"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Debug, Default)]
pub struct Dev {
    pub path: String,
    pub address: String,
    pub name: Option<String>,
    pub alias: Option<String>,
    pub uuids: Vec<String>,
    pub service_data: Vec<String>,
    pub manufacturer: Vec<(u16, Vec<u8>)>,
    pub connected: bool,
    /// When `connected` last flipped true (None while disconnected).
    pub connected_since: Option<Instant>,
    pub rssi: Option<i16>,
    pub rssi_ema: Option<f64>,
    pub last_seen: Option<Instant>,
    /// Last time we saw real advertising evidence (RSSI or advertisement data),
    /// as opposed to a cached/connectivity property change.
    pub last_adv: Option<Instant>,
    pub matched: bool,
    pub advertised: bool,
}

fn address_from_path(path: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or_default();
    let Some(rest) = last.strip_prefix("dev_") else {
        return String::new();
    };
    let parts: Vec<&str> = rest.split('_').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.len() != 2) {
        return String::new();
    }
    parts.join(":").to_ascii_uppercase()
}

fn prop_str(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_owned)
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v).ok())
}

fn prop_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    props.get(key).and_then(|v| i16::try_from(v).ok())
}

fn prop_str_list(v: &OwnedValue) -> Vec<String> {
    match Value::try_from(v) {
        Ok(Value::Array(a)) => a.iter().filter_map(|e| String::try_from(e).ok()).collect(),
        _ => Vec::new(),
    }
}

fn prop_dict_keys(v: &OwnedValue) -> Vec<String> {
    match Value::try_from(v) {
        Ok(Value::Dict(d)) => d
            .iter()
            .filter_map(|(k, _)| String::try_from(k).ok())
            .collect(),
        _ => Vec::new(),
    }
}

fn prop_manufacturer(v: &OwnedValue) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    if let Ok(Value::Dict(d)) = Value::try_from(v) {
        for (k, val) in d.iter() {
            let Value::U16(id) = k else { continue };
            let inner = match val {
                Value::Value(boxed) => boxed.as_ref(),
                other => other,
            };
            let bytes = match inner {
                Value::Array(a) => a
                    .iter()
                    .filter_map(|e| match e {
                        Value::U8(b) => Some(*b),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            out.push((*id, bytes));
        }
    }
    out
}

impl Dev {
    pub fn new(path: String) -> Self {
        let address = address_from_path(&path);
        Self {
            path,
            address,
            ..Default::default()
        }
    }

    pub fn apply(&mut self, props: &HashMap<String, OwnedValue>, now: Instant) {
        if let Some(a) = prop_str(props, "Address") {
            self.address = a;
        }
        if let Some(a) = prop_str(props, "Alias") {
            self.alias = Some(a);
        }
        if let Some(n) = prop_str(props, "Name") {
            self.name = Some(n);
        }
        if let Some(c) = prop_bool(props, "Connected") {
            if c != self.connected {
                self.connected_since = c.then_some(now);
            }
            self.connected = c;
        }
        if let Some(u) = props.get("UUIDs") {
            self.uuids = prop_str_list(u)
                .into_iter()
                .filter(|x| !is_nil_uuid(x))
                .collect();
        }
        if let Some(sd) = props.get("ServiceData") {
            self.service_data = prop_dict_keys(sd)
                .into_iter()
                .filter(|x| !is_nil_uuid(x))
                .collect();
        }
        if let Some(md) = props.get("ManufacturerData") {
            self.manufacturer = prop_manufacturer(md);
        }

        if let Some(r) = prop_i16(props, "RSSI") {
            self.rssi = Some(r);
            self.rssi_ema = Some(match self.rssi_ema {
                None => f64::from(r),
                Some(e) => e + 0.3 * (f64::from(r) - e),
            });
            self.advertised = true;
            self.last_seen = Some(now);
            self.last_adv = Some(now);
        } else if props.keys().any(|k| {
            matches!(
                k.as_str(),
                "ManufacturerData" | "ServiceData" | "TxPower" | "Flags" | "Name" | "UUIDs"
            )
        }) {
            self.advertised = true;
            self.last_seen = Some(now);
            self.last_adv = Some(now);
        } else if props.keys().any(|k| k == "Connected") {
            self.last_seen = Some(now);
        }
    }

    pub fn label(&self) -> String {
        let candidate = self
            .alias
            .clone()
            .or_else(|| self.name.clone())
            .unwrap_or_else(|| self.address.clone());
        // BlueZ falls back to a dash-separated address as the alias; show it as an address.
        if !self.address.is_empty()
            && candidate
                .replace('-', ":")
                .eq_ignore_ascii_case(&self.address)
        {
            return self.address.clone();
        }
        if candidate.is_empty() {
            self.path.clone()
        } else {
            candidate
        }
    }

    pub fn matches(&self, m: &Matcher) -> bool {
        match m {
            Matcher::NamePrefix(prefix) => {
                let p = prefix.to_ascii_lowercase();
                self.alias
                    .as_deref()
                    .or(self.name.as_deref())
                    .map(|n| n.to_ascii_lowercase().starts_with(&p))
                    .unwrap_or(false)
            }
            Matcher::Address(a) => self.address.eq_ignore_ascii_case(a),
            Matcher::ServiceUuid(u) => {
                self.uuids.iter().any(|x| uuid_eq(x, u))
                    || self.service_data.iter().any(|x| uuid_eq(x, u))
            }
            Matcher::Manufacturer(id, data) => self
                .manufacturer
                .iter()
                .any(|(mid, bytes)| mid == id && (data.is_empty() || bytes.starts_with(data))),
        }
    }
}
