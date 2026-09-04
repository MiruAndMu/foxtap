//! FoxTap shared memory protocol.
//!
//! Lock-free SPSC ring buffer over shared memory for real-time audio IPC.
//! Producer: VST3 plugin. Consumer: relay app.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Shared memory mapping name (Windows CreateFileMappingW).
pub const SHM_NAME: &str = "FoxTapAudioBridge";

/// Ring buffer capacity in frames (stereo f32 pairs).
/// ~2 seconds at 48kHz = 96000 frames.
pub const RING_FRAMES: usize = 96000;

/// Maximum supported channels.
pub const MAX_CHANNELS: usize = 2;

/// Total ring buffer size in f32 samples.
pub const RING_SAMPLES: usize = RING_FRAMES * MAX_CHANNELS;

/// Magic value to verify shared memory is initialized.
pub const MAGIC: u32 = 0xF017_A900;

/// Header at the start of shared memory.
/// All fields use atomics for lock-free access.
#[repr(C)]
pub struct ShmHeader {
    /// Magic number — set to MAGIC when initialized.
    pub magic: AtomicU32,
    /// Sample rate reported by the DAW (e.g. 44100, 48000).
    pub sample_rate: AtomicU32,
    /// Number of channels (1 or 2).
    pub channels: AtomicU32,
    /// Write position in frames (monotonically increasing, wraps via modulo).
    pub write_pos: AtomicU64,
    /// Read position in frames (monotonically increasing, wraps via modulo).
    pub read_pos: AtomicU64,
    /// Padding to cache line boundary.
    pub _pad: [u8; 24],
}

/// Full shared memory layout: header + ring buffer.
#[repr(C)]
pub struct ShmLayout {
    pub header: ShmHeader,
    pub ring: [f32; RING_SAMPLES],
}

impl ShmLayout {
    /// Total size in bytes for the shared memory mapping.
    pub const fn size_bytes() -> usize {
        std::mem::size_of::<Self>()
    }
}

/// How many frames are available to read.
pub fn available_frames(header: &ShmHeader) -> u64 {
    let w = header.write_pos.load(Ordering::Acquire);
    let r = header.read_pos.load(Ordering::Acquire);
    w.saturating_sub(r)
}

/// Write interleaved audio frames into the ring buffer.
///
/// Called by the plugin in `process()`. If the consumer is too slow,
/// old data is silently overwritten (we never block the audio thread).
///
/// # Safety
/// `ring` must point to a valid `[f32; RING_SAMPLES]` in shared memory.
pub unsafe fn write_frames(
    header: &ShmHeader,
    ring: *mut f32,
    channels: usize,
    data: &[&[f32]],
    num_frames: usize,
) {
    let cap = RING_FRAMES as u64;
    let mut pos = header.write_pos.load(Ordering::Relaxed);

    for i in 0..num_frames {
        let idx = (pos % cap) as usize * MAX_CHANNELS;
        for ch in 0..channels.min(MAX_CHANNELS) {
            *ring.add(idx + ch) = data[ch][i];
        }
        // Zero any unused channels in the interleaved slot
        for ch in channels..MAX_CHANNELS {
            *ring.add(idx + ch) = 0.0;
        }
        pos += 1;
    }

    header.write_pos.store(pos, Ordering::Release);
}

/// Write interleaved audio frames into the ring buffer while applying a
/// per-sample gain.
///
/// `gain` must have at least `num_frames` entries — one multiplier per frame,
/// e.g. from a smoothed parameter's `Smoother::next_block`. This is intended
/// for real-time audio code paths where allocating temporary gain buffers
/// would be undesirable, so the gain slice is caller-owned.
///
/// # Safety
/// `ring` must point to a valid `[f32; RING_SAMPLES]` in shared memory.
pub unsafe fn write_frames_with_gain(
    header: &ShmHeader,
    ring: *mut f32,
    channels: usize,
    data: &[&[f32]],
    num_frames: usize,
    gain: &[f32],
) {
    let cap = RING_FRAMES as u64;
    let mut pos = header.write_pos.load(Ordering::Relaxed);

    for i in 0..num_frames {
        let idx = (pos % cap) as usize * MAX_CHANNELS;
        let g = gain[i];
        for ch in 0..channels.min(MAX_CHANNELS) {
            *ring.add(idx + ch) = data[ch][i] * g;
        }
        // Zero any unused channels in the interleaved slot
        for ch in channels..MAX_CHANNELS {
            *ring.add(idx + ch) = 0.0;
        }
        pos += 1;
    }

    header.write_pos.store(pos, Ordering::Release);
}

/// Read interleaved audio frames from the ring buffer into a flat buffer.
///
/// Returns the number of frames actually read (may be less than requested
/// if not enough data is available). Advances read_pos.
///
/// # Safety
/// `ring` must point to a valid `[f32; RING_SAMPLES]` in shared memory.
pub unsafe fn read_frames(
    header: &ShmHeader,
    ring: *const f32,
    out: &mut [f32],
    max_frames: usize,
    channels: usize,
) -> usize {
    let avail = available_frames(header) as usize;
    let frames = avail.min(max_frames);
    let cap = RING_FRAMES as u64;
    let mut pos = header.read_pos.load(Ordering::Relaxed);

    for i in 0..frames {
        let idx = (pos % cap) as usize * MAX_CHANNELS;
        for ch in 0..channels.min(MAX_CHANNELS) {
            out[i * channels + ch] = *ring.add(idx + ch);
        }
        pos += 1;
    }

    header.read_pos.store(pos, Ordering::Release);
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_header() -> ShmHeader {
        ShmHeader {
            magic: AtomicU32::new(MAGIC),
            sample_rate: AtomicU32::new(48_000),
            channels: AtomicU32::new(MAX_CHANNELS as u32),
            write_pos: AtomicU64::new(0),
            read_pos: AtomicU64::new(0),
            _pad: [0; 24],
        }
    }

    #[test]
    fn layout_size_is_reasonable() {
        // Header (~64 bytes) + ring (96000 * 2 * 4 = 768000 bytes) ≈ 768KB
        let size = ShmLayout::size_bytes();
        assert!(size > 700_000);
        assert!(size < 1_000_000);
    }

    #[test]
    fn write_frames_with_gain_scales_samples() {
        let header = test_header();
        let mut ring = vec![0.0f32; RING_SAMPLES];
        let left = [1.0f32, -0.5f32];
        let right = [0.25f32, 0.5f32];
        let data: [&[f32]; 2] = [&left, &right];

        unsafe {
            write_frames_with_gain(&header, ring.as_mut_ptr(), 2, &data, 2, &[0.5, 0.5]);
        }

        assert_eq!(ring[0], 0.5);
        assert_eq!(ring[1], 0.125);
        assert_eq!(ring[2], -0.25);
        assert_eq!(ring[3], 0.25);
        assert_eq!(header.write_pos.load(Ordering::Acquire), 2);
    }

    #[test]
    fn write_frames_with_gain_zeroes_unused_channels() {
        let header = test_header();
        let mut ring = vec![0.0f32; RING_SAMPLES];
        let mono = [0.75f32];
        let data: [&[f32]; 1] = [&mono];

        unsafe {
            write_frames_with_gain(&header, ring.as_mut_ptr(), 1, &data, 1, &[1.0]);
        }

        assert_eq!(ring[0], 0.75);
        assert_eq!(ring[1], 0.0);
        assert_eq!(header.write_pos.load(Ordering::Acquire), 1);
    }
}
