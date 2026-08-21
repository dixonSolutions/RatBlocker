//! Just enough DNS wire format to read a question and synthesize an answer.
//!
//! Written by hand rather than pulled in, because this is the daemon's most
//! exposed surface: every byte here arrives from the network. The parser is
//! strictly bounded, refuses compression pointers in the question section
//! (nothing legitimate uses them there, and they are the classic way to build
//! a parser loop), and never allocates based on an attacker-supplied length.

use std::fmt;

/// Maximum DNS message the daemon will look at. Larger UDP payloads are
/// dropped; larger TCP messages are refused.
pub const MAX_MESSAGE: usize = 4096;

/// Fixed DNS header size.
const HEADER_LEN: usize = 12;

/// Longest legal domain name in presentation form.
const MAX_NAME: usize = 253;
/// Longest legal label.
const MAX_LABEL: usize = 63;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DnsError {
    #[error("message is shorter than a DNS header")]
    Truncated,
    #[error("message is larger than {MAX_MESSAGE} bytes")]
    TooLong,
    #[error("message contains no question")]
    NoQuestion,
    #[error("question name is malformed")]
    BadName,
    #[error("question uses name compression")]
    CompressedQuestion,
    #[error("message is a response, not a query")]
    NotAQuery,
}

/// Record types the proxy distinguishes.
pub mod qtype {
    pub const A: u16 = 1;
    pub const AAAA: u16 = 28;
    pub const HTTPS: u16 = 65;
}

/// Response codes.
pub mod rcode {
    pub const NOERROR: u8 = 0;
    pub const SERVFAIL: u8 = 2;
    pub const NXDOMAIN: u8 = 3;
    pub const REFUSED: u8 = 5;
}

/// A parsed DNS question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub id: u16,
    /// Lowercased, dot-separated, no trailing dot.
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
    /// Byte offset just past the question, where an answer section begins.
    pub question_end: usize,
    /// Whether the client set the recursion-desired bit.
    pub recursion_desired: bool,
}

impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} type {}", self.name, self.qtype)
    }
}

/// Parse the first question out of a query message.
pub fn parse_query(buf: &[u8]) -> Result<Query, DnsError> {
    if buf.len() > MAX_MESSAGE {
        return Err(DnsError::TooLong);
    }
    if buf.len() < HEADER_LEN {
        return Err(DnsError::Truncated);
    }

    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    // QR bit set means this is a response; the proxy only answers queries.
    if flags & 0x8000 != 0 {
        return Err(DnsError::NotAQuery);
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    if qdcount == 0 {
        return Err(DnsError::NoQuestion);
    }

    let mut name = String::with_capacity(64);
    let mut pos = HEADER_LEN;
    loop {
        let len = *buf.get(pos).ok_or(DnsError::Truncated)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        // The top two bits of a length octet select the label type: 0b00 is an
        // ordinary label, 0b11 is a compression pointer, and 0b01/0b10 are
        // reserved and have never been assigned. Lumping the reserved values in
        // with pointers reported a malformed name as "uses compression", which
        // is a misleading thing to log about hostile input.
        match len & 0xc0 {
            0xc0 => return Err(DnsError::CompressedQuestion),
            0x00 => {}
            _ => return Err(DnsError::BadName),
        }
        // An ordinary label cannot exceed MAX_LABEL: the check above already
        // guarantees the top two bits are clear, so `len` is at most 63.
        debug_assert!(len <= MAX_LABEL);
        let end = pos.checked_add(len).ok_or(DnsError::Truncated)?;
        let label = buf.get(pos..end).ok_or(DnsError::Truncated)?;
        if !name.is_empty() {
            name.push('.');
        }
        if name.len() + label.len() > MAX_NAME {
            return Err(DnsError::BadName);
        }
        for &b in label {
            // Reject anything that cannot appear in a hostname rather than
            // lossily converting it.
            if !(b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'*')) {
                return Err(DnsError::BadName);
            }
            name.push(b.to_ascii_lowercase() as char);
        }
        pos = end;
    }

    let qtype_end = pos.checked_add(4).ok_or(DnsError::Truncated)?;
    if buf.len() < qtype_end {
        return Err(DnsError::Truncated);
    }
    let qtype = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
    let qclass = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]);

    if name.is_empty() {
        return Err(DnsError::NoQuestion);
    }

    Ok(Query {
        id,
        name,
        qtype,
        qclass,
        question_end: qtype_end,
        recursion_desired: flags & 0x0100 != 0,
    })
}

/// How a blocked name should be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockResponse {
    /// Report the name does not exist. Quietest for most clients.
    NxDomain,
    /// Answer with `0.0.0.0` / `::`, the classic sinkhole.
    ZeroAddress,
    /// Refuse the query outright.
    Refused,
}

impl Default for BlockResponse {
    fn default() -> Self {
        BlockResponse::NxDomain
    }
}

