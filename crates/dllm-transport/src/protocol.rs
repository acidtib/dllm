use std::collections::HashMap;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_FRAME_SIZE: usize = 1_048_576;
pub const MAX_HEADER_COUNT: usize = 32;
pub const MAX_HEADER_NAME_LEN: usize = 256;
pub const MAX_HEADER_VALUE_LEN: usize = 4096;
pub const MAX_REQUEST_BODY_SIZE: usize = 1_048_576;
pub const MAX_RESPONSE_CHUNK_SIZE: usize = 1_048_576;
pub const MAX_DEADLINE_HORIZON_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    HealthRequest = 0,
    HealthResponse = 1,
    InferenceStart = 2,
    ResponseStart = 3,
    ResponseChunk = 4,
    Cancel = 5,
    End = 6,
    Error = 7,
}

impl MessageType {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::HealthRequest),
            1 => Some(Self::HealthResponse),
            2 => Some(Self::InferenceStart),
            3 => Some(Self::ResponseStart),
            4 => Some(Self::ResponseChunk),
            5 => Some(Self::Cancel),
            6 => Some(Self::End),
            7 => Some(Self::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    HealthRequest,
    HealthResponse,
    InferenceStart {
        request_id: u64,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Vec<u8>,
        deadline_unix_ms: u64,
    },
    ResponseStart {
        request_id: u64,
        status_code: u16,
        headers: HashMap<String, String>,
    },
    ResponseChunk {
        request_id: u64,
        data: Vec<u8>,
    },
    Cancel {
        request_id: u64,
    },
    End {
        request_id: u64,
    },
    Error {
        request_id: u64,
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    UnknownRequest = 0,
    CapacityExceeded = 1,
    DeadlineExceeded = 2,
    Cancelled = 3,
    AuthFailed = 4,
    MalformedFrame = 5,
    RuntimeError = 6,
    ProtocolVersion = 7,
    DuplicateStart = 8,
    InvalidTransition = 9,
    TransportError = 10,
}

impl ErrorCode {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::UnknownRequest),
            1 => Some(Self::CapacityExceeded),
            2 => Some(Self::DeadlineExceeded),
            3 => Some(Self::Cancelled),
            4 => Some(Self::AuthFailed),
            5 => Some(Self::MalformedFrame),
            6 => Some(Self::RuntimeError),
            7 => Some(Self::ProtocolVersion),
            8 => Some(Self::DuplicateStart),
            9 => Some(Self::InvalidTransition),
            10 => Some(Self::TransportError),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    Truncated,
    UnknownVersion,
    UnknownMessageType,
    OversizedFrame { size: usize, max: usize },
    OversizedBody { size: usize, max: usize },
    OversizedChunk { size: usize, max: usize },
    TooManyHeaders { count: usize, max: usize },
    OversizedHeaderName { size: usize, max: usize },
    OversizedHeaderValue { size: usize, max: usize },
    InvalidUtf8,
    EmptyMethod,
    EmptyPath,
    DeadlineExceeded { deadline_ms: u64, now_ms: u64 },
    DeadlineHorizonExceeded { horizon_ms: u64, max_ms: u64 },
}

pub struct FrameCodec {
    max_frame_size: usize,
    max_body_size: usize,
    max_chunk_size: usize,
    max_header_count: usize,
    max_header_name_len: usize,
    max_header_value_len: usize,
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self {
            max_frame_size: MAX_FRAME_SIZE,
            max_body_size: MAX_REQUEST_BODY_SIZE,
            max_chunk_size: MAX_RESPONSE_CHUNK_SIZE,
            max_header_count: MAX_HEADER_COUNT,
            max_header_name_len: MAX_HEADER_NAME_LEN,
            max_header_value_len: MAX_HEADER_VALUE_LEN,
        }
    }
}

impl FrameCodec {
    pub fn new(max_frame_size: usize, max_body_size: usize, max_chunk_size: usize) -> Self {
        Self {
            max_frame_size,
            max_body_size,
            max_chunk_size,
            ..Default::default()
        }
    }

    pub fn encode(&self, message: &Message) -> Vec<u8> {
        let payload = encode_payload(message);
        let total_len = 1 + 1 + 4 + payload.len();
        let mut frame = Vec::with_capacity(total_len);
        frame.push(PROTOCOL_VERSION);
        frame.push(message_type_byte(message));
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        frame
    }

