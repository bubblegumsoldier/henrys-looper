//! Device selection and stream configuration.
//!
//! Every subcommand takes the same set of device flags, so host lookup, device matching,
//! sample-rate/buffer validation and config building live here exactly once.

use std::str::FromStr;

use clap::Args;
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{
    BufferSize, Device, FromSample, Host, HostId, InputCallbackInfo, OutputCallbackInfo,
    SampleFormat, SizedSample, Stream, StreamConfig, SupportedBufferSize,
    SupportedStreamConfigRange,
};

// ---------------------------------------------------------------------------------------------
// Platform defaults
// ---------------------------------------------------------------------------------------------

/// The audio host a command uses when none is named. On Windows the CLI has always started on
/// WASAPI (ASIO needs a separate build); on macOS there is only CoreAudio, and it is the one that
/// reaches a usable latency there.
#[cfg(target_os = "macos")]
pub const DEFAULT_HOST: &str = "coreaudio";
#[cfg(target_os = "windows")]
pub const DEFAULT_HOST: &str = "wasapi";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const DEFAULT_HOST: &str = "alsa";

/// Roundtrip latency compensation, in frames, that is assumed until somebody measures it.
///
/// * Windows: 827 - **measured** in phase 0 with a loopback cable (Scarlett 2i2, ASIO, 48 kHz,
///   128 frames, standard deviation 0 over 60 runs).
/// * macOS: 596 - **not measured**. An estimate from what CoreAudio reports for the same
///   Scarlett 2i2 at 48 kHz / 128 frames: device latency, safety offset and I/O buffer on both
///   sides, plus the depth the input FIFO settles at with the stream layout the engine uses.
///   It was read off silently, without a loopback cable, and converter latency the driver does
///   not report is missing from it - so the real value is more likely higher than lower. Measure
///   it with `calibrate` before trusting a recording to the sample.
#[cfg(target_os = "macos")]
pub const DEFAULT_LATENCY_FRAMES: u64 = 596;
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_LATENCY_FRAMES: u64 = 827;

/// Whether [`DEFAULT_LATENCY_FRAMES`] is a measurement or an estimate. Every place that shows the
/// number says which of the two it is.
pub const DEFAULT_LATENCY_MEASURED: bool = cfg!(not(target_os = "macos"));

/// One sentence on where [`DEFAULT_LATENCY_FRAMES`] comes from, for help texts and the UI.
#[cfg(target_os = "macos")]
pub const DEFAULT_LATENCY_ORIGIN: &str = "Estimate from what CoreAudio reports for a Scarlett 2i2 \
     (48 kHz, 128 frames), not measured with a loopback cable - measure it with calibrate";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_LATENCY_ORIGIN: &str =
    "Measured in phase 0 (Scarlett 2i2 on ASIO, 48 kHz, 128 frames)";

/// Flags shared by every subcommand that opens an audio device.
#[derive(Args, Debug, Clone)]
pub struct DeviceOpts {
    /// Audio host: coreaudio (macOS), wasapi or asio (Windows; asio only when built with
    /// --features asio). Default is the system's usual host.
    #[arg(long, default_value = DEFAULT_HOST)]
    pub host: String,

    /// Part of the device name (case-insensitive); default device if omitted
    #[arg(long)]
    pub device: Option<String>,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 48_000)]
    pub rate: u32,

    /// Buffer size in frames
    #[arg(long, default_value_t = 128)]
    pub buffer: u32,

    /// Number of input channels (default: the device default)
    #[arg(long)]
    pub in_channels: Option<u16>,

    /// Number of output channels (default: the device default)
    #[arg(long)]
    pub out_channels: Option<u16>,

    /// Request the buffer size anyway, even if the device reports a different range.
    /// WASAPI in shared mode only reports the device period (usually 480 frames at 48 kHz);
    /// whether a smaller request really takes effect is part of the measurement. CoreAudio
    /// honestly reports 15..4096 and does not need this.
    #[arg(long)]
    pub force_buffer: bool,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Input => "input",
            Direction::Output => "output",
        }
    }
}