/// Build the response for a blocked name, echoing the original question.
pub fn build_blocked(query: &Query, request: &[u8], mode: BlockResponse, ttl: u32) -> Vec<u8> {
    let question = &request[HEADER_LEN..query.question_end];
    let mut out = Vec::with_capacity(HEADER_LEN + question.len() + 32);

    let (rcode, answers) = match mode {
        BlockResponse::NxDomain => (rcode::NXDOMAIN, 0u16),
        BlockResponse::Refused => (rcode::REFUSED, 0u16),
        BlockResponse::ZeroAddress => {
            let answerable = matches!(query.qtype, qtype::A | qtype::AAAA);
            (rcode::NOERROR, if answerable { 1 } else { 0 })
        }
    };

    out.extend_from_slice(&query.id.to_be_bytes());
    // QR=1, Opcode=0, AA=1, TC=0, RD as asked, RA=1, Z=0, RCODE.
    let mut flags: u16 = 0x8400;
    if query.recursion_desired {
        flags |= 0x0100;
    }
    flags |= 0x0080; // recursion available
    flags |= rcode as u16;
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&answers.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    out.extend_from_slice(question);

    if answers == 1 {
        // Point back at the question's name with a compression pointer.
        out.extend_from_slice(&[0xc0, HEADER_LEN as u8]);
        out.extend_from_slice(&query.qtype.to_be_bytes());
        out.extend_from_slice(&query.qclass.to_be_bytes());
        out.extend_from_slice(&ttl.to_be_bytes());
        match query.qtype {
            qtype::A => {
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&[0, 0, 0, 0]);
            }
            qtype::AAAA => {
                out.extend_from_slice(&16u16.to_be_bytes());
                out.extend_from_slice(&[0u8; 16]);
            }
            _ => unreachable!("answers only set for A/AAAA"),
        }
    }
    out
}

/// Build a bare error response, used when the upstream fails.
pub fn build_error(query: &Query, request: &[u8], code: u8) -> Vec<u8> {
    let question = &request[HEADER_LEN..query.question_end];
    let mut out = Vec::with_capacity(HEADER_LEN + question.len());
    out.extend_from_slice(&query.id.to_be_bytes());
    let flags: u16 = 0x8180 | code as u16;
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(question);
    out
}

/// Smallest TTL across the answer records, for cache expiry.
///
/// Best-effort: a response whose records cannot be walked is cached for the
/// caller's floor rather than being trusted or discarded.
pub fn minimum_ttl(buf: &[u8], question_end: usize, floor: u32) -> u32 {
    let Some(ancount) = buf.get(6..8).map(|b| u16::from_be_bytes([b[0], b[1]])) else {
        return floor;
    };
    if ancount == 0 {
        return floor;
    }
    let mut pos = question_end;
    let mut min = u32::MAX;
    for _ in 0..ancount {
        // Skip the record's name.
        loop {
            let len = match buf.get(pos) {
                Some(&l) => l as usize,
                None => return if min == u32::MAX { floor } else { min },
            };
            if len & 0xc0 == 0xc0 {
                pos += 2;
                break;
            }
            pos += 1;
            if len == 0 {
                break;
            }
            pos += len;
        }
        let Some(fields) = buf.get(pos..pos + 10) else {
            return if min == u32::MAX { floor } else { min };
        };
        let ttl = u32::from_be_bytes([fields[4], fields[5], fields[6], fields[7]]);
        let rdlen = u16::from_be_bytes([fields[8], fields[9]]) as usize;
        min = min.min(ttl);
        pos += 10 + rdlen;
    }
    if min == u32::MAX {
        floor
    } else {
        min.max(floor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query for `ads.example.com` type A.
    fn sample_query() -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in ["ads", "example", "com"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype::A.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn parses_a_normal_query() {
        let q = parse_query(&sample_query()).unwrap();
        assert_eq!(q.id, 0x1234);
        assert_eq!(q.name, "ads.example.com");
        assert_eq!(q.qtype, qtype::A);
        assert!(q.recursion_desired);
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(parse_query(&[]), Err(DnsError::Truncated));
        assert_eq!(parse_query(&[0; 11]), Err(DnsError::Truncated));
        assert_eq!(parse_query(&vec![0u8; MAX_MESSAGE + 1]), Err(DnsError::TooLong));
        // qdcount = 0
        assert_eq!(parse_query(&[0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]), Err(DnsError::NoQuestion));
        // A response, not a query.
        let mut response = sample_query();
        response[2] |= 0x80;
        assert_eq!(parse_query(&response), Err(DnsError::NotAQuery));
    }

    #[test]
    fn refuses_compression_pointers_in_the_question() {
        let mut q = vec![0x00, 0x01, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        q.extend_from_slice(&[0xc0, 0x0c]);
        assert_eq!(parse_query(&q), Err(DnsError::CompressedQuestion));
    }

    #[test]
    fn refuses_oversized_labels_and_names() {
        let mut q = vec![0x00, 0x01, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        q.push(64);
        q.extend_from_slice(&[b'a'; 64]);
        q.push(0);
        q.extend_from_slice(&[0, 1, 0, 1]);
        assert_eq!(parse_query(&q), Err(DnsError::BadName));
    }

    #[test]
    fn a_truncated_name_does_not_loop() {
        // A label length that runs off the end of the buffer.
        let mut q = vec![0x00, 0x01, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        q.push(50);
        q.extend_from_slice(b"short");
        assert_eq!(parse_query(&q), Err(DnsError::Truncated));
    }

    #[test]
    fn builds_an_nxdomain_response() {
        let request = sample_query();
        let query = parse_query(&request).unwrap();
        let response = build_blocked(&query, &request, BlockResponse::NxDomain, 60);
        assert_eq!(&response[0..2], &request[0..2], "id must be echoed");
        assert_eq!(response[2] & 0x80, 0x80, "QR bit must be set");
        assert_eq!(response[3] & 0x0f, rcode::NXDOMAIN);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 0);
    }

    #[test]
    fn builds_a_zero_address_response() {
        let request = sample_query();
        let query = parse_query(&request).unwrap();
        let response = build_blocked(&query, &request, BlockResponse::ZeroAddress, 60);
        assert_eq!(response[3] & 0x0f, rcode::NOERROR);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 1);
        assert_eq!(&response[response.len() - 4..], &[0, 0, 0, 0]);
    }
}
