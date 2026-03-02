mod editor;

use foxtap_common::{ShmHeader, MAX_CHANNELS};
#[cfg(target_os = "windows")]
use foxtap_common::{ShmLayout, MAGIC, SHM_NAME};
use nih_plug::prelude::*;
use nih_plug_vizia::ViziaState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared memory handle — Windows-only at runtime, stubbed on other platforms.
struct ShmHandle {
    #[cfg(target_os = "windows")]
    mapping: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(target_os = "windows")]
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
    header: *mut ShmHeader,
    ring: *mut f32,
}

// SAFETY: The shared memory is only written to by one producer (this plugin)
// and read by one consumer (the relay). No concurrent mutable access to the same data.
unsafe impl Send for ShmHandle {}
unsafe impl Sync for ShmHandle {}

impl ShmHandle {
    #[cfg(target_os = "windows")]
    fn open() -> Option<Self> {
        use windows_sys::Win32::Foundation::*;
        use windows_sys::Win32::System::Memory::*;

        unsafe {
            let name: Vec<u16> = SHM_NAME.encode_utf16().chain(std::iter::once(0)).collect();
            let size = ShmLayout::size_bytes() as u32;

            let mapping = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                size,
                name.as_ptr(),
            );
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
            let ring = (base as *mut u8).add(std::mem::offset_of!(ShmLayout, ring)) as *mut f32;

            // Initialize header
            (*header).magic.store(MAGIC, Ordering::Release);
            (*header).write_pos.store(0, Ordering::Release);
            (*header).read_pos.store(0, Ordering::Release);

            Some(ShmHandle {
                mapping,
                view: ptr,
                header,
                ring,
            })
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn open() -> Option<Self> {
        None
    }
}

#[cfg(target_os = "windows")]
impl Drop for ShmHandle {
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

/// Check if the relay process is already running by trying to open its named lock.
#[cfg(target_os = "windows")]
fn is_relay_running() -> bool {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Memory::*;

    unsafe {
        let name: Vec<u16> = "FoxTapRelayRunning\0"
            .encode_utf16()
            .collect();
        let handle = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if !handle.is_null() {
            CloseHandle(handle);
            true
        } else {
            false
        }
    }
}

#[cfg(target_os = "windows")]
fn wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn sanitize_candidate_path(path: &str) -> String {
    path.trim().trim_matches('"').to_string()
}

/// Resolve the current plugin DLL's directory using this function's address.
#[cfg(target_os = "windows")]
fn current_dll_dir() -> Option<String> {
    use windows_sys::Win32::Foundation::HMODULE;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleExW};

    unsafe {
        let mut h_module: HMODULE = std::ptr::null_mut();
        // Pass address of this function; Windows resolves which module owns it.
        let this_fn_addr = launch_relay as *const () as *const u16;
        const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x00000004;
        const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x00000002;
        let flags =
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;

        if GetModuleHandleExW(flags, this_fn_addr, &mut h_module) == 0 || h_module.is_null() {
            return None;
        }

        let mut path = [0u16; 1024];
        let len = GetModuleFileNameW(h_module, path.as_mut_ptr(), path.len() as u32);
        if len == 0 {
            return None;
        }

        let full_path = String::from_utf16_lossy(&path[..len as usize]);
        full_path.rfind('\\').map(|pos| full_path[..pos].to_string())
    }
}

/// Read installer-written relay path from registry.
#[cfg(target_os = "windows")]
fn relay_path_from_registry() -> Option<String> {
    use windows_sys::Win32::System::Registry::{
        RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    fn read_value(root: HKEY) -> Option<String> {
        let subkey = wide_null("SOFTWARE\\MiruAndMu\\FoxTap");
        let value_name = wide_null("RelayPath");

        unsafe {
            let mut value_type = 0u32;
            let mut bytes = 0u32;
            if RegGetValueW(
                root,
                subkey.as_ptr(),
                value_name.as_ptr(),
                RRF_RT_REG_SZ,
                &mut value_type,
                std::ptr::null_mut(),
                &mut bytes,
            ) != 0
                || bytes < 2
            {
                return None;
            }

            let mut utf16_buf = vec![0u16; (bytes as usize / 2).saturating_add(1)];
            if RegGetValueW(
                root,
                subkey.as_ptr(),
                value_name.as_ptr(),
                RRF_RT_REG_SZ,
                &mut value_type,
                utf16_buf.as_mut_ptr() as *mut _,
                &mut bytes,
            ) != 0
            {
                return None;
            }

            let len = utf16_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(utf16_buf.len());
            if len == 0 {
                None
            } else {
                Some(String::from_utf16_lossy(&utf16_buf[..len]))
            }
        }
    }

    read_value(HKEY_CURRENT_USER).or_else(|| read_value(HKEY_LOCAL_MACHINE))
}

#[cfg(target_os = "windows")]
fn try_launch_candidate(candidate: &str) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let app_name = wide_null(candidate);
    let mut cmd_line = wide_null(&format!("\"{}\" --silent", candidate));

    // Attempt 1: detached process (best UX when it works)
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = 0x00000001; // STARTF_USESHOWWINDOW
        si.wShowWindow = 0; // SW_HIDE

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            app_name.as_ptr(),
            cmd_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0x00000008 | 0x00000010, // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok != 0 {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
            return true;
        }
    }

    // Attempt 2: plain process creation (fallback for host-specific restrictions)
    let mut cmd_line_fallback = wide_null(&format!("\"{}\" --silent", candidate));
    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = 0x00000001; // STARTF_USESHOWWINDOW
        si.wShowWindow = 0; // SW_HIDE

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessW(
            app_name.as_ptr(),
            cmd_line_fallback.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok != 0 {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
            return true;
        }
    }

    // Attempt 3: shell-based launch fallback
    let operation = wide_null("open");
    let file = wide_null(candidate);
    let params = wide_null("--silent");
    unsafe {
        let result = ShellExecuteW(
            0 as _,
            operation.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        ) as isize;
        result > 32
    }
}