/// Everything needed to build one stream.
#[derive(Clone, Copy, Debug)]
pub struct StreamPlan {
    pub config: StreamConfig,
    pub format: SampleFormat,
    /// What the device claims it can do, for the record.
    pub reported_buffer: SupportedBufferSize,
}

impl StreamPlan {
    pub fn buffer_frames(&self) -> u32 {
        match self.config.buffer_size {
            BufferSize::Fixed(f) => f,
            BufferSize::Default => 0,
        }
    }
}

/// A resolved output-only setup.
pub struct OutputSetup {
    pub device: Device,
    pub plan: StreamPlan,
    pub name: String,
    pub host_name: String,
}

/// A resolved full-duplex setup (input and output on the selected host).
pub struct DuplexSetup {
    pub input: Device,
    pub in_plan: StreamPlan,
    pub in_name: String,
    pub output: Device,
    pub out_plan: StreamPlan,
    pub out_name: String,
    pub host_name: String,
}

impl DuplexSetup {
    pub fn sample_rate(&self) -> u32 {
        self.in_plan.config.sample_rate
    }

    pub fn buffer_frames(&self) -> u32 {
        self.in_plan.buffer_frames()
    }
}

/// Whether this binary was compiled with the ASIO backend.
///
/// The ASIO path itself goes through `cpal::host_from_id(HostId::Asio)`; `HostId::from_str`
/// only knows hosts that were compiled in, so `--host asio` fails with a clear message when the
/// feature is missing.
#[cfg(feature = "asio")]
pub fn asio_status() -> &'static str {
    "ASIO is built in (feature \"asio\")."
}

#[cfg(all(not(feature = "asio"), target_os = "windows"))]
pub fn asio_status() -> &'static str {
    "ASIO is NOT built in. Rebuild with: cargo build --release --features asio"
}

#[cfg(all(not(feature = "asio"), not(target_os = "windows")))]
pub fn asio_status() -> &'static str {
    "ASIO exists only on Windows; here the audio host is CoreAudio (macOS) or ALSA (Linux)."
}

pub fn open_host(name: &str) -> Result<Host, String> {
    let id = HostId::from_str(name).map_err(|e| {
        if name.eq_ignore_ascii_case("asio") && cfg!(not(target_os = "windows")) {
            format!(
                "There is no host \"asio\" on this system - ASIO is a Windows driver model. \
                 On macOS the host is called \"{DEFAULT_HOST}\"."
            )
        } else if name.eq_ignore_ascii_case("asio") {
            format!("ASIO host not available: {e}. {}", asio_status())
        } else {
            let available: Vec<String> =
                cpal::available_hosts().iter().map(|h| h.to_string()).collect();
            format!(
                "Unknown host \"{name}\": {e}. Available on this system: {}",
                available.join(", ")
            )
        }
    })?;
    cpal::host_from_id(id).map_err(|e| format!("Could not open host \"{name}\": {e}"))
}

fn device_name(device: &Device) -> String {
    device.to_string()
}

/// Pick a device by case-insensitive substring, or the host default.
pub fn pick_device(host: &Host, wanted: Option<&str>, dir: Direction) -> Result<Device, String> {
    match wanted {
        None => match dir {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        }
        .ok_or_else(|| format!("No default {} device found on this host.", dir.label())),
        Some(needle) => {
            let needle_lc = needle.to_lowercase();
            let devices = host
                .devices()
                .map_err(|e| format!("Cannot read the device list: {e}"))?;
            let mut candidates = Vec::new();
            let mut all = Vec::new();
            for device in devices {
                let fits = match dir {
                    Direction::Input => device.supports_input(),
                    Direction::Output => device.supports_output(),
                };
                let name = device_name(&device);
                if fits {
                    all.push(name.clone());
                    if name.to_lowercase().contains(&needle_lc) {
                        candidates.push(device);
                    }
                }
            }
            match candidates.len() {
                0 => Err(format!(
                    "No {} device matches \"{needle}\". Available:\n  {}",
                    dir.label(),
                    all.join("\n  ")
                )),
                _ => {
                    if candidates.len() > 1 {
                        eprintln!(
                            "Note: {} devices match \"{needle}\", using \"{}\".",
                            candidates.len(),
                            device_name(&candidates[0])
                        );
                    }
                    Ok(candidates.remove(0))
                }
            }
        }
    }
}

