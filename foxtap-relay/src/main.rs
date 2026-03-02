use cpal::traits::{DeviceTrait, StreamTrait};
use foxtap_common::{ShmHeader, MAGIC, RING_FRAMES, MAX_CHANNELS};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

struct ShmPtr {
    #[cfg(target_os = "windows")]
    mapping: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(target_os = "windows")]
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    header: *mut ShmHeader,
    ring: *const f32,
}
unsafe impl Send for ShmPtr {}
unsafe impl Sync for ShmPtr {}

/// Single-instance lock using a named file mapping.
/// If we can create a new mapping with our sentinel name, we're the only instance.
/// If OpenFileMapping succeeds first, another instance owns it.
#[cfg(target_os = "windows")]
struct RelayLock {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
impl RelayLock {
    fn try_acquire() -> Option<Self> {
        use windows_sys::Win32::Foundation::*;
        use windows_sys::Win32::System::Memory::*;

        unsafe {
            let name: Vec<u16> = "FoxTapRelayRunning\0"
                .encode_utf16()
                .collect();

            // Create/open the lock mapping atomically and check creation result.
            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                64,
                name.as_ptr(),
            );
            if handle.is_null() {
                return None;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return None;
            }
            Some(RelayLock { handle })
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for RelayLock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for ShmPtr {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::UnmapViewOfFile;

        unsafe {
            if !self.view.Value.is_null() {
                UnmapViewOfFile(self.view);
                self.view.Value = std::ptr::null_mut();
            }
            if !self.mapping.is_null() {
                CloseHandle(self.mapping);
                self.mapping = std::ptr::null_mut();
            }
        }
    }
}

fn main() {
    let silent = std::env::args().any(|a| a == "--silent");

    if !silent {
        println!("FoxTap Relay v1.0.0");
    }

    // Single instance check
    #[cfg(target_os = "windows")]
    let _lock = match RelayLock::try_acquire() {
        Some(m) => m,
        None => {
            if !silent {
                eprintln!("Another FoxTap Relay is already running.");
            }
            return;
        }
    };

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })
        .expect("Failed to set Ctrl+C handler");
    }

    if !silent {
        println!("  Waiting for FoxTap plugin...");
    }

    // Wait for shared memory to appear
    let shm = loop {
        if !running.load(Ordering::Relaxed) { return; }
        if let Some(s) = open_shm() {
            break s;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };

    // Wait for plugin to initialize (magic number)
    loop {
        if !running.load(Ordering::Relaxed) { return; }
        let magic = unsafe { (*shm.header).magic.load(Ordering::Acquire) };
        if magic == MAGIC { break; }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    let sample_rate = unsafe { (*shm.header).sample_rate.load(Ordering::Acquire) };
    let channels = unsafe { (*shm.header).channels.load(Ordering::Acquire) as usize };

    if !silent {
        println!("  Connected! {}Hz, {} ch", sample_rate, channels);
    }

    let shm = Arc::new(shm);
    let buffer_frames: u64 = 4096;

    let (stream, read_pos, peak, initial_write) = loop {
        if !running.load(Ordering::Relaxed) {
            return;
        }

        let device = match find_vb_cable() {
            Some(d) => d,
            None => {
                if !silent {
                    eprintln!("VB-Cable not found. Retrying...");
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };

        if !silent {
            if let Ok(name) = device.name() {
                println!("  Output: {}", name);
            }
        }

        let supported = match device.default_output_config() {
            Ok(s) => {
                if !silent {
                    println!(
                        "  Device config: {}Hz, {} ch, {:?}",
                        s.sample_rate().0,
                        s.channels(),
                        s.sample_format()
                    );
                }
                s
            }
            Err(e) => {
                if !silent {
                    eprintln!("Could not get device config: {} (retrying)", e);
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };

        let out_channels = supported.channels() as usize;
        let config: cpal::StreamConfig = supported.into();

        // Local read position — start behind write_pos to prevent underruns
        let initial_write = unsafe { (*shm.header).write_pos.load(Ordering::Acquire) };
        let read_pos = Arc::new(AtomicU64::new(initial_write.saturating_sub(buffer_frames)));
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let shm_cb = shm.clone();
        let rp_cb = read_pos.clone();
        let peak_cb = peak.clone();

        if !silent {
            println!("  Building stream...");
        }

        let stream = match device.build_output_stream(
            &config,
            move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let header = unsafe { &*shm_cb.header };
                let ring = shm_cb.ring;
                let w = header.write_pos.load(Ordering::Acquire);
                let mut r = rp_cb.load(Ordering::Relaxed);
                let cap = RING_FRAMES as u64;
                let frames_needed = output.len() / out_channels.max(1);

                // Consumer has fallen behind by more than ring capacity:
                // drop oldest frames and resync to the newest valid window.
                if w.saturating_sub(r) > cap {
                    r = w.saturating_sub(cap);
                }

                let avail = w.saturating_sub(r) as usize;
                let to_read = frames_needed.min(avail);

                for i in 0..to_read {
                    let frame_start = i * out_channels;
                    for ch in 0..out_channels {
                        output[frame_start + ch] = 0.0;
                    }

                    let idx = (r % cap) as usize * MAX_CHANNELS;
                    for ch in 0..out_channels.min(MAX_CHANNELS) {
                        output[frame_start + ch] = unsafe { *ring.add(idx + ch) };
                    }
                    r += 1;
                }

                // Silence remainder
                let written = to_read * out_channels;
                for s in &mut output[written..] {
                    *s = 0.0;
                }

                rp_cb.store(r, Ordering::Relaxed);
                header.read_pos.store(r, Ordering::Release);

                // Peak tracking
                let mut p = 0.0f32;
                for s in &output[..written] {
                    p = p.max(s.abs());
                }
                if p > f32::from_bits(peak_cb.load(Ordering::Relaxed)) {
                    peak_cb.store(p.to_bits(), Ordering::Relaxed);
                }
            },
            |err| eprintln!("Audio error: {}", err),
            None,
        ) {
            Ok(stream) => stream,
            Err(e) => {
                if !silent {
                    eprintln!("Failed to build stream: {} (retrying)", e);
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };

        if let Err(e) = stream.play() {
            if !silent {
                eprintln!("Failed to start playback: {} (retrying)", e);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            continue;
        }

        break (stream, read_pos, peak, initial_write);
    };

    if !silent {
        println!("  Starting playback...");
        println!("  Live! Streamlabs can capture CABLE Output.");
        println!("  Press Ctrl+C to stop.\n");
    }

    // Stale detection: if plugin magic is cleared, exit cleanly.
    // In manual mode, also exit after prolonged no-write activity.
    // In silent auto-launch mode, keep running so startup-idle periods
    // don't kill the relay before the user starts playback.
    let mut stale_counter = 0u32;
    let mut last_write_pos = initial_write;

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_secs(1));

        let header = unsafe { &*shm.header };

        // Check if plugin cleared magic (deactivate was called)
        let magic = header.magic.load(Ordering::Acquire);
        if magic != MAGIC {
            if !silent {
                println!("  Plugin disconnected (magic cleared). Exiting.");
            }
            break;
        }

        // Check for stale write position
        let w = header.write_pos.load(Ordering::Relaxed);
        if w == last_write_pos {
            stale_counter += 1;
            if !silent && stale_counter >= 5 {
                if !silent {
                    println!("  Plugin appears inactive (no writes for 5s). Exiting.");
                }
                break;
            }
        } else {
            stale_counter = 0;
            last_write_pos = w;
        }

        if !silent {
            let p = f32::from_bits(peak.load(Ordering::Relaxed));
            let r = read_pos.load(Ordering::Relaxed);
            println!("  [status] peak={:.4} buf={} write={} read={}", p, w.saturating_sub(r), w, r);
            peak.store(0, Ordering::Relaxed);
        }
    }

    drop(stream);
    if !silent {
        println!("FoxTap Relay stopped.");
    }
}

#[cfg(target_os = "windows")]
fn open_shm() -> Option<ShmPtr> {
    use foxtap_common::{ShmLayout, SHM_NAME};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Memory::*;

    unsafe {
        let name: Vec<u16> = SHM_NAME.encode_utf16().chain(std::iter::once(0)).collect();

        let mapping = OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name.as_ptr());
        if mapping.is_null() {
            return None;
        }

        let ptr = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        if ptr.Value.is_null() {
            CloseHandle(mapping);
            return None;
        }

        let base = ptr.Value;
        let header = base as *mut ShmHeader;
        let ring = (base as *const u8).add(std::mem::offset_of!(ShmLayout, ring)) as *const f32;

        Some(ShmPtr {
            mapping,
            view: ptr,
            header,
            ring,
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn open_shm() -> Option<ShmPtr> {
    None
}

#[cfg(target_os = "windows")]
fn find_vb_cable() -> Option<cpal::Device> {
    use cpal::traits::HostTrait;
    let host = cpal::default_host();
    for device in host.output_devices().ok()? {
        if let Ok(name) = device.name() {
            if name.to_lowercase().contains("cable input") {
                return Some(device);
            }
        }
    }
    None
}

#[cfg(not(target_os = "windows"))]
fn find_vb_cable() -> Option<cpal::Device> {
    None
}
