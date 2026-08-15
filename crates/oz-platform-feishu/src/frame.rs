use std::collections::HashMap;

pub struct WsFrame {
    pub headers: HashMap<String, String>,
    pub payload_type: String,
    pub payload: Option<Vec<u8>>,
    pub method: i32,
}

pub fn decode_frame(data: &[u8]) -> Result<WsFrame, String> {
    let mut headers = HashMap::new();
    let mut payload_type = String::new();
    let mut payload: Option<Vec<u8>> = None;
    let mut method: i32 = 0;

    let mut pos = 0;
    while pos < data.len() {
        let (tag, n) = read_varint(&data[pos..])
            .ok_or_else(|| "protobuf: eof reading tag".to_string())?;
        pos += n;

        let field_num = (tag >> 3) as u32;
        let wire_type = tag & 0x07;

        match (field_num, wire_type) {
            (1, 0) | (2, 0) | (3, 0) => {
                let (_, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading varint".to_string())?;
                pos += n;
                if field_num == 4 { method = read_i32_from_bytes(&data[pos.saturating_sub(n)..pos]); }
            }
            (4, 0) => {
                let (val, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading varint".to_string())?;
                method = val as i32;
                pos += n;
            }
            (5, 2) => {
                let (len, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading header length".to_string())?;
                pos += n;
                let header_data = data.get(pos..pos.saturating_add(len as usize))
                    .ok_or_else(|| "protobuf: eof reading header body".to_string())?;
                pos += len as usize;
                if let Some((k, v)) = decode_header(header_data) {
                    headers.insert(k, v);
                }
            }
            (6, 2) => {
                let (len, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading payload_encoding length".to_string())?;
                pos += n + len as usize;
            }
            (7, 2) => {
                let (len, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading payload_type length".to_string())?;
                pos += n;
                let bytes = data.get(pos..pos.saturating_add(len as usize))
                    .ok_or_else(|| "protobuf: eof reading payload_type body".to_string())?;
                payload_type = String::from_utf8_lossy(bytes).to_string();
                pos += len as usize;
            }
            (8, 2) => {
                let (len, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading payload length".to_string())?;
                pos += n;
                let bytes = data.get(pos..pos.saturating_add(len as usize))
                    .ok_or_else(|| "protobuf: eof reading payload body".to_string())?;
                payload = Some(bytes.to_vec());
                pos += len as usize;
            }
            (9, 2) => {
                let (len, n) = read_varint(&data[pos..])
                    .ok_or_else(|| "protobuf: eof reading log_id_new length".to_string())?;
                pos += n + len as usize;
            }
            _ => {
                if wire_type == 0 {
                    let (_, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "protobuf: eof reading unknown varint".to_string())?;
                    pos += n;
                } else if wire_type == 2 {
                    let (len, n) = read_varint(&data[pos..])
                        .ok_or_else(|| "protobuf: eof reading unknown bytes length".to_string())?;
                    pos += n + len as usize;
                } else if wire_type == 1 {
                    pos += 8;
                } else if wire_type == 5 {
                    pos += 4;
                } else {
                    return Err(format!("protobuf: unknown wire type {wire_type}"));
                }
            }
        }
    }

    Ok(WsFrame { headers, payload_type, payload, method })
}

fn decode_header(data: &[u8]) -> Option<(String, String)> {
    let mut key = String::new();
    let mut value = String::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, n) = read_varint(&data[pos..])?;
        pos += n;
        let field_num = tag >> 3;
        let wire_type = tag & 0x07;
        match (field_num, wire_type) {
            (1, 2) => {
                let (len, n) = read_varint(&data[pos..])?;
                pos += n;
                let bytes = data.get(pos..pos.saturating_add(len as usize))?;
                key = String::from_utf8_lossy(bytes).to_string();
                pos += len as usize;
            }
            (2, 2) => {
                let (len, n) = read_varint(&data[pos..])?;
                pos += n;
                let bytes = data.get(pos..pos.saturating_add(len as usize))?;
                value = String::from_utf8_lossy(bytes).to_string();
                pos += len as usize;
            }
            _ => {
                if wire_type == 0 {
                    let (_, n) = read_varint(&data[pos..])?;
                    pos += n;
                } else if wire_type == 2 {
                    let (len, n) = read_varint(&data[pos..])?;
                    pos += n + len as usize;
                } else {
                    return None;
                }
            }
        }
    }
    Some((key, value))
}

fn read_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0;
    for (i, &b) in data.iter().enumerate() {
        value |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

fn read_i32_from_bytes(data: &[u8]) -> i32 {
    let mut value: u64 = 0;
    let mut shift = 0;
    let total = data.len().min(10);
    for &b in &data[..total] {
        value |= ((b & 0x7f) as u64) << shift;
        shift += 7;
    }
    value as i32
}

// ── Encoding helpers (for ACK frames) ──

fn write_varint(value: u64, buf: &mut Vec<u8>) {
    let mut v = value;
    while v >= 0x80 {
        buf.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn write_field_varint(field_num: u32, value: i32, buf: &mut Vec<u8>) {
    let tag = field_num << 3; // wire_type 0 = varint
    write_varint(tag as u64, buf);
    write_varint(value as u64, buf);
}

pub fn encode_ack_frame(method: i32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8);
    write_field_varint(4, method, &mut buf);
    buf
}
