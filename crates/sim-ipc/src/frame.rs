//! Compact simulation frame. Little-endian, fixed header, then three `f32` columns.
//!
//! The desktop thread publishes one of these. The webview copies it into typed arrays.
//! Nothing in the hot path builds a JSON document.

pub const FRAME_MAGIC: u32 = 0x5453_4631;
pub const FRAME_VERSION: u32 = 1;
pub const FRAME_HEADER_BYTES: usize = 80;

const FLAG_PAUSED: u32 = 1;
const FLAG_SEEKING: u32 = 2;
const FLAG_UNCHANGED: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct FrameHeader {
    pub sequence: u64,
    pub time_s: f64,
    pub speed: f32,
    pub paused: bool,
    pub seeking: bool,
    pub mean_speed_mps: f32,
    pub vht_s: f64,
    pub vkt_m: f64,
    pub delay_s: f64,
    pub spawned: u32,
    pub active: u32,
    pub completed: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bottleneck {
    pub edge: u32,
    pub speed_ratio: f32,
    pub queue_veh: f32,
}

pub struct DecodedFrame<'a> {
    pub header: FrameHeader,
    /// The link columns are omitted because they match the previous frame.
    pub unchanged: bool,
    pub bottlenecks: Vec<Bottleneck>,
    pub speed_ratio: &'a [u8],
    pub density_ratio: &'a [u8],
    pub queue_veh: &'a [u8],
}

/// Highest queues first, then the lower speed ratio, then the lower edge index.
/// An edge with no stopped vehicle is not a bottleneck.
pub fn top_bottlenecks(speed_ratio: &[f32], queue_veh: &[f32], limit: usize) -> Vec<Bottleneck> {
    if limit == 0 {
        return Vec::new();
    }
    let mut best = Vec::new();
    for (index, (&ratio, &queued)) in speed_ratio.iter().zip(queue_veh.iter()).enumerate() {
        if queued <= 0.0 {
            continue;
        }
        best.push(Bottleneck {
            edge: index as u32,
            speed_ratio: ratio,
            queue_veh: queued,
        });
        best.sort_by(|left, right| {
            right
                .queue_veh
                .total_cmp(&left.queue_veh)
                .then_with(|| left.speed_ratio.total_cmp(&right.speed_ratio))
                .then_with(|| left.edge.cmp(&right.edge))
        });
        if best.len() > limit {
            best.pop();
        }
    }
    best
}

pub fn encode_frame(
    header: &FrameHeader,
    bottlenecks: &[Bottleneck],
    speed_ratio: &[f32],
    density_ratio: &[f32],
    queue_veh: &[f32],
    out: &mut Vec<u8>,
) {
    let edges = speed_ratio.len().min(density_ratio.len()).min(queue_veh.len());
    out.clear();
    out.reserve(FRAME_HEADER_BYTES + bottlenecks.len() * 12 + edges * 12);
    push_u32(out, FRAME_MAGIC);
    push_u32(out, FRAME_VERSION);
    push_u64(out, header.sequence);
    push_f64(out, header.time_s);
    push_f32(out, header.speed);
    let mut flags = 0u32;
    if header.paused {
        flags |= FLAG_PAUSED;
    }
    if header.seeking {
        flags |= FLAG_SEEKING;
    }
    push_u32(out, flags);
    push_f32(out, header.mean_speed_mps);
    push_f64(out, header.vht_s);
    push_f64(out, header.vkt_m);
    push_f64(out, header.delay_s);
    push_u32(out, header.spawned);
    push_u32(out, header.active);
    push_u32(out, header.completed);
    push_u32(out, edges as u32);
    push_u32(out, bottlenecks.len() as u32);
    for item in bottlenecks {
        push_u32(out, item.edge);
        push_f32(out, item.speed_ratio);
        push_f32(out, item.queue_veh);
    }
    push_f32_slice(out, &speed_ratio[..edges]);
    push_f32_slice(out, &density_ratio[..edges]);
    push_f32_slice(out, &queue_veh[..edges]);
}

/// Header only. The webview keeps the previous link columns.
pub fn encode_keepalive(header: &FrameHeader, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(FRAME_HEADER_BYTES);
    push_u32(out, FRAME_MAGIC);
    push_u32(out, FRAME_VERSION);
    push_u64(out, header.sequence);
    push_f64(out, header.time_s);
    push_f32(out, header.speed);
    let mut flags = FLAG_UNCHANGED;
    if header.paused {
        flags |= FLAG_PAUSED;
    }
    if header.seeking {
        flags |= FLAG_SEEKING;
    }
    push_u32(out, flags);
    push_f32(out, header.mean_speed_mps);
    push_f64(out, header.vht_s);
    push_f64(out, header.vkt_m);
    push_f64(out, header.delay_s);
    push_u32(out, header.spawned);
    push_u32(out, header.active);
    push_u32(out, header.completed);
    push_u32(out, 0);
    push_u32(out, 0);
}

