//! Bounded printable frames survive SSH PTYs and scheduler hops. No escape
//! sequences or raw input records travel on the editing protocol. JSON is
//! compressed before framing; chunks are small enough for canonical PTYs too.
use super::*;
use std::{
    collections::HashMap,
    io::{BufRead, Read, Write},
};

#[cfg(test)]
#[path = "remote_wire_tests.rs"]
mod tests;

const PREFIX: &str = "VTCR26";
const CHUNK: usize = 2048;
pub(crate) const MAX_LINE: usize = 32 * 1024;
const MAX_MESSAGE: usize = MAX_PAYLOAD_BYTES + 1024 * 1024;
pub(super) const MAX_PENDING: usize = 8;
pub(crate) const EVENT_ID: u64 = 1 << 63;
const OSC: &[u8] = b"\x1b]777;vtcode-headless;";

/// Protect output records from terminal reflow in Windows ConPTY and nested
/// setup PTYs. Private OSC payloads pass through without becoming screen text.
pub(crate) fn write_record(writer: &mut impl Write, record: &str) -> io::Result<()> {
    writer.write_all(OSC)?;
    writer.write_all(record.as_bytes())?;
    writer.write_all(b"\x07\n")
}
pub(crate) fn record(line: &[u8]) -> &[u8] {
    // Server frames normally start with the envelope. Preserve the search for
    // decorated output, including a server OSC following echoed client text.
    let start = if line.starts_with(OSC) {
        Some(0)
    } else {
        memchr::memmem::find(line, OSC)
    };
    if let Some(start) = start {
        let rest = &line[start + OSC.len()..];
        if let Some(end) = memchr::memchr(7, rest) {
            return &rest[..end];
        }
    }
    line.trim_ascii_end()
}