fn supported_ranges(
    device: &Device,
    dir: Direction,
) -> Result<Vec<SupportedStreamConfigRange>, String> {
    let list: Vec<SupportedStreamConfigRange> = match dir {
        Direction::Input => device
            .supported_input_configs()
            .map_err(|e| format!("Cannot read {} configurations: {e}", dir.label()))?
            .collect(),
        Direction::Output => device
            .supported_output_configs()
            .map_err(|e| format!("Cannot read {} configurations: {e}", dir.label()))?
            .collect(),
    };
    Ok(list)
}

fn default_channels(device: &Device, dir: Direction) -> Result<u16, String> {
    let cfg = match dir {
        Direction::Input => device.default_input_config(),
        Direction::Output => device.default_output_config(),
    }
    .map_err(|e| format!("Cannot read default configuration ({}): {e}", dir.label()))?;
    Ok(cfg.channels())
}

pub fn describe_buffer_size(b: &SupportedBufferSize) -> String {
    match b {
        SupportedBufferSize::Range { min, max } => format!("{min}..{max} frames"),
        SupportedBufferSize::Unknown => "unknown".to_string(),
    }
}

fn describe_range(r: &SupportedStreamConfigRange) -> String {
    format!(
        "{} channels, {}..{} Hz, {:?}, buffer {}",
        r.channels(),
        r.min_sample_rate(),
        r.max_sample_rate(),
        r.sample_format(),
        describe_buffer_size(r.buffer_size())
    )
}

/// Build a `StreamConfig` for the requested rate/buffer/channels, or explain in plain words why the
/// device cannot do it. Never silently substitutes different values.
pub fn resolve_config(
    device: &Device,
    dir: Direction,
    opts: &DeviceOpts,
    channels: Option<u16>,
) -> Result<StreamPlan, String> {
    let ranges = supported_ranges(device, dir)?;
    if ranges.is_empty() {
        return Err(format!(
            "Device \"{}\" reports no {} configurations.",
            device_name(device),
            dir.label()
        ));
    }
    let channels = match channels {
        Some(c) => c,
        None => default_channels(device, dir)?,
    };

    let by_channels: Vec<&SupportedStreamConfigRange> =
        ranges.iter().filter(|r| r.channels() == channels).collect();
    if by_channels.is_empty() {
        return Err(format!(
            "Device \"{}\" on the {} cannot do {channels} channels. Supported:\n  {}",
            device_name(device),
            dir.label(),
            ranges.iter().map(describe_range).collect::<Vec<_>>().join("\n  ")
        ));
    }

    let by_rate: Vec<&&SupportedStreamConfigRange> = by_channels
        .iter()
        .filter(|r| r.min_sample_rate() <= opts.rate && opts.rate <= r.max_sample_rate())
        .collect();
    if by_rate.is_empty() {
        return Err(format!(
            "Device \"{}\" on the {} cannot do {} Hz with {channels} channels. Supported:\n  {}",
            device_name(device),
            dir.label(),
            opts.rate,
            by_channels.iter().map(|r| describe_range(r)).collect::<Vec<_>>().join("\n  ")
        ));
    }

    // All processing happens in f32. Native f32 is preferred because it needs no conversion at
    // all; the Focusrite ASIO driver only offers i32, so that has to work too.
    let chosen = FORMAT_PREFERENCE
        .iter()
        .find_map(|f| by_rate.iter().find(|r| r.sample_format() == *f))
        .ok_or_else(|| {
            format!(
                "Device \"{}\" on the {} at {} Hz offers none of the supported formats ({}). Available:\n  {}",
                device_name(device),
                dir.label(),
                opts.rate,
                FORMAT_PREFERENCE.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join(", "),
                by_rate.iter().map(|r| describe_range(r)).collect::<Vec<_>>().join("\n  ")
            )
        })?;

    if let SupportedBufferSize::Range { min, max } = *chosen.buffer_size()
        && (opts.buffer < min || opts.buffer > max)
    {
        if !opts.force_buffer {
            let ways = if cfg!(target_os = "windows") {
                format!(
                    "\x20 - measure with --buffer {min} (that is the device period in WASAPI shared mode)\n\
                     \x20 - use ASIO: --host asio --buffer {} (build the binary with --features asio)\n",
                    opts.buffer
                )
            } else {
                format!("\x20 - choose a buffer size between {min} and {max} frames\n")
            };
            return Err(format!(
                "Buffer size {} frames is not supported on the {}. Device \"{}\" reports only {min}..{max} frames at {} Hz.\n\
                 Options:\n\
                 {ways}\
                 \x20 - set --force-buffer to request {} frames anyway (the result then shows what the driver made of it)",
                opts.buffer,
                dir.label(),
                device_name(device),
                opts.rate,
                opts.buffer
            ));
        }
        eprintln!(
            "Warning: --force-buffer is on. Requesting {} frames on the {}, although the device reports {min}..{max}.",
            opts.buffer,
            dir.label()
        );
    }

    Ok(StreamPlan {
        config: StreamConfig {
            channels,
            sample_rate: opts.rate,
            buffer_size: BufferSize::Fixed(opts.buffer),
        },
        format: chosen.sample_format(),
        reported_buffer: *chosen.buffer_size(),
    })
}

