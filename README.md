# FoxTap

**Zero-latency audio tap for streaming.** FoxTap is a VST3 plugin that captures audio from your DAW and routes it to your streaming software (Streamlabs, OBS) through VB-Cable — with no added latency. Drop it on your master bus, and your stream hears exactly what you hear.

## Features

- **Zero latency** — lock-free shared memory ring buffer, no extra audio processing delay
- **Auto-launching relay** — the plugin starts its companion relay process automatically
- **Independent volume control** — adjust what the stream hears without affecting your monitors
- **Pixel art GUI** — because we care about the little things
- **Transparent pass-through** — your DAW audio is never modified

## Requirements

- **Windows** (uses Windows shared memory APIs)
- **[VB-Cable](https://vb-audio.com/Cable/)** (free virtual audio device)
- **VST3-compatible DAW** (FL Studio, Ableton, Reaper, etc.)

## Installation

### Option A: Installer (recommended)

Download `FoxTap-v1.0-setup.exe` from [Releases](https://github.com/MiruAndMu/foxtap/releases) and run it. The installer places the VST3 plugin and relay in the right locations and checks for VB-Cable.

### Option B: Build from source

See [Building](#building) below.

## Usage

1. **Install VB-Cable** if you haven't already — [vb-audio.com/Cable](https://vb-audio.com/Cable/)
2. **Load FoxTap** on your DAW's master bus (or any channel you want to stream)
3. **In Streamlabs/OBS**, add an Audio Input Capture source set to **CABLE Output**

That's it. The relay launches automatically when the plugin initializes.

### Controls

- **Enabled** — toggle audio capture on/off
- **Stream Volume** — adjust the level sent to your stream (0–100%) without affecting your DAW monitors

## Building

Requires [Rust](https://rustup.rs/) (nightly recommended for nih-plug).

```bash
# Clone
git clone https://github.com/MiruAndMu/foxtap.git
cd foxtap

# Build the VST3 plugin bundle
cargo xtask bundle foxtap-plugin --release

# Build the relay
cargo build --release -p foxtap-relay
```

Output:
- Plugin: `target/bundled/foxtap-plugin.vst3/`
- Relay: `target/release/foxtap-relay.exe`

Copy the `.vst3` folder to your VST3 directory (typically `C:\Program Files\Common Files\VST3\`) and place `foxtap-relay.exe` either next to the plugin DLL or in `C:\Program Files\Miru & Mu\FoxTap\`.

### Building the installer

Requires [Inno Setup](https://jrsoftware.org/isinfo.php). After building the plugin and relay:

```bash
iscc installer/foxtap.iss
```

Output: `target/installer/FoxTap-v1.0-setup.exe`

## How It Works

FoxTap uses a three-part architecture:

1. **foxtap-common** — shared memory protocol (lock-free SPSC ring buffer)
2. **foxtap-plugin** — VST3 plugin that writes audio to shared memory
3. **foxtap-relay** — standalone process that reads shared memory and outputs to VB-Cable

The plugin writes audio frames into a ~768KB shared memory ring buffer. The relay reads from that buffer and plays it through VB-Cable's virtual input. Your streaming software captures VB-Cable's output. No network, no pipes, no extra latency.

## License

[MIT](LICENSE)

---

*Fox designed, fox approved.* 🦊

**[Miru & Mu](https://github.com/MiruAndMu)**