pub(crate) fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    struct Limited(Vec<u8>);
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > (MAX_MESSAGE + 1).saturating_sub(self.0.len()) {
                return Err(io::Error::other("Remote message exceeds limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    // Apply the limit during serialization, including JSON escaping. A file
    // containing control characters must not allocate six times the limit.
    let mut data = Limited(vec![0]);
    serde_json::to_writer(&mut data, value)?;
    let data = data.0;
    if data.len() < 1024 {
        return Ok(data);
    }
    let mut compressed = flate2::write::ZlibEncoder::new(vec![1], flate2::Compression::fast());
    compressed.write_all(&data[1..])?;
    let compressed = compressed.finish()?;
    if compressed.len() < data.len() {
        Ok(compressed)
    } else {
        Ok(data)
    }
}

fn decode(mut data: Vec<u8>) -> io::Result<Vec<u8>> {
    match data.first() {
        Some(0) => {
            // Reuse the receive buffer instead of allocating another complete
            // message merely to discard its one-byte encoding marker.
            data.remove(0);
            Ok(data)
        }
        Some(1) => {
            let mut result = Vec::new();
            flate2::read::ZlibDecoder::new(&data[1..])
                .take((MAX_MESSAGE + 1) as u64)
                .read_to_end(&mut result)?;
            if result.len() > MAX_MESSAGE {
                return Err(io::Error::other("Expanded remote message exceeds limit"));
            }
            Ok(result)
        }
        _ => Err(io::Error::other("Invalid remote message encoding")),
    }
}

pub(crate) fn write_message(
    writer: &mut impl Write,
    direction: char,
    session: u64,
    id: u64,
    data: &[u8],
) -> io::Result<()> {
    write_interleaved(writer, direction, session, id, data, |_| Ok(()))
}
pub(crate) fn write_interleaved<W: Write>(
    writer: &mut W,
    direction: char,
    session: u64,
    id: u64,
    data: &[u8],
    mut control: impl FnMut(&mut W) -> io::Result<()>,
) -> io::Result<()> {
    if data.len() > MAX_MESSAGE + 1 || data.is_empty() {
        return Err(io::Error::other("Invalid remote message size"));
    }
    let mut record = String::with_capacity(data.len().min(CHUNK).div_ceil(3) * 4 + 128);
    for (sequence, chunk) in data.chunks(CHUNK).enumerate() {
        let last = usize::from((sequence + 1) * CHUNK >= data.len());
        write_frame(
            writer,
            direction,
            session,
            id,
            sequence,
            chunk,
            last != 0,
            &mut record,
        )?;
        control(writer)?;
    }
    writer.flush()
}

// Keep protocol fields explicit at the two framing call sites.
#[allow(clippy::too_many_arguments)]
fn write_frame(
    writer: &mut impl Write,
    direction: char,
    session: u64,
    id: u64,
    sequence: usize,
    chunk: &[u8],
    last: bool,
    record: &mut String,
) -> io::Result<()> {
    // Reuse one frame buffer, with no intermediate base64/format Strings.
    // One write includes the OSC envelope and stays within a canonical PTY
    // line. write_all still handles short writes and Interrupted correctly.
    use std::fmt::Write as _;
    record.clear();
    if direction == 'S' {
        record.push_str("\x1b]777;vtcode-headless;");
    }
    write!(
        record,
        "{PREFIX}{direction}:{session:016x}:{id:016x}:{sequence:x}:{}:",
        usize::from(last)
    )
    .expect("writing into a String cannot fail");
    URL_SAFE_NO_PAD.encode_string(chunk, record);
    if direction == 'S' {
        record.push('\x07');
    }
    record.push('\n');
    writer.write_all(record.as_bytes())
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Order {
    Request(u64),
    Process(u64),
}
struct Packet {
    id: u64,
    data: Vec<u8>,
    offset: usize,
    event: bool,
    interactive: bool,
    search: bool,
    order: Option<Order>,
    serial: u64,
}
/// Fairly multiplex file responses. Small process events complete between
/// file frames, so a large open cannot monopolize a slow connection.
#[derive(Default)]
pub(crate) struct OutputQueue {
    packets: std::collections::VecDeque<Packet>,
    bytes: usize,
    next_event: u64,
    next_packet: u64,
    orders: HashMap<Order, u64>,
    frame: String,
    interactive_turns: u8,
}
impl OutputQueue {
    pub(crate) fn available_slots(&self) -> usize {
        256usize.saturating_sub(self.packets.len())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }
    pub(crate) fn discard_obsolete_search(&mut self, current: u64) -> io::Result<()> {
        // A partially transmitted message must finish reassembly. Unstarted
        // batches can be dropped as soon as a newer query or Stop arrives.
        self.packets.retain(|packet| !packet.search || packet.offset != 0
            || !packet.event || packet.order == Some(Order::Request(current)));
        for packet in &mut self.packets {
            if packet.search && packet.offset == 0 && packet.order != Some(Order::Request(current)) {
                packet.data = encode(&Response { id: packet.id,
                    result: Err(io::Error::from(io::ErrorKind::Interrupted).into()) })?;
                packet.search = false;
            }
        }
        self.bytes = self.packets.iter().map(|packet| packet.data.len()).sum();
        Ok(())
    }
    pub(crate) fn push(&mut self, response: Response) -> io::Result<()> {
        if self.available_slots() == 0 {
            return Err(io::Error::other("Remote output queue limit exceeded"));
        }
        // Search progress and its terminal reply share an ordering barrier.
        // A small final reply must not overtake a partially written batch.
        let order = if response.id != 0 { Some(Order::Request(response.id)) } else {
            match &response.result {
                Ok(Reply::SearchProgress { request, .. }) => Some(Order::Request(*request)),
                // Larger process chunks can span frames. A short final chunk
                // or EOF must never overtake older output from the same PTY.
                Ok(Reply::ProcessData { process, .. } | Reply::ProcessEnd { process, .. }) => Some(Order::Process(*process)),
                _ => None,
            }
        };
        let interactive = matches!(response.result, Ok(Reply::ProcessData { .. }
            | Reply::ProcessEnd { .. } | Reply::ProcessInputConsumed { .. }));
        let search = matches!(response.result, Ok(Reply::SearchProgress { .. } | Reply::Search { .. }));
        let mut response_id = response.id;
        let failure_id = match order { Some(Order::Request(id)) => id, _ => response.id };
        let mut data = encode(&response).or_else(|error| {
            response_id = failure_id;
            encode(&Response {
                id: response_id,
                result: Err(error.into()),
            })
        })?;
        if data.len() > (MAX_MESSAGE * 2).saturating_sub(self.bytes) {
            response_id = failure_id;
            data = encode(&Response {
                id: response_id,
                result: Err(
                    "Remote transfer queue is full; retry after current transfers finish".into(),
                ),
            })?;
        }
        self.bytes += data.len();
        let event = response_id == 0;
        let id = if event {
            self.next_event = self.next_event.wrapping_add(1) & !EVENT_ID;
            EVENT_ID | self.next_event
        } else {
            response_id
        };
        self.packets.push_back(Packet {
            id,
            data,
            offset: 0,
            event,
            interactive,
            search,
            order,
            serial: self.next_packet,
        });
        self.next_packet = self.next_packet.wrapping_add(1);
        Ok(())
    }
    pub(crate) fn step(&mut self, writer: &mut impl Write, session: u64) -> io::Result<()> {
        let begun = self.packets.iter().filter(|p| p.offset != 0).count();
        self.orders.clear();
        for packet in &self.packets {
            if let Some(order) = packet.order {
                self.orders.entry(order).and_modify(|first| *first = (*first).min(packet.serial)).or_insert(packet.serial);
            }
        }
        let ready = |packet: &Packet| packet.order.is_none_or(|order| self.orders.get(&order) == Some(&packet.serial));
        let can_start = |p: &Packet| p.offset != 0 || begun < MAX_PENDING - 1 || p.data.len() <= CHUNK;
        let interactive = self
            .packets
            .iter()
            .position(|p| ready(p) && p.interactive && p.data.len() <= CHUNK * 4)
            .or_else(|| self.packets.iter().position(|p| ready(p) && p.interactive && can_start(p)));
        let ordinary = self.packets.iter()
            .position(|p| ready(p) && !p.interactive && p.event && p.data.len() <= CHUNK * 4)
            .or_else(|| {
                self.packets
                    .iter()
                    .position(|p| ready(p) && !p.interactive && can_start(p))
            });
        // Favor echo without letting a continuously drained, noisy terminal
        // starve file replies or workspace updates indefinitely.
        let index = if self.interactive_turns < 8 {
            interactive.or(ordinary)
        } else {
            ordinary.or(interactive)
        }.unwrap();
        let mut packet = self.packets.remove(index).unwrap();
        self.interactive_turns = if packet.interactive { self.interactive_turns.saturating_add(1) } else { 0 };
        // Small process events finish promptly. Larger notifications (such as
        // a history page) have unique transfer IDs and rotate with file replies.
        loop {
            let end = (packet.offset + CHUNK).min(packet.data.len());
            write_frame(
                writer,
                'S',
                session,
                packet.id,
                packet.offset / CHUNK,
                &packet.data[packet.offset..end],
                end == packet.data.len(),
                &mut self.frame,
            )?;
            packet.offset = end;
            if packet.offset == packet.data.len() {
                self.bytes -= packet.data.len();
                break;
            }
            if !packet.event || packet.data.len() > CHUNK * 4 {
                self.packets.push_back(packet);
                break;
            }
        }
        writer.flush()
    }
}

pub(crate) struct Decoder {
    prefix: String,
    pending: HashMap<u64, (usize, Vec<u8>)>,
    bytes: usize,
    chunk: [u8; CHUNK + 3],
}
impl Decoder {
    pub(crate) fn in_progress(&self, id: u64) -> bool {
        self.pending.contains_key(&id)
    }
    pub(crate) fn new(direction: char, session: u64) -> Self {
        Self {
            prefix: format!("{PREFIX}{direction}:{session:016x}:"),
            pending: HashMap::new(),
            bytes: 0,
            chunk: [0; CHUNK + 3],
        }
    }
    /// Non-protocol terminal output and the opposite direction (PTY echo) are
    /// ignored. Malformed packets for this connection fail closed.
    #[inline]
    pub(crate) fn line(&mut self, line: &[u8]) -> io::Result<Option<(u64, Vec<u8>)>> {
        self.line_with_output(line, |_| {})
    }
    /// The observer runs only on the existing non-protocol branch. Valid,
    /// fragmented, echoed and malformed protocol records never reach it.
    pub(crate) fn line_with_output(&mut self, line: &[u8], mut output: impl FnMut(&[u8])) -> io::Result<Option<(u64, Vec<u8>)>> {
        if line.len() > MAX_LINE {
            return Err(io::Error::other("Remote frame exceeds limit"));
        }
        let record = record(line);
        let Ok(text) = std::str::from_utf8(record) else {
            if !record.starts_with(b"VTCR") { output(line); }
            return Ok(None);
        };
        let Some(body) = text
            .trim_end_matches(['\r', '\n'])
            .strip_prefix(&self.prefix)
        else {
            if !record.starts_with(b"VTCR") { output(line); }
            return Ok(None);
        };
        let invalid = || io::Error::other("Invalid remote frame sequence");
        let mut parts = body.splitn(4, ':');
        let id =
            u64::from_str_radix(parts.next().ok_or_else(invalid)?, 16).map_err(|_| invalid())?;
        let sequence =
            usize::from_str_radix(parts.next().ok_or_else(invalid)?, 16).map_err(|_| invalid())?;
        let last = parts.next().ok_or_else(invalid)?;
        if last != "0" && last != "1" {
            return Err(invalid());
        }
        let encoded = parts.next().ok_or_else(invalid)?;
        if encoded.is_empty() || encoded.len() > CHUNK.div_ceil(3) * 4 {
            return Err(invalid());
        }
        if sequence == 0 {
            if self.pending.contains_key(&id) {
                return Err(invalid());
            }
            let data = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
            if data.is_empty() || data.len() > CHUNK || (last == "0" && data.len() != CHUNK) {
                return Err(invalid());
            }
            // Most requests/replies finish in one frame. They need neither a
            // hash-table insertion nor a fragmented-message allocation/window.
            if last == "1" {
                return Ok(Some((id, decode(data)?)));
            }
            if self.pending.len() >= MAX_PENDING {
                return Err(io::Error::other("Too many remote transfers"));
            }
            if self.bytes + data.len() > MAX_MESSAGE * 2 {
                return Err(invalid());
            }
            self.bytes += data.len();
            self.pending.insert(id, (1, data));
            return Ok(None);
        }
        let (expected, data) = self.pending.get_mut(&id).ok_or_else(invalid)?;
        if sequence != *expected {
            return Err(invalid());
        }
        let previous = data.len();
        let decoded_len = encoded.len() / 4 * 3 + encoded.len() % 4 * 6 / 8;
        if previous + decoded_len > MAX_MESSAGE + 1 || self.bytes + decoded_len > MAX_MESSAGE * 2 {
            return Err(invalid());
        }
        // Reuse bounded scratch space. Vec-based decoding would resize/clear
        // the assembly tail for every frame; a fresh Vec also allocates for
        // every frame. The extra bytes cover base64's conservative estimate.
        let count = URL_SAFE_NO_PAD
            .decode_slice(encoded, &mut self.chunk)
            .map_err(|_| invalid())?;
        if count == 0
            || count > CHUNK
            || (last == "0" && count != CHUNK)
            || previous + count > MAX_MESSAGE + 1
            || self.bytes + count > MAX_MESSAGE * 2
        {
            return Err(invalid());
        }
        data.extend_from_slice(&self.chunk[..count]);
        self.bytes += count;
        *expected += 1;
        if last == "0" {
            return Ok(None);
        }
        let (_, data) = self.pending.remove(&id).unwrap();
        self.bytes -= data.len();
        Ok(Some((id, decode(data)?)))
    }
}

/// read_until alone can allocate without bound on a corrupt peer or shell
/// output. Limit every line before extending the retained buffer.
pub(crate) fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<bool> {
    line.clear();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Truncated remote frame",
                ))
            };
        }
        let end = memchr::memchr(b'\n', buffer).map(|p| p + 1);
        let len = end.unwrap_or(buffer.len());
        if line.len() + len > MAX_LINE {
            return Err(io::Error::other("Remote frame exceeds limit"));
        }
        line.extend_from_slice(&buffer[..len]);
        reader.consume(len);
        if end.is_some() {
            return Ok(true);
        }
    }
}