pub fn open_output(opts: &DeviceOpts) -> Result<OutputSetup, String> {
    let host = open_host(&opts.host)?;
    let device = pick_device(&host, opts.device.as_deref(), Direction::Output)?;
    let plan = resolve_config(&device, Direction::Output, opts, opts.out_channels)?;
    let name = device_name(&device);
    Ok(OutputSetup {
        device,
        plan,
        name,
        host_name: opts.host.to_lowercase(),
    })
}

/// What to do when input and output live on different devices. The engine needs one clock for
/// both directions; CoreAudio can provide that through an aggregate device.
fn separate_devices_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "\nInput and output must be on the same device (one shared clock). To use two \
         different ones, create an aggregate device with drift correction in \"Audio MIDI Setup\" \
         and choose that."
    } else {
        ""
    }
}

pub fn open_duplex(opts: &DeviceOpts) -> Result<DuplexSetup, String> {
    let host = open_host(&opts.host)?;
    let input = pick_device(&host, opts.device.as_deref(), Direction::Input)?;
    let output = match opts.device.as_deref() {
        Some(_) => pick_device(&host, opts.device.as_deref(), Direction::Output)
            .map_err(|e| format!("{e}{}", separate_devices_hint()))?,
        // No device named: the output follows the input device whenever that one can play. On
        // macOS the default input is typically the interface while the default output is the
        // built-in speakers - two devices, two clocks, and the loop would come out of the laptop.
        None if input.supports_output() => input.clone(),
        None => {
            let output = pick_device(&host, None, Direction::Output)?;
            eprintln!(
                "Warning: default input \"{}\" and default output \"{}\" are different \
                 devices with separate clocks. For the looper, better choose one device with \
                 --device.{}",
                device_name(&input),
                device_name(&output),
                separate_devices_hint()
            );
            output
        }
    };
    let in_plan = resolve_config(&input, Direction::Input, opts, opts.in_channels)?;
    let out_plan = resolve_config(&output, Direction::Output, opts, opts.out_channels)?;
    let in_name = device_name(&input);
    let out_name = device_name(&output);
    Ok(DuplexSetup {
        input,
        in_plan,
        in_name,
        output,
        out_plan,
        out_name,
        host_name: opts.host.to_lowercase(),
    })
}

// ---------------------------------------------------------------------------------------------
// Stream construction with format conversion
// ---------------------------------------------------------------------------------------------

/// Sample formats this tool understands, in order of preference. f32 needs no conversion at all;
/// i32 is what the Focusrite ASIO driver reports; i16 covers the rest.
const FORMAT_PREFERENCE: [SampleFormat; 3] =
    [SampleFormat::F32, SampleFormat::I32, SampleFormat::I16];

