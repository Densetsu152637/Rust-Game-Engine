use super::*;

pub(super) fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
pub(super) fn depth(value: &Value, current: usize, limit: usize) -> bool {
    if current > limit {
        return false;
    }
    match value {
        Value::Array(items) => items.iter().all(|v| depth(v, current + 1, limit)),
        Value::Object(items) => items.values().all(|v| depth(v, current + 1, limit)),
        _ => true,
    }
}
// The configured payload limit is <=64; reject excessive nesting before recursive
// parsing, keeping parser safety refusals distinct from recoverable malformed JSON.
pub(super) fn parser_nesting_within(bytes: &[u8]) -> bool {
    let (mut nesting, mut quoted, mut escaped) = (0_u32, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match *byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    nesting += 1;
                    if nesting > 96 {
                        return false;
                    }
                }
                b'}' | b']' => nesting = nesting.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}