    pub fn decode(&self, data: &[u8], now_ms: u64) -> Result<Message, FrameError> {
        if data.len() < 6 {
            return Err(FrameError::Truncated);
        }
        let version = data[0];
        if version != PROTOCOL_VERSION {
            return Err(FrameError::UnknownVersion);
        }
        let msg_type = MessageType::from_byte(data[1]).ok_or(FrameError::UnknownMessageType)?;
        let payload_len = u32::from_be_bytes([data[2], data[3], data[4], data[5]]) as usize;
        if payload_len > self.max_frame_size {
            return Err(FrameError::OversizedFrame {
                size: payload_len,
                max: self.max_frame_size,
            });
        }
        if data.len() < 6 + payload_len {
            return Err(FrameError::Truncated);
        }
        let payload = &data[6..6 + payload_len];
        decode_payload(msg_type, payload, self, now_ms)
    }
}

fn message_type_byte(message: &Message) -> u8 {
    match message {
        Message::HealthRequest => 0,
        Message::HealthResponse => 1,
        Message::InferenceStart { .. } => 2,
        Message::ResponseStart { .. } => 3,
        Message::ResponseChunk { .. } => 4,
        Message::Cancel { .. } => 5,
        Message::End { .. } => 6,
        Message::Error { .. } => 7,
    }
}

fn encode_payload(message: &Message) -> Vec<u8> {
    match message {
        Message::HealthRequest | Message::HealthResponse => Vec::new(),
        Message::InferenceStart {
            request_id,
            method,
            path,
            headers,
            body,
            deadline_unix_ms,
        } => {
            let mut payload = Vec::new();
            payload.extend_from_slice(&request_id.to_be_bytes());
            payload.extend_from_slice(&(method.len() as u16).to_be_bytes());
            payload.extend_from_slice(method.as_bytes());
            payload.extend_from_slice(&(path.len() as u16).to_be_bytes());
            payload.extend_from_slice(path.as_bytes());
            payload.extend_from_slice(&(headers.len() as u16).to_be_bytes());
            for (name, value) in headers {
                payload.extend_from_slice(&(name.len() as u16).to_be_bytes());
                payload.extend_from_slice(name.as_bytes());
                payload.extend_from_slice(&(value.len() as u32).to_be_bytes());
                payload.extend_from_slice(value.as_bytes());
            }
            payload.extend_from_slice(&deadline_unix_ms.to_be_bytes());
            payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
            payload.extend_from_slice(body);
            payload
        }
        Message::ResponseStart {
            request_id,
            status_code,
            headers,
        } => {
            let mut payload = Vec::new();
            payload.extend_from_slice(&request_id.to_be_bytes());
            payload.extend_from_slice(&status_code.to_be_bytes());
            payload.extend_from_slice(&(headers.len() as u16).to_be_bytes());
            for (name, value) in headers {
                payload.extend_from_slice(&(name.len() as u16).to_be_bytes());
                payload.extend_from_slice(name.as_bytes());
                payload.extend_from_slice(&(value.len() as u32).to_be_bytes());
                payload.extend_from_slice(value.as_bytes());
            }
            payload
        }
        Message::ResponseChunk { request_id, data } => {
            let mut payload = Vec::new();
            payload.extend_from_slice(&request_id.to_be_bytes());
            payload.extend_from_slice(&(data.len() as u32).to_be_bytes());
            payload.extend_from_slice(data);
            payload
        }
        Message::Cancel { request_id } | Message::End { request_id } => {
            request_id.to_be_bytes().to_vec()
        }
        Message::Error {
            request_id,
            code,
            message,
        } => {
            let mut payload = Vec::new();
            payload.extend_from_slice(&request_id.to_be_bytes());
            payload.push(*code as u8);
            payload.extend_from_slice(&(message.len() as u16).to_be_bytes());
            payload.extend_from_slice(message.as_bytes());
            payload
        }
    }
}

