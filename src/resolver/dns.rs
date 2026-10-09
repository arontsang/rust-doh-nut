use bytes::Bytes;
use std::io::{Error, ErrorKind, Result};

const HEADER_LEN: usize = 12;

#[derive(Debug, PartialEq, Eq)]
struct Question {
    name: Vec<Vec<u8>>,
    qtype: [u8; 2],
    qclass: [u8; 2],
}

struct Message {
    flags: u16,
    question_end: usize,
    questions: Vec<Question>,
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

fn read_name(message: &[u8], offset: &mut usize) -> Result<Vec<Vec<u8>>> {
    let mut cursor = *offset;
    let mut consumed = None;
    let mut labels = Vec::new();
    let mut steps = 0;

    loop {
        if cursor >= message.len() || steps > message.len() {
            return Err(invalid("invalid DNS name"));
        }
        steps += 1;
        let length = message[cursor];
        match length & 0xc0 {
            0x00 => {
                cursor += 1;
                if length == 0 {
                    *offset = consumed.unwrap_or(cursor);
                    return Ok(labels);
                }
                let end = cursor + usize::from(length);
                if end > message.len() {
                    return Err(invalid("truncated DNS label"));
                }
                labels.push(message[cursor..end].iter().map(u8::to_ascii_lowercase).collect());
                cursor = end;
            }
            0xc0 => {
                if cursor + 1 >= message.len() {
                    return Err(invalid("truncated DNS compression pointer"));
                }
                let pointer = (usize::from(length & 0x3f) << 8) | usize::from(message[cursor + 1]);
                if pointer >= message.len() {
                    return Err(invalid("DNS compression pointer out of range"));
                }
                consumed.get_or_insert(cursor + 2);
                cursor = pointer;
            }
            _ => return Err(invalid("reserved DNS label encoding")),
        }
    }
}

fn parse_message(bytes: &[u8]) -> Result<Message> {
    if bytes.len() < HEADER_LEN {
        return Err(invalid("DNS message shorter than header"));
    }
    let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
    let questions_count = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let mut offset = HEADER_LEN;
    let mut questions = Vec::with_capacity(questions_count);
    for _ in 0..questions_count {
        let name = read_name(bytes, &mut offset)?;
        if offset + 4 > bytes.len() {
            return Err(invalid("truncated DNS question"));
        }
        questions.push(Question {
            name,
            qtype: [bytes[offset], bytes[offset + 1]],
            qclass: [bytes[offset + 2], bytes[offset + 3]],
        });
        offset += 4;
    }
    let question_end = offset;

    for count_offset in [6, 8, 10] {
        let count = usize::from(u16::from_be_bytes([
            bytes[count_offset],
            bytes[count_offset + 1],
        ]));
        for _ in 0..count {
            read_name(bytes, &mut offset)?;
            if offset + 10 > bytes.len() {
                return Err(invalid("truncated DNS resource record"));
            }
            let data_length = usize::from(u16::from_be_bytes([bytes[offset + 8], bytes[offset + 9]]));
            offset += 10;
            if offset + data_length > bytes.len() {
                return Err(invalid("truncated DNS resource data"));
            }
            offset += data_length;
        }
    }
    if offset != bytes.len() {
        return Err(invalid("trailing data after DNS message"));
    }

    Ok(Message {
        flags,
        question_end,
        questions,
    })
}

pub fn validate_query(query: &[u8]) -> Result<()> {
    let message = parse_message(query)?;
    if message.flags & 0x8000 != 0 {
        return Err(invalid("DNS query has the response bit set"));
    }
    Ok(())
}

pub fn validate_response(query: &[u8], response: &[u8]) -> Result<()> {
    validate_query(query)?;
    let query_message = parse_message(query)?;
    let response_message = parse_message(response)?;
    if response_message.flags & 0x8000 == 0 {
        return Err(invalid("DNS response bit is not set"));
    }
    if query[..2] != response[..2] {
        return Err(invalid("DNS response transaction ID does not match query"));
    }
    if query_message.flags & 0x7800 != response_message.flags & 0x7800 {
        return Err(invalid("DNS response opcode does not match query"));
    }
    if query_message.questions != response_message.questions {
        return Err(invalid("DNS response question does not match query"));
    }
    Ok(())
}

pub fn servfail_response(query: &[u8]) -> Result<Bytes> {
    validate_query(query)?;
    let message = parse_message(query)?;
    let flags = message.flags & (0x7800 | 0x0100 | 0x0010) | 0x8082;
    let mut response = Vec::with_capacity(message.question_end);
    response.extend_from_slice(&query[..2]);
    response.extend_from_slice(&flags.to_be_bytes());
    response.extend_from_slice(&query[4..6]);
    response.extend_from_slice(&[0; 6]);
    response.extend_from_slice(&query[HEADER_LEN..message.question_end]);
    Ok(Bytes::from(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn query() -> Vec<u8> {
        b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x03www\x07example\x03com\x00\x00\x01\x00\x01".to_vec()
    }

    pub fn response() -> Vec<u8> {
        let mut response = query();
        response[2] = 0x81;
        response[3] = 0x80;
        response
    }

    #[test]
    fn validates_correlated_dns_message() {
        validate_response(&query(), &response()).unwrap();
    }

    #[test]
    fn rejects_malformed_message_and_mismatched_response() {
        assert!(validate_query(b"dns").is_err());
        let mut response = response();
        response[1] ^= 1;
        assert!(validate_response(&query(), &response).is_err());
    }

    #[test]
    fn builds_servfail_with_transaction_and_question() {
        let query = query();
        let servfail = servfail_response(&query).unwrap();
        validate_response(&query, &servfail).unwrap();
        assert_eq!(u16::from_be_bytes([servfail[2], servfail[3]]) & 0xf, 2);
    }
}