/// Conversion scratch space, in multiples of the requested buffer size. Allocated once while the
/// stream is being built, never inside a callback. Generous on purpose: if a driver ever hands us
/// a larger block, the wrapper loops over it in several passes instead of allocating.
const SCRATCH_FACTOR: u32 = 16;

fn scratch_samples(config: &StreamConfig) -> usize {
    let frames = match config.buffer_size {
        BufferSize::Fixed(f) => f.max(1),
        BufferSize::Default => 1024,
    };
    // A multiple of the channel count, so every pass covers whole frames.
    (frames * SCRATCH_FACTOR) as usize * config.channels as usize
}

/// Wraps an f32 callback so it can serve a stream of integer samples.
///
/// The extra `usize` handed to the callback is the frame offset of this pass inside the driver
/// callback - only relevant in the (practically unreachable) case of a block larger than the
/// scratch buffer, where the callback still needs correct offsets for timestamp arithmetic.
fn input_adapter<T, D>(
    scratch_len: usize,
    channels: usize,
    mut callback: D,
) -> impl FnMut(&[T], &InputCallbackInfo) + Send + 'static
where
    T: SizedSample,
    f32: FromSample<T>,
    D: FnMut(&[f32], &InputCallbackInfo, usize) + Send + 'static,
{
    let mut scratch = vec![0.0f32; scratch_len];
    move |data: &[T], info: &InputCallbackInfo| {
        let mut done = 0usize;
        while done < data.len() {
            let n = (data.len() - done).min(scratch.len());
            let out = &mut scratch[..n];
            for (o, i) in out.iter_mut().zip(data[done..done + n].iter()) {
                *o = f32::from_sample_(*i);
            }
            callback(out, info, done / channels);
            done += n;
        }
    }
}

fn output_adapter<T, D>(
    scratch_len: usize,
    channels: usize,
    mut callback: D,
) -> impl FnMut(&mut [T], &OutputCallbackInfo) + Send + 'static
where
    T: SizedSample + FromSample<f32>,
    D: FnMut(&mut [f32], &OutputCallbackInfo, usize) + Send + 'static,
{
    let mut scratch = vec![0.0f32; scratch_len];
    move |data: &mut [T], info: &OutputCallbackInfo| {
        let mut done = 0usize;
        while done < data.len() {
            let n = (data.len() - done).min(scratch.len());
            let buf = &mut scratch[..n];
            buf.fill(0.0);
            callback(buf, info, done / channels);
            for (o, i) in data[done..done + n].iter_mut().zip(buf.iter()) {
                *o = T::from_sample_(*i);
            }
            done += n;
        }
    }
}

/// Build an input stream that delivers f32 no matter what the device speaks.
pub fn build_input<D, E>(
    device: &Device,
    plan: &StreamPlan,
    callback: D,
    error_callback: E,
) -> Result<Stream, String>
where
    D: FnMut(&[f32], &InputCallbackInfo, usize) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let config = plan.config;
    let channels = config.channels as usize;
    let len = scratch_samples(&config);
    let mut callback = callback;
    let result = match plan.format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], info: &InputCallbackInfo| callback(data, info, 0),
            error_callback,
            None,
        ),
        SampleFormat::I32 => device.build_input_stream(
            config,
            input_adapter::<i32, D>(len, channels, callback),
            error_callback,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            config,
            input_adapter::<i16, D>(len, channels, callback),
            error_callback,
            None,
        ),
        other => return Err(unsupported_format(other)),
    };
    result.map_err(|e| format!("Could not build the input stream: {e}"))
}

/// Build an output stream that is fed f32 no matter what the device speaks.
pub fn build_output<D, E>(
    device: &Device,
    plan: &StreamPlan,
    callback: D,
    error_callback: E,
) -> Result<Stream, String>
where
    D: FnMut(&mut [f32], &OutputCallbackInfo, usize) + Send + 'static,
    E: FnMut(cpal::Error) + Send + 'static,
{
    let config = plan.config;
    let channels = config.channels as usize;
    let len = scratch_samples(&config);
    let mut callback = callback;
    let result = match plan.format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], info: &OutputCallbackInfo| callback(data, info, 0),
            error_callback,
            None,
        ),
        SampleFormat::I32 => device.build_output_stream(
            config,
            output_adapter::<i32, D>(len, channels, callback),
            error_callback,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            config,
            output_adapter::<i16, D>(len, channels, callback),
            error_callback,
            None,
        ),
        other => return Err(unsupported_format(other)),
    };
    result.map_err(|e| format!("Could not build the output stream: {e}"))
}

