use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub sans: Vec<String>,
    pub is_ca: bool,
    pub self_signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PemBlock {
    Certificate(Result<CertInfo, String>),
    Other(String),
}

#[derive(Clone, Copy)]
struct Tlv {
    tag: u8,
    start: usize,
    end: usize,
}

fn tlv(b: &[u8], off: usize) -> Result<Tlv, String> {
    let tag = *b.get(off).ok_or_else(|| "truncated".to_string())?;
    let len_byte = *b.get(off + 1).ok_or_else(|| "truncated".to_string())?;
    let mut len = (len_byte & 0x7f) as usize;
    let mut start = off + 2;
    if (len_byte & 0x80) != 0 {
        let n = len;
        if n == 0 || n > 4 || start + n > b.len() {
            return Err("unsupported length".into());
        }
        len = 0;
        for i in 0..n {
            len = len * 256 + b[start + i] as usize;
        }
        start += n;
    }
    if start + len > b.len() {
        return Err("truncated".into());
    }
    Ok(Tlv {
        tag,
        start,
        end: start + len,
    })
}

fn children(b: &[u8], t: Tlv) -> Result<Vec<Tlv>, String> {
    let mut out = Vec::new();
    let mut off = t.start;
    while off < t.end {
        let c = tlv(b, off)?;
        off = c.end;
        out.push(c);
    }
    Ok(out)
}

fn oid(b: &[u8], t: Tlv) -> String {
    if t.start >= t.end {
        return String::new();
    }
    let first = b[t.start];
    let mut parts = vec![(first / 40).to_string(), (first % 40).to_string()];
    let mut n: u64 = 0;
    for &byte in &b[t.start + 1..t.end] {
        n = n.saturating_mul(128).saturating_add((byte & 0x7f) as u64);
        if (byte & 0x80) == 0 {
            parts.push(n.to_string());
            n = 0;
        }
    }
    parts.join(".")
}

fn text(b: &[u8], t: Tlv) -> String {
    String::from_utf8_lossy(&b[t.start..t.end]).into_owned()
}

fn name_attr(o: &str) -> &str {
    match o {
        "2.5.4.3" => "CN",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        _ => o,
    }
}

fn parse_name(b: &[u8], t: Tlv) -> Result<String, String> {
    let mut out = Vec::new();
    for set in children(b, t)? {
        for atv in children(b, set)? {
            let parts = children(b, atv)?;
            if parts.len() >= 2 {
                let o = oid(b, parts[0]);
                out.push(format!("{}={}", name_attr(&o), text(b, parts[1])));
            }
        }
    }
    Ok(out.join(", "))
}

fn parse_time(b: &[u8], t: Tlv) -> Result<DateTime<Utc>, String> {
    let s = text(b, t);
    let full = if t.tag == 0x17 {
        let two: u32 = s
            .get(0..2)
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| "invalid time".to_string())?;
        if two >= 50 {
            format!("19{s}")
        } else {
            format!("20{s}")
        }
    } else {
        s
    };
    let get_u32 = |a: usize, b_idx: usize| -> Result<u32, String> {
        full.get(a..b_idx)
            .and_then(|x| x.parse().ok())
            .ok_or_else(|| "invalid time".to_string())
    };
    let y = get_u32(0, 4)? as i32;
    let mo = get_u32(4, 6)?;
    let d = get_u32(6, 8)?;
    let h = get_u32(8, 10)?;
    let mi = get_u32(10, 12)?;
    let se = full.get(12..14).and_then(|x| x.parse().ok()).unwrap_or(0);
    Utc.with_ymd_and_hms(y, mo, d, h, mi, se)
        .single()
        .ok_or_else(|| "invalid date".into())
}