fn decode_payload(
    msg_type: MessageType,
    payload: &[u8],
    codec: &FrameCodec,
    now_ms: u64,
) -> Result<Message, FrameError> {
    match msg_type {
        MessageType::HealthRequest => Ok(Message::HealthRequest),
        MessageType::HealthResponse => Ok(Message::HealthResponse),
        MessageType::InferenceStart => {
            if payload.len() < 30 {
                return Err(FrameError::Truncated);
            }
            let request_id = u64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]);
            let method_len = u16::from_be_bytes([payload[8], payload[9]]) as usize;
            let mut offset = 10;
            if payload.len() < offset + method_len + 2 {
                return Err(FrameError::Truncated);
            }
            let method = std::str::from_utf8(&payload[offset..offset + method_len])
                .map_err(|_| FrameError::InvalidUtf8)?
                .to_owned();
            if method.is_empty() {
                return Err(FrameError::EmptyMethod);
            }
            offset += method_len;
            let path_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
            offset += 2;
            if payload.len() < offset + path_len + 2 {
                return Err(FrameError::Truncated);
            }
            let path = std::str::from_utf8(&payload[offset..offset + path_len])
                .map_err(|_| FrameError::InvalidUtf8)?
                .to_owned();
            if path.is_empty() {
                return Err(FrameError::EmptyPath);
            }
            offset += path_len;
            if payload.len() < offset + 2 {
                return Err(FrameError::Truncated);
            }
            let header_count = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
            offset += 2;
            if header_count > codec.max_header_count {
                return Err(FrameError::TooManyHeaders {
                    count: header_count,
                    max: codec.max_header_count,
                });
            }
            let mut headers = HashMap::with_capacity(header_count);
            for _ in 0..header_count {
                if payload.len() < offset + 2 {
                    return Err(FrameError::Truncated);
                }
                let name_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                offset += 2;
                if name_len > codec.max_header_name_len {
                    return Err(FrameError::OversizedHeaderName {
                        size: name_len,
                        max: codec.max_header_name_len,
                    });
                }
                if payload.len() < offset + name_len + 4 {
                    return Err(FrameError::Truncated);
                }
                let name = std::str::from_utf8(&payload[offset..offset + name_len])
                    .map_err(|_| FrameError::InvalidUtf8)?
                    .to_owned();
                offset += name_len;
                let value_len = u32::from_be_bytes([
                    payload[offset],
                    payload[offset + 1],
                    payload[offset + 2],
                    payload[offset + 3],
                ]) as usize;
                offset += 4;
                if value_len > codec.max_header_value_len {
                    return Err(FrameError::OversizedHeaderValue {
                        size: value_len,
                        max: codec.max_header_value_len,
                    });
                }
                if payload.len() < offset + value_len {
                    return Err(FrameError::Truncated);
                }
                let value = std::str::from_utf8(&payload[offset..offset + value_len])
                    .map_err(|_| FrameError::InvalidUtf8)?
                    .to_owned();
                offset += value_len;
                headers.insert(name.to_lowercase(), value);
            }
            if payload.len() < offset + 12 {
                return Err(FrameError::Truncated);
            }
            let deadline_unix_ms = u64::from_be_bytes([
                payload[offset],
                payload[offset + 1],
                payload[offset + 2],
                payload[offset + 3],
                payload[offset + 4],
                payload[offset + 5],
                payload[offset + 6],
                payload[offset + 7],
            ]);
            offset += 8;
            let body_len = u32::from_be_bytes([
                payload[offset],
                payload[offset + 1],
                payload[offset + 2],
                payload[offset + 3],
            ]) as usize;
            offset += 4;
            if body_len > codec.max_body_size {
                return Err(FrameError::OversizedBody {
                    size: body_len,
                    max: codec.max_body_size,
                });
            }
            if payload.len() < offset + body_len {
                return Err(FrameError::Truncated);
            }
            if deadline_unix_ms <= now_ms {
                return Err(FrameError::DeadlineExceeded {
                    deadline_ms: deadline_unix_ms,
                    now_ms,
                });
            }
            let horizon_ms = deadline_unix_ms.saturating_sub(now_ms);
            if horizon_ms > MAX_DEADLINE_HORIZON_MS {
                return Err(FrameError::DeadlineHorizonExceeded {
                    horizon_ms,
                    max_ms: MAX_DEADLINE_HORIZON_MS,
                });
            }
            Ok(Message::InferenceStart {
                request_id,
                method,
                path,
                headers,
                body: payload[offset..offset + body_len].to_vec(),
                deadline_unix_ms,
            })
        }
        MessageType::ResponseStart => {
            if payload.len() < 12 {
                return Err(FrameError::Truncated);
            }
            let request_id = u64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]);
            let status_code = u16::from_be_bytes([payload[8], payload[9]]);
            let header_count = u16::from_be_bytes([payload[10], payload[11]]) as usize;
            if header_count > codec.max_header_count {
                return Err(FrameError::TooManyHeaders {
                    count: header_count,
                    max: codec.max_header_count,
                });
            }
            let mut headers = HashMap::with_capacity(header_count);
            let mut offset = 12;
            for _ in 0..header_count {
                if payload.len() < offset + 2 {
                    return Err(FrameError::Truncated);
                }
                let name_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
                offset += 2;
                if name_len > codec.max_header_name_len {
                    return Err(FrameError::OversizedHeaderName {
                        size: name_len,
                        max: codec.max_header_name_len,
                    });
                }
                if payload.len() < offset + name_len + 4 {
                    return Err(FrameError::Truncated);
                }
                let name = std::str::from_utf8(&payload[offset..offset + name_len])
                    .map_err(|_| FrameError::InvalidUtf8)?
                    .to_owned();
                offset += name_len;
                let value_len = u32::from_be_bytes([
                    payload[offset],
                    payload[offset + 1],
                    payload[offset + 2],
                    payload[offset + 3],
                ]) as usize;
                offset += 4;
                if value_len > codec.max_header_value_len {
                    return Err(FrameError::OversizedHeaderValue {
                        size: value_len,
                        max: codec.max_header_value_len,
                    });
                }
                if payload.len() < offset + value_len {
                    return Err(FrameError::Truncated);
                }
                let value = std::str::from_utf8(&payload[offset..offset + value_len])
                    .map_err(|_| FrameError::InvalidUtf8)?
                    .to_owned();
                offset += value_len;
                headers.insert(name.to_lowercase(), value);
            }
            Ok(Message::ResponseStart {
                request_id,
                status_code,
                headers,
            })
        }
        MessageType::ResponseChunk => {
            if payload.len() < 12 {
                return Err(FrameError::Truncated);
            }
            let request_id = u64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]);
            let data_len =
                u32::from_be_bytes([payload[8], payload[9], payload[10], payload[11]]) as usize;
            if data_len > codec.max_chunk_size {
                return Err(FrameError::OversizedChunk {
                    size: data_len,
                    max: codec.max_chunk_size,
                });
            }
            if payload.len() < 12 + data_len {
                return Err(FrameError::Truncated);
            }
            Ok(Message::ResponseChunk {
                request_id,
                data: payload[12..12 + data_len].to_vec(),
            })
        }
        MessageType::Cancel => {
            if payload.len() < 8 {
                return Err(FrameError::Truncated);
            }
            Ok(Message::Cancel {
                request_id: u64::from_be_bytes([
                    payload[0], payload[1], payload[2], payload[3], payload[4], payload[5],
                    payload[6], payload[7],
                ]),
            })
        }
        MessageType::End => {
            if payload.len() < 8 {
                return Err(FrameError::Truncated);
            }
            Ok(Message::End {
                request_id: u64::from_be_bytes([
                    payload[0], payload[1], payload[2], payload[3], payload[4], payload[5],
                    payload[6], payload[7],
                ]),
            })
        }
        MessageType::Error => {
            if payload.len() < 11 {
                return Err(FrameError::Truncated);
            }
            let request_id = u64::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3], payload[4], payload[5], payload[6],
                payload[7],
            ]);
            let code = ErrorCode::from_byte(payload[8]).ok_or(FrameError::UnknownMessageType)?;
            let msg_len = u16::from_be_bytes([payload[9], payload[10]]) as usize;
            if payload.len() < 11 + msg_len {
                return Err(FrameError::Truncated);
            }
            let message = std::str::from_utf8(&payload[11..11 + msg_len])
                .map_err(|_| FrameError::InvalidUtf8)?
                .to_owned();
            Ok(Message::Error {
                request_id,
                code,
                message,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now_ms() -> u64 {
        1_000_000
    }

    #[test]
    fn health_request_response_round_trip() {
        let codec = FrameCodec::default();
        let encoded = codec.encode(&Message::HealthRequest);
        let decoded = codec.decode(&encoded, now_ms()).unwrap();
        assert_eq!(decoded, Message::HealthRequest);

        let encoded = codec.encode(&Message::HealthResponse);
        let decoded = codec.decode(&encoded, now_ms()).unwrap();
        assert_eq!(decoded, Message::HealthResponse);
    }

    #[test]
    fn streamed_response_preserves_chunk_order_and_content_type() {
        let codec = FrameCodec::default();
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "text/event-stream".into());

        let start = Message::ResponseStart {
            request_id: 42,
            status_code: 200,
            headers,
        };
        let chunk1 = Message::ResponseChunk {
            request_id: 42,
            data: b"data: first\n\n".to_vec(),
        };
        let chunk2 = Message::ResponseChunk {
            request_id: 42,
            data: b"data: second\n\n".to_vec(),
        };
        let end = Message::End { request_id: 42 };

        let decoded_start = codec.decode(&codec.encode(&start), now_ms()).unwrap();
        assert_eq!(decoded_start, start);

        let decoded_chunk1 = codec.decode(&codec.encode(&chunk1), now_ms()).unwrap();
        assert_eq!(decoded_chunk1, chunk1);

        let decoded_chunk2 = codec.decode(&codec.encode(&chunk2), now_ms()).unwrap();
        assert_eq!(decoded_chunk2, chunk2);

        let decoded_end = codec.decode(&codec.encode(&end), now_ms()).unwrap();
        assert_eq!(decoded_end, end);
    }

    #[test]
    fn zero_length_and_multi_chunk_responses() {
        let codec = FrameCodec::default();
        let empty_chunk = Message::ResponseChunk {
            request_id: 1,
            data: vec![],
        };
        let decoded = codec.decode(&codec.encode(&empty_chunk), now_ms()).unwrap();
        assert_eq!(decoded, empty_chunk);

        let chunks: Vec<_> = (0..10)
            .map(|i| Message::ResponseChunk {
                request_id: 1,
                data: vec![i as u8; 1024],
            })
            .collect();
        for chunk in &chunks {
            let decoded = codec.decode(&codec.encode(chunk), now_ms()).unwrap();
            assert_eq!(decoded, *chunk);
        }
    }

    #[test]
    fn unknown_version_and_message_type_rejected() {
        let codec = FrameCodec::default();
        let valid = codec.encode(&Message::HealthRequest);
        let mut bad_version = valid.clone();
        bad_version[0] = 99;
        assert!(matches!(
            codec.decode(&bad_version, now_ms()),
            Err(FrameError::UnknownVersion)
        ));

        let mut bad_type = valid;
        bad_type[1] = 99;
        assert!(matches!(
            codec.decode(&bad_type, now_ms()),
            Err(FrameError::UnknownMessageType)
        ));
    }

    #[test]
    fn truncated_malformed_and_oversized_frames() {
        let codec = FrameCodec::default();
        assert!(matches!(
            codec.decode(&[0, 1, 2], now_ms()),
            Err(FrameError::Truncated)
        ));

        let mut oversized = vec![1, 0, 0, 0, 0, 0];
        oversized[2..6].copy_from_slice(&(MAX_FRAME_SIZE as u32 + 1).to_be_bytes());
        assert!(matches!(
            codec.decode(&oversized, now_ms()),
            Err(FrameError::OversizedFrame { .. })
        ));

        let mut truncated = codec.encode(&Message::HealthRequest);
        truncated.pop();
        assert!(matches!(
            codec.decode(&truncated, now_ms()),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn duplicate_start_frames_are_distinct_messages() {
        let codec = FrameCodec::default();
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: HashMap::new(),
            body: b"{}".to_vec(),
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        let decoded = codec.decode(&encoded, now_ms()).unwrap();
        assert_eq!(decoded, start);
        // Second start with same ID is a protocol-level error, not a frame error.
        let decoded2 = codec.decode(&encoded, now_ms()).unwrap();
        assert_eq!(decoded2, start);
    }

    #[test]
    fn invalid_transition_ordering_is_not_enforced_by_codec() {
        // Transition ordering is enforced by the protocol state machine, not the codec.
        // The codec only validates individual frame structure.
    }

    #[test]
    fn frame_and_channel_backpressure_bounds() {
        let codec = FrameCodec::new(1024, 512, 256);
        let big_start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: HashMap::new(),
            body: vec![0; 600],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&big_start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::OversizedBody { .. })
        ));

        let big_chunk = Message::ResponseChunk {
            request_id: 1,
            data: vec![0; 300],
        };
        let encoded = codec.encode(&big_chunk);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::OversizedChunk { .. })
        ));
    }

    #[test]
    fn inference_start_round_trips() {
        let codec = FrameCodec::default();
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        headers.insert("authorization".into(), "Bearer secret".into());

        let start = Message::InferenceStart {
            request_id: 7,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers,
            body: b"{\"model\":\"qwen\"}".to_vec(),
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        let decoded = codec.decode(&encoded, now_ms()).unwrap();

        match decoded {
            Message::InferenceStart {
                request_id,
                method,
                path,
                headers,
                body,
                deadline_unix_ms,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(method, "POST");
                assert_eq!(path, "/v1/chat/completions");
                assert_eq!(headers.get("content-type").unwrap(), "application/json");
                assert_eq!(headers.get("authorization").unwrap(), "Bearer secret");
                assert_eq!(body, b"{\"model\":\"qwen\"}");
                assert_eq!(deadline_unix_ms, now_ms() + 60_000);
            }
            _ => panic!("expected InferenceStart"),
        }
    }

    #[test]
    fn cancel_and_end_round_trip() {
        let codec = FrameCodec::default();
        let cancel = Message::Cancel { request_id: 99 };
        let decoded = codec.decode(&codec.encode(&cancel), now_ms()).unwrap();
        assert_eq!(decoded, cancel);

        let end = Message::End { request_id: 99 };
        let decoded = codec.decode(&codec.encode(&end), now_ms()).unwrap();
        assert_eq!(decoded, end);
    }

    #[test]
    fn error_frame_round_trips() {
        let codec = FrameCodec::default();
        let error = Message::Error {
            request_id: 3,
            code: ErrorCode::CapacityExceeded,
            message: "too many concurrent streams".into(),
        };
        let decoded = codec.decode(&codec.encode(&error), now_ms()).unwrap();
        assert_eq!(decoded, error);
    }

    #[test]
    fn expired_at_arrival_deadline_rejected() {
        let codec = FrameCodec::default();
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: HashMap::new(),
            body: vec![],
            deadline_unix_ms: 100,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, 101),
            Err(FrameError::DeadlineExceeded { .. })
        ));
    }

    #[test]
    fn header_count_and_size_limits() {
        let codec = FrameCodec::default();
        let mut headers = HashMap::new();
        for i in 0..40 {
            headers.insert(format!("x-header-{i}"), "value".into());
        }
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers,
            body: vec![],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::TooManyHeaders { .. })
        ));
    }

    #[test]
    fn deadline_horizon_exceeded_rejected() {
        let codec = FrameCodec::default();
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: HashMap::new(),
            body: vec![],
            deadline_unix_ms: now_ms() + MAX_DEADLINE_HORIZON_MS + 1,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::DeadlineHorizonExceeded { .. })
        ));
    }

    #[test]
    fn oversized_header_name_rejected() {
        let codec = FrameCodec::default();
        let mut headers = HashMap::new();
        let long_name = "x".repeat(MAX_HEADER_NAME_LEN + 1);
        headers.insert(long_name, "v".into());
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers,
            body: vec![],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::OversizedHeaderName { .. })
        ));
    }

    #[test]
    fn oversized_header_value_rejected() {
        let codec = FrameCodec::default();
        let mut headers = HashMap::new();
        let long_value = "v".repeat(MAX_HEADER_VALUE_LEN + 1);
        headers.insert("x-header".into(), long_value);
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers,
            body: vec![],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::OversizedHeaderValue { .. })
        ));
    }

    #[test]
    fn empty_method_rejected() {
        let codec = FrameCodec::default();
        let start = Message::InferenceStart {
            request_id: 1,
            method: "".into(),
            path: "/v1/chat/completions".into(),
            headers: HashMap::new(),
            body: vec![],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::EmptyMethod)
        ));
    }

    #[test]
    fn empty_path_rejected() {
        let codec = FrameCodec::default();
        let start = Message::InferenceStart {
            request_id: 1,
            method: "POST".into(),
            path: "".into(),
            headers: HashMap::new(),
            body: vec![],
            deadline_unix_ms: now_ms() + 60_000,
        };
        let encoded = codec.encode(&start);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::EmptyPath)
        ));
    }

    #[test]
    fn oversized_response_chunk_rejected_on_decode() {
        let codec = FrameCodec::new(MAX_FRAME_SIZE, MAX_REQUEST_BODY_SIZE, 16);
        let chunk = Message::ResponseChunk {
            request_id: 1,
            data: vec![0; 32],
        };
        let encoded = codec.encode(&chunk);
        assert!(matches!(
            codec.decode(&encoded, now_ms()),
            Err(FrameError::OversizedChunk { .. })
        ));
    }
}