/// Try to launch the relay exe from known locations.
#[cfg(target_os = "windows")]
fn launch_relay() {
    if is_relay_running() {
        return;
    }

    let mut candidates = Vec::<String>::new();
    let mut push_unique = |candidate: String| {
        if !candidate.is_empty()
            && !candidates
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&candidate))
        {
            candidates.push(candidate);
        }
    };

    // 1) Preferred: installer-written registry path, including custom install directories.
    if let Some(path) = relay_path_from_registry() {
        push_unique(path);
    }

    // 2) DLL-relative locations.
    if let Some(dll_dir) = current_dll_dir() {
        let dll_dir_path = std::path::Path::new(&dll_dir);
        push_unique(dll_dir_path.join("foxtap-relay.exe").to_string_lossy().into_owned());

        // If plugin is loaded from ...\FoxTap.vst3\Contents\x86_64-win
        if let Some(contents_dir) = dll_dir_path.parent() {
            if let Some(bundle_dir) = contents_dir.parent() {
                push_unique(
                    bundle_dir
                        .join("Contents\\x86_64-win\\foxtap-relay.exe")
                        .to_string_lossy()
                        .into_owned(),
                );
                push_unique(
                    bundle_dir
                        .join("foxtap-relay.exe")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    // 3) Known default install locations.
    push_unique(
        "C:\\Program Files\\Common Files\\VST3\\FoxTap.vst3\\Contents\\x86_64-win\\foxtap-relay.exe"
            .to_string(),
    );
    push_unique("C:\\Program Files\\Miru & Mu\\FoxTap\\foxtap-relay.exe".to_string());

    for candidate in candidates {
        let sanitized = sanitize_candidate_path(&candidate);
        let path = std::path::Path::new(&sanitized);
        if !path.exists() {
            continue;
        }

        if try_launch_candidate(&sanitized) {
            return;
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn launch_relay() {}

#[cfg(target_os = "windows")]
fn start_relay_watchdog(stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if !is_relay_running() {
                launch_relay();
            }
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    });
}

/// Shared state the GUI can read to show relay activity.
pub(crate) struct FoxTapState {
    /// True when shared memory is active and relay appears connected.
    pub relay_active: AtomicBool,
}

impl Default for FoxTapState {
    fn default() -> Self {
        Self {
            relay_active: AtomicBool::new(false),
        }
    }
}

struct FoxTap {
    params: Arc<FoxTapParams>,
    shm: Option<ShmHandle>,
    state: Arc<FoxTapState>,
    #[cfg(target_os = "windows")]
    relay_watchdog_stop: Option<Arc<AtomicBool>>,
    /// Tracks last write_pos seen — used to detect relay activity.
    last_check_write_pos: u64,
    process_count: u64,
}

impl Default for FoxTap {
    fn default() -> Self {
        Self {
            params: Arc::new(FoxTapParams::default()),
            shm: None,
            state: Arc::new(FoxTapState::default()),
            #[cfg(target_os = "windows")]
            relay_watchdog_stop: None,
            last_check_write_pos: 0,
            process_count: 0,
        }
    }
}

#[derive(Params)]
pub(crate) struct FoxTapParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<ViziaState>,

    /// ON/OFF toggle — when OFF, stop writing to shared memory.
    #[id = "enabled"]
    pub enabled: BoolParam,

    /// Volume slider (0–100%) — controls level sent to stream.
    /// Does NOT affect monitoring (audio passes through unchanged).
    #[id = "stream_gain"]
    pub stream_gain: FloatParam,
}

impl Default for FoxTapParams {
    fn default() -> Self {
        Self {
            editor_state: editor::default_state(),
            enabled: BoolParam::new("Enabled", true),
            stream_gain: FloatParam::new(
                "Stream Volume",
                1.0,
                FloatRange::Linear { min: 0.0, max: 1.0 },
            )
            .with_unit(" %")
            .with_value_to_string(formatters::v2s_f32_percentage(0))
            .with_string_to_value(formatters::s2v_f32_percentage()),
        }
    }
}

impl Plugin for FoxTap {
    const NAME: &'static str = "FoxTap";
    const VENDOR: &'static str = "Miru & Mu";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        aux_input_ports: &[],
        aux_output_ports: &[],
        names: PortNames::const_default(),
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(
            self.params.clone(),
            self.state.clone(),
            self.params.editor_state.clone(),
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        self.shm = ShmHandle::open();
        if let Some(ref shm) = self.shm {
            unsafe {
                (*shm.header)
                    .sample_rate
                    .store(buffer_config.sample_rate as u32, Ordering::Release);
                (*shm.header)
                    .channels
                    .store(MAX_CHANNELS as u32, Ordering::Release);
            }
        }

        // Try to auto-launch the relay
        launch_relay();
        #[cfg(target_os = "windows")]
        {
            // Keep relay alive while the plugin instance is active.
            if let Some(stop) = self.relay_watchdog_stop.take() {
                stop.store(true, Ordering::Relaxed);
            }
            let stop = Arc::new(AtomicBool::new(false));
            start_relay_watchdog(stop.clone());
            self.relay_watchdog_stop = Some(stop);
        }

        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Only write to shared memory if enabled
        if self.params.enabled.value() {
            if let Some(ref shm) = self.shm {
                let num_frames = buffer.samples();
                let channels = buffer.channels();
                let gain = self.params.stream_gain.value();
                let slices = buffer.as_slice_immutable();
                let channel_count = channels.min(MAX_CHANNELS);

                // Build fixed-size channel refs on the stack to avoid allocations in process().
                let mut channel_refs: [&[f32]; MAX_CHANNELS] = [&[], &[]];
                for ch in 0..channel_count {
                    channel_refs[ch] = &slices[ch][..num_frames];
                }
                let channel_refs = &channel_refs[..channel_count];

                if (gain - 1.0).abs() < f32::EPSILON {
                    unsafe {
                        foxtap_common::write_frames(
                            &*shm.header,
                            shm.ring,
                            channels,
                            channel_refs,
                            num_frames,
                        );
                    }
                } else {
                    unsafe {
                        foxtap_common::write_frames_with_gain(
                            &*shm.header,
                            shm.ring,
                            channels,
                            channel_refs,
                            num_frames,
                            gain,
                        );
                    }
                }

                // Check relay activity periodically (~every 1024 process calls)
                self.process_count += 1;
                if self.process_count % 1024 == 0 {
                    let header = unsafe { &*shm.header };
                    let write_pos = header.write_pos.load(Ordering::Relaxed);
                    let read_pos = header.read_pos.load(Ordering::Relaxed);
                    // Relay is active if it's consuming frames (read_pos is advancing)
                    let active = read_pos > 0 && write_pos > read_pos;
                    self.state.relay_active.store(active, Ordering::Relaxed);
                    self.last_check_write_pos = write_pos;
                }
            }
        }

        // Audio passes through unchanged — we never modify the buffer.
        ProcessStatus::Normal
    }

    fn deactivate(&mut self) {
        // Clear magic so relay knows we're gone
        if let Some(ref shm) = self.shm {
            unsafe {
                (*shm.header).magic.store(0, Ordering::Release);
            }
        }
        self.shm = None;
        #[cfg(target_os = "windows")]
        if let Some(stop) = self.relay_watchdog_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        self.state.relay_active.store(false, Ordering::Relaxed);
    }
}

impl Vst3Plugin for FoxTap {
    const VST3_CLASS_ID: [u8; 16] = *b"FoxTapMiruAndMu!";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nih_export_vst3!(FoxTap);