fn format_ip(bytes: &[u8]) -> String {
    if bytes.len() == 4 {
        return format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]);
    }
    bytes
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| format!("{:x}", ((c[0] as u16) << 8) | c[1] as u16))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn parse_certificate(der: &[u8]) -> Result<CertInfo, String> {
    let cert = tlv(der, 0)?;
    let cert_children = children(der, cert)?;
    let tbs_tlv = *cert_children.first().ok_or_else(|| "missing tbs".to_string())?;
    let tbs = children(der, tbs_tlv)?;
    let mut i = if tbs.first().is_some_and(|t| t.tag == 0xa0) {
        1
    } else {
        0
    };
    i += 2; // serial, signature algorithm
    if i + 2 >= tbs.len() {
        return Err("truncated tbs".into());
    }
    let issuer = parse_name(der, tbs[i])?;
    i += 1;
    let validity = children(der, tbs[i])?;
    if validity.len() < 2 {
        return Err("invalid validity".into());
    }
    let not_before = parse_time(der, validity[0])?;
    let not_after = parse_time(der, validity[1])?;
    i += 1;
    let subject = parse_name(der, tbs[i])?;

    let mut info = CertInfo {
        self_signed: subject == issuer,
        subject,
        issuer,
        not_before,
        not_after,
        sans: Vec::new(),
        is_ca: false,
    };

    if let Some(&ext) = tbs.iter().find(|t| t.tag == 0xa3) {
        if let Some(&seq) = children(der, ext)?.first() {
            for e in children(der, seq)? {
                let parts = children(der, e)?;
                if parts.len() < 2 {
                    continue;
                }
                let id = oid(der, parts[0]);
                let val_outer = parts[parts.len() - 1];
                if let Ok(value) = tlv(der, val_outer.start) {
                    if id == "2.5.29.17" {
                        for g in children(der, value)? {
                            if matches!(g.tag, 0x82 | 0x81 | 0x86) {
                                info.sans.push(text(der, g));
                            } else if g.tag == 0x87 {
                                info.sans.push(format_ip(&der[g.start..g.end]));
                            }
                        }
                    } else if id == "2.5.29.19" {
                        if let Some(&ca) = children(der, value)?.first() {
                            info.is_ca = ca.tag == 0x01 && der[ca.start] != 0;
                        }
                    }
                }
            }
        }
    }
    Ok(info)
}

pub fn parse_pem(value: &str) -> Vec<PemBlock> {
    let mut blocks = Vec::new();
    let mut rest = value;
    while let Some(begin_idx) = rest.find("-----BEGIN ") {
        let after_begin = &rest[begin_idx + 11..];
        let Some(header_end) = after_begin.find("-----") else {
            break;
        };
        let label = after_begin[..header_end].trim();
        let body_start = &after_begin[header_end + 5..];
        let end_marker = format!("-----END {label}-----");
        let Some(end_idx) = body_start.find(&end_marker) else {
            break;
        };
        let body = &body_start[..end_idx];
        if label == "CERTIFICATE" {
            let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            let parsed = base64::engine::general_purpose::STANDARD
                .decode(compact)
                .map_err(|e| e.to_string())
                .and_then(|der| parse_certificate(&der));
            blocks.push(PemBlock::Certificate(parsed));
        } else {
            blocks.push(PemBlock::Other(label.to_string()));
        }
        rest = &body_start[end_idx + end_marker.len()..];
    }
    blocks
}

fn format_pem_blocks(blocks: &[PemBlock]) -> Vec<String> {
    let mut out = Vec::new();
    let now = Utc::now();
    for (idx, b) in blocks.iter().enumerate() {
        match b {
            PemBlock::Certificate(Ok(cert)) => {
                let tag = if blocks.len() > 1 {
                    format!("[Certificate #{}]", idx + 1)
                } else {
                    "[Certificate]".to_string()
                };
                out.push(tag);
                if !cert.subject.is_empty() {
                    out.push(format!("Subject: {}", cert.subject));
                }
                if cert.self_signed {
                    out.push("Issuer: (self-signed)".into());
                } else if !cert.issuer.is_empty() {
                    out.push(format!("Issuer: {}", cert.issuer));
                }
                let status = if cert.not_after < now {
                    "EXPIRED".to_string()
                } else {
                    let days = (cert.not_after - now).num_days();
                    format!("{days}d left")
                };
                out.push(format!(
                    "Valid: {} .. {} ({status})",
                    cert.not_before.format("%Y-%m-%d %H:%M UTC"),
                    cert.not_after.format("%Y-%m-%d %H:%M UTC"),
                ));
                if cert.is_ca {
                    out.push("CA: true".into());
                }
                if !cert.sans.is_empty() {
                    out.push(format!("SANs: {}", cert.sans.join(", ")));
                }
            }
            PemBlock::Certificate(Err(e)) => {
                out.push(format!("[Certificate: error: {e}]"));
            }
            PemBlock::Other(label) => {
                out.push(format!("[PEM: {label}]"));
            }
        }
    }
    out
}