fn unsupported_format(format: SampleFormat) -> String {
    format!(
        "Sample format {format:?} is not supported by this tool (only {}).",
        FORMAT_PREFERENCE
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub fn print_setup(setup: &DuplexSetup) {
    println!("Host:     {}", setup.host_name);
    println!(
        "Input:    {} ({} channels, {:?})",
        setup.in_name, setup.in_plan.config.channels, setup.in_plan.format
    );
    println!(
        "Output:   {} ({} channels, {:?})",
        setup.out_name, setup.out_plan.config.channels, setup.out_plan.format
    );
    println!(
        "Format:   {} Hz, {} frames buffer requested ({:.2} ms per buffer)",
        setup.sample_rate(),
        setup.buffer_frames(),
        setup.buffer_frames() as f64 * 1000.0 / setup.sample_rate() as f64
    );
    println!(
        "Device reports buffer range: input {}, output {}",
        describe_buffer_size(&setup.in_plan.reported_buffer),
        describe_buffer_size(&setup.out_plan.reported_buffer)
    );
}

/// `list` subcommand: every host, every device, every supported config.
pub fn cmd_list() -> Result<(), String> {
    println!("{}\n", asio_status());
    let hosts = cpal::available_hosts();
    if hosts.is_empty() {
        println!("No audio host available.");
        return Ok(());
    }
    for host_id in hosts {
        println!("=== Host: {} ({}) ===", host_id.name(), host_id);
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(e) => {
                println!("  Cannot open host: {e}\n");
                continue;
            }
        };
        let default_in = host.default_input_device();
        let default_out = host.default_output_device();
        let devices = match host.devices() {
            Ok(d) => d,
            Err(e) => {
                println!("  Cannot read the device list: {e}\n");
                continue;
            }
        };
        let mut count = 0usize;
        for device in devices {
            count += 1;
            let name = device_name(&device);
            let mut tags = Vec::new();
            if default_in.as_ref() == Some(&device) {
                tags.push("DEFAULT INPUT");
            }
            if default_out.as_ref() == Some(&device) {
                tags.push("DEFAULT OUTPUT");
            }
            let tag = if tags.is_empty() {
                String::new()
            } else {
                format!("  [{}]", tags.join(" + "))
            };
            println!("\n  Device: {name}{tag}");
            if let Ok(desc) = device.description() {
                let mut extra = Vec::new();
                if let Some(m) = desc.manufacturer() {
                    extra.push(format!("Manufacturer: {m}"));
                }
                if let Some(d) = desc.driver() {
                    extra.push(format!("Driver: {d}"));
                }
                extra.push(format!("Type: {:?}", desc.device_type()));
                extra.push(format!("Interface: {:?}", desc.interface_type()));
                println!("    {}", extra.join(", "));
            }
            for dir in [Direction::Input, Direction::Output] {
                match supported_ranges(&device, dir) {
                    Ok(list) if list.is_empty() => {}
                    Ok(list) => {
                        println!("    {} configurations:", dir.label());
                        for r in &list {
                            println!("      - {}", describe_range(r));
                        }
                        let default_cfg = match dir {
                            Direction::Input => device.default_input_config(),
                            Direction::Output => device.default_output_config(),
                        };
                        if let Ok(cfg) = default_cfg {
                            println!(
                                "      Default: {} channels, {} Hz, {:?}, buffer {}",
                                cfg.channels(),
                                cfg.sample_rate(),
                                cfg.sample_format(),
                                describe_buffer_size(cfg.buffer_size())
                            );
                        }
                    }
                    Err(_) => {}
                }
            }
        }
        if count == 0 {
            println!("  (no devices)");
        }
        println!();
    }
    Ok(())
}