pub fn decode_frame(bytes: &[u8]) -> Result<DecodedFrame<'_>, String> {
    if bytes.len() < FRAME_HEADER_BYTES {
        return Err("simulation frame is shorter than its header".into());
    }
    let magic = read_u32(bytes, 0);
    if magic != FRAME_MAGIC {
        return Err(format!("simulation frame magic {magic:#x} is not {FRAME_MAGIC:#x}"));
    }
    let version = read_u32(bytes, 4);
    if version != FRAME_VERSION {
        return Err(format!("simulation frame version {version} is not {FRAME_VERSION}"));
    }
    let flags = read_u32(bytes, 28);
    let unchanged = flags & FLAG_UNCHANGED != 0;
    let header = FrameHeader {
        sequence: read_u64(bytes, 8),
        time_s: read_f64(bytes, 16),
        speed: read_f32(bytes, 24),
        paused: flags & FLAG_PAUSED != 0,
        seeking: flags & FLAG_SEEKING != 0,
        mean_speed_mps: read_f32(bytes, 32),
        vht_s: read_f64(bytes, 36),
        vkt_m: read_f64(bytes, 44),
        delay_s: read_f64(bytes, 52),
        spawned: read_u32(bytes, 60),
        active: read_u32(bytes, 64),
        completed: read_u32(bytes, 68),
    };
    let edges = read_u32(bytes, 72) as usize;
    let listed = read_u32(bytes, 76) as usize;
    let table = FRAME_HEADER_BYTES + listed * 12;
    let columns = edges * 4;
    let need = table + columns * 3;
    if bytes.len() < need {
        return Err(format!("simulation frame needs {need} bytes and has {}", bytes.len()));
    }
    let mut bottlenecks = Vec::with_capacity(listed);
    for index in 0..listed {
        let at = FRAME_HEADER_BYTES + index * 12;
        bottlenecks.push(Bottleneck {
            edge: read_u32(bytes, at),
            speed_ratio: read_f32(bytes, at + 4),
            queue_veh: read_f32(bytes, at + 8),
        });
    }
    let speed_ratio = &bytes[table..table + columns];
    let density_ratio = &bytes[table + columns..table + columns * 2];
    let queue_veh = &bytes[table + columns * 2..table + columns * 3];
    Ok(DecodedFrame {
        header,
        unchanged,
        bottlenecks,
        speed_ratio,
        density_ratio,
        queue_veh,
    })
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_f64(out: &mut Vec<u8>, value: f64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_f32_slice(out: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        push_f32(out, *value);
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32"))
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64"))
}

fn read_f32(bytes: &[u8], offset: usize) -> f32 {
    f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("f32"))
}

fn read_f64(bytes: &[u8], offset: usize) -> f64 {
    f64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("f64"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_and_ranks_stopped_vehicles() {
        let speed = [1.0_f32, 0.4, 0.9];
        let density = [0.0_f32, 0.8, 0.2];
        let queue = [0.0_f32, 12.0, 3.0];
        let ranked = top_bottlenecks(&speed, &queue, 1);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].edge, 1);
        let header = FrameHeader {
            sequence: 7,
            time_s: 3661.5,
            speed: 20.0,
            paused: true,
            seeking: false,
            mean_speed_mps: 11.25,
            vht_s: 40.0,
            vkt_m: 1500.0,
            delay_s: 8.5,
            spawned: 12,
            active: 4,
            completed: 8,
        };
        let mut bytes = Vec::new();
        encode_frame(&header, &ranked, &speed, &density, &queue, &mut bytes);
        let decoded = decode_frame(&bytes).expect("frame");
        assert_eq!(decoded.header, header);
        assert_eq!(decoded.bottlenecks, ranked);
        assert_eq!(read_f32_at(decoded.speed_ratio, 1), 0.4);
        assert_eq!(read_f32_at(decoded.density_ratio, 1), 0.8);
        assert_eq!(read_f32_at(decoded.queue_veh, 2), 3.0);
        assert!(!decoded.unchanged);

        encode_keepalive(&header, &mut bytes);
        assert_eq!(bytes.len(), FRAME_HEADER_BYTES);
        let kept = decode_frame(&bytes).expect("keepalive");
        assert!(kept.unchanged);
        assert!(kept.header.paused);
        assert_eq!(kept.header.time_s, header.time_s);
        assert!(kept.bottlenecks.is_empty());
        assert!(kept.speed_ratio.is_empty());
    }

    fn read_f32_at(bytes: &[u8], index: usize) -> f32 {
        let offset = index * 4;
        f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}