fn try_interpret_jwt(value: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = value.trim().split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let decode_part = |s: &str| -> Option<serde_json::Value> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(s))
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    let header = decode_part(parts[0])?;
    let payload = decode_part(parts[1])?;
    if !header.is_object() || !payload.is_object() {
        return None;
    }
    let mut out = vec!["[JWT]".to_string()];
    if let Ok(h) = serde_json::to_string_pretty(&header) {
        out.push("Header:".into());
        out.extend(h.lines().map(|l| format!("  {l}")));
    }
    if let Ok(p) = serde_json::to_string_pretty(&payload) {
        out.push("Payload:".into());
        out.extend(p.lines().map(|l| format!("  {l}")));
    }
    if let Some(exp) = payload.get("exp").and_then(|v| v.as_i64()) {
        if let Some(dt) = Utc.timestamp_opt(exp, 0).single() {
            out.push(format!("Expires: {}", dt.format("%Y-%m-%d %H:%M:%S UTC")));
        }
    }
    Some(out)
}

fn interpret_plain_value(trimmed: &str) -> Option<Vec<String>> {
    if trimmed.contains("-----BEGIN ") {
        let blocks = parse_pem(trimmed);
        if !blocks.is_empty() {
            return Some(format_pem_blocks(&blocks));
        }
    }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                return Some(pretty.lines().map(ToString::to_string).collect());
            }
        }
    }
    if let Some(jwt) = try_interpret_jwt(trimmed) {
        return Some(jwt);
    }
    None
}

fn interpret_value(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if let Some(rendered) = interpret_plain_value(trimmed) {
        return Some(rendered);
    }
    // Automatically base64-decode if the decoded content is an interpretable format (PEM, JSON, JWT).
    if trimmed.len() >= 12 && !trimmed.chars().any(char::is_whitespace) {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(trimmed) {
            if let Ok(text) = String::from_utf8(bytes) {
                if let Some(rendered) = interpret_plain_value(text.trim()) {
                    return Some(rendered);
                }
            }
        }
    }
    None
}

/// Interprets `data:` / `stringData:` values in YAML (decoding base64 first if needed),
/// rendering X.509 certificates, PEM blocks, JSON, and JWTs in a readable form.
pub fn interpret_yaml_lines(raw: &[String]) -> Vec<String> {
    let decoded = crate::app::decode_yaml_data_lines(raw);
    let mut out = Vec::with_capacity(decoded.len());
    let mut in_data = false;
    let mut i = 0;

    while i < decoded.len() {
        let line = &decoded[i];
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !line.starts_with(' ')
            && !line.starts_with('\t')
        {
            in_data = trimmed == "data:" || trimmed == "stringData:";
            out.push(line.clone());
            i += 1;
            continue;
        }

        if let Some((key_part, val_part)) = line.split_once(':') {
            let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let val_trimmed = val_part.trim();
            if val_trimmed == "|" || val_trimmed == "|-" {
                let mut block_lines = Vec::new();
                let mut j = i + 1;
                let block_indent_len = indent.len() + 2;
                while j < decoded.len() {
                    let next = &decoded[j];
                    let next_indent = next.chars().take_while(|c| *c == ' ' || *c == '\t').count();
                    if !next.trim().is_empty() && next_indent < block_indent_len {
                        break;
                    }
                    let stripped = if next.len() >= block_indent_len {
                        &next[block_indent_len..]
                    } else {
                        next.trim()
                    };
                    block_lines.push(stripped);
                    j += 1;
                }
                let joined = block_lines.join("\n");
                if let Some(rendered) = interpret_value(&joined) {
                    out.push(format!("{key_part}: |"));
                    for r in rendered {
                        out.push(format!("{indent}  {r}"));
                    }
                    i = j;
                    continue;
                }
            } else if !val_trimmed.is_empty() {
                let unquoted = val_trimmed
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .or_else(|| val_trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                    .unwrap_or(val_trimmed);
                if let Some(rendered) = interpret_value(unquoted) {
                    out.push(format!("{key_part}: |"));
                    for r in rendered {
                        out.push(format!("{indent}  {r}"));
                    }
                    i += 1;
                    continue;
                }
            }
        }
        let _ = in_data;
        out.push(line.clone());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CERT_PEM: &str = "\
-----BEGIN CERTIFICATE-----
MIIB0jCCAXigAwIBAgIUNp6MHyIQ8bHvQ+NJKuO+C3e37iQwCgYIKoZIzj0EAwIw
NTEYMBYGA1UECgwPU3RlZXJnbGFzcyBUZXN0MRkwFwYDVQQDDBBkZW1vLmV4YW1w
bGUuY29tMB4XDTI2MTEwMTIzNTU0MFoXDTI3MTEwMTIzNTU0MFowNTEYMBYGA1UE
CgwPU3RlZXJnbGFzcyBUZXN0MRkwFwYDVQQDDBBkZW1vLmV4YW1wbGUuY29tMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEOMZcO28n1iD7dihlNW8f6suWO08KeVUR
iavRkoayZytU8C8wQHrQhq8MXMNc+u2xuzXY10g1Rxp5nEiWdRxrnKNmMGQwHQYD
VR0OBBYEFIClUQkIqSGDYbs4Rt7L5wF3E5RsMDUGA1UdEQQuMCyCEGRlbW8uZXhh
bXBsZS5jb22CEiouZGVtby5leGFtcGxlLmNvbYcECgAAATAMBgNVHRMBAf8EAjAA
MAoGCCqGSM49BAMCA0gAMEUCIQCT9XnSudGPY7Hn39RdLvQSHpEn51co9tyNbJxk
EhIqkgIgeY9WwjaOrOvB+CVykxGawoYRZxydbDXTcArFSmMPUOE=
-----END CERTIFICATE-----";

    #[test]
    fn interprets_pem_cert_key_json_and_jwt() {
        let cert_b64 = base64::engine::general_purpose::STANDARD.encode(TEST_CERT_PEM);
        let jwt = format!(
            "{}.{}.sig",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#),
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"sub":"123","exp":1800000000}"#),
        );
        let raw: Vec<String> = vec![
            "apiVersion: v1".into(),
            "kind: Secret".into(),
            "data:".into(),
            format!("  tls.crt: {cert_b64}"),
            format!("  tls.key: {}", base64::engine::general_purpose::STANDARD.encode("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----")),
            format!("  cfg.json: {}", base64::engine::general_purpose::STANDARD.encode(r#"{"port":8080}"#)),
            format!("  token: {}", base64::engine::general_purpose::STANDARD.encode(&jwt)),
        ];

        let interpreted = interpret_yaml_lines(&raw);
        let joined = interpreted.join("\n");
        assert!(joined.contains("[Certificate]"), "got:\n{joined}");
        assert!(joined.contains("Subject: O=Steerglass Test, CN=demo.example.com"), "got:\n{joined}");
        assert!(joined.contains("Issuer: (self-signed)"), "got:\n{joined}");
        assert!(joined.contains("2026-11-01 23:55 UTC .. 2027-11-01 23:55 UTC"), "got:\n{joined}");
        assert!(joined.contains("SANs: demo.example.com, *.demo.example.com, 10.0.0.1"), "got:\n{joined}");
        assert!(joined.contains("[PEM: PRIVATE KEY]"), "got:\n{joined}");
        assert!(joined.contains("\"port\": 8080"), "got:\n{joined}");
        assert!(joined.contains("[JWT]"), "got:\n{joined}");
        assert!(joined.contains("\"sub\": \"123\""), "got:\n{joined}");

        // Also auto-decodes base64 certs outside top-level `data:` (e.g. `caBundle:` in webhooks)
        let webhook_raw = vec![
            "webhooks:".to_string(),
            "  - clientConfig:".to_string(),
            format!("      caBundle: {cert_b64}"),
        ];
        let webhook_interpreted = interpret_yaml_lines(&webhook_raw).join("\n");
        assert!(webhook_interpreted.contains("Subject: O=Steerglass Test, CN=demo.example.com"));
    }
}
