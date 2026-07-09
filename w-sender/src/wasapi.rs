#[cfg(not(windows))]
pub fn list_capture_devices() -> anyhow::Result<()> {
    anyhow::bail!("w-sender is Windows-only")
}

#[cfg(not(windows))]
pub fn run_capture_sender(
    _args: crate::args::Args,
    _metrics: std::sync::Arc<crate::metrics::Metrics>,
    _stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    anyhow::bail!("w-sender is Windows-only")
}

#[cfg(windows)]
mod imp {
    use crate::args::Args;
    use crate::metrics::Metrics;
    use crate::packetizer::{InputFormat, Packetizer, SampleEncoding};
    use anyhow::{anyhow, bail, Context, Result};
    use lan_audio_common::audio::SAMPLE_RATE;
    use std::ffi::c_void;
    use std::net::UdpSocket;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;
    use windows::core::{PCSTR, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{
        CloseHandle, HANDLE, RPC_E_CHANGED_MODE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::Media::Audio::{
        eCapture, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
        AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_NOPERSIST, AUDCLNT_S_BUFFER_EMPTY,
        DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
    };
    use windows::Win32::Media::KernelStreaming::{
        KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE,
    };
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };
    use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
        CreateEventA, WaitForSingleObject, AVRT_PRIORITY_CRITICAL,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;

    pub fn list_capture_devices() -> Result<()> {
        let _com = ComGuard::init()?;
        let devices = enumerate_capture_devices()?;
        for device in devices {
            match open_mix_format(&device.device) {
                Ok(format) => println!("{}: {} [{}]", device.index, device.name, format.describe()),
                Err(err) => println!(
                    "{}: {} [format unavailable: {err}]",
                    device.index, device.name
                ),
            }
        }
        Ok(())
    }

    pub fn run_capture_sender(
        args: Args,
        metrics: Arc<Metrics>,
        stop: Arc<AtomicBool>,
    ) -> Result<()> {
        let _com = ComGuard::init()?;
        let _priority = MmcssGuard::enter_pro_audio();
        let socket = UdpSocket::bind(args.bind)
            .with_context(|| format!("failed to bind UDP socket to {}", args.bind))?;
        socket
            .connect(args.target)
            .with_context(|| format!("failed to connect UDP socket to {}", args.target))?;
        socket
            .set_nonblocking(true)
            .context("failed to set UDP socket nonblocking")?;

        let selected = select_capture_device(&args.device)?;
        let audio_client: IAudioClient = unsafe {
            selected
                .device
                .Activate(CLSCTX_ALL, None)
                .context("failed to activate WASAPI audio client")?
        };
        let mix_format = unsafe { MixFormatPtr::new(audio_client.GetMixFormat()?) };
        let format = parse_wave_format(mix_format.as_ptr())?;
        validate_format(&format, args.require_48k_stereo)?;
        let (default_period_hns, minimum_period_hns) = device_periods(&audio_client);

        unsafe {
            audio_client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_NOPERSIST,
                    0,
                    0,
                    mix_format.as_ptr(),
                    None,
                )
                .context("failed to initialize WASAPI shared event capture")?;
        }

        let buffer_frames = unsafe { audio_client.GetBufferSize()? };
        let stream_latency_hns = unsafe { audio_client.GetStreamLatency().unwrap_or_default() };
        let event = EventHandle::new()?;
        unsafe {
            audio_client
                .SetEventHandle(event.handle())
                .context("failed to set WASAPI event handle")?;
        }
        let capture_client = unsafe {
            audio_client
                .GetService::<IAudioCaptureClient>()
                .context("failed to get WASAPI capture client")?
        };

        println!(
            "w-sender: selected_device=\"{}\" target={} bind={} format={} buffer={}fr default_period={:.3}ms minimum_period={:.3}ms stream_latency={:.3}ms max_packet_frames={} wasapi_mode=shared_event",
            selected.name,
            args.target,
            args.bind,
            format.describe(),
            buffer_frames,
            hns_to_ms(default_period_hns),
            hns_to_ms(minimum_period_hns),
            hns_to_ms(stream_latency_hns),
            args.max_packet_frames,
        );

        let mut packetizer = Packetizer::new(args.max_packet_frames);
        unsafe {
            audio_client
                .Start()
                .context("failed to start WASAPI capture")?;
        }
        let run_result = capture_loop(
            &capture_client,
            event.handle(),
            &format,
            &socket,
            &metrics,
            &stop,
            &mut packetizer,
        );
        unsafe {
            let _ = audio_client.Stop();
        }
        run_result
    }

    fn capture_loop(
        capture_client: &IAudioCaptureClient,
        event: HANDLE,
        format: &InputFormat,
        socket: &UdpSocket,
        metrics: &Metrics,
        stop: &AtomicBool,
        packetizer: &mut Packetizer,
    ) -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            match unsafe { WaitForSingleObject(event, 100) } {
                WAIT_OBJECT_0 => {
                    drain_capture(capture_client, format, socket, metrics, packetizer)?
                }
                WAIT_TIMEOUT => {}
                WAIT_FAILED => bail!("WaitForSingleObject failed"),
                other => bail!("unexpected wait result: {:?}", other),
            }
        }
        Ok(())
    }

    fn drain_capture(
        capture_client: &IAudioCaptureClient,
        format: &InputFormat,
        socket: &UdpSocket,
        metrics: &Metrics,
        packetizer: &mut Packetizer,
    ) -> Result<()> {
        loop {
            let mut frames_available = match unsafe { capture_client.GetNextPacketSize() } {
                Ok(0) => return Ok(()),
                Ok(frames) => frames,
                Err(err) => return Err(anyhow!(err)).context("failed to query WASAPI packet size"),
            };
            let mut data_ptr: *mut u8 = ptr::null_mut();
            let mut flags = 0u32;
            let mut device_position = 0u64;
            let mut qpc_position = 0u64;
            let result = unsafe {
                capture_client.GetBuffer(
                    &mut data_ptr,
                    &mut frames_available,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                )
            };
            match result {
                Err(err) if err.code() == AUDCLNT_S_BUFFER_EMPTY => continue,
                Err(err) => {
                    return Err(anyhow!(err)).context("failed to read WASAPI capture buffer")
                }
                Ok(()) => {}
            }

            let event_start = Instant::now();
            let silent = flag_set(flags, AUDCLNT_BUFFERFLAGS_SILENT.0);
            let discontinuity = flag_set(flags, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0);
            let timestamp_error = flag_set(flags, AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0);
            metrics.record_event(
                event_start,
                frames_available,
                silent,
                discontinuity,
                timestamp_error,
            );

            let data_len = frames_available as usize * usize::from(format.block_align);
            let data = if silent {
                None
            } else {
                if data_ptr.is_null() {
                    let _ = unsafe { capture_client.ReleaseBuffer(frames_available) };
                    bail!("WASAPI returned a null capture buffer for non-silent data");
                }
                Some(unsafe { std::slice::from_raw_parts(data_ptr, data_len) })
            };

            let packetize_result =
                packetizer.packetize_capture_chunk(data, frames_available, format, |bytes| {
                    match socket.send(bytes) {
                        Ok(sent) => metrics.record_packet_sent(sent),
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            metrics.record_send_would_block()
                        }
                        Err(err) => {
                            metrics.record_send_error();
                            eprintln!("w-sender: UDP send error: {err}");
                        }
                    }
                });
            let release_result = unsafe { capture_client.ReleaseBuffer(frames_available) };
            release_result.context("failed to release WASAPI capture buffer")?;
            let stats = packetize_result?;
            metrics.record_rms(stats.rms_left_db, stats.rms_right_db);
            metrics.record_event_done(event_start);
        }
    }

    fn select_capture_device(filter: &str) -> Result<CaptureDevice> {
        let filter_lower = filter.to_lowercase();
        enumerate_capture_devices()?
            .into_iter()
            .find(|device| device.name.to_lowercase().contains(&filter_lower))
            .ok_or_else(|| anyhow!("capture device containing {filter:?} was not found"))
    }

    fn enumerate_capture_devices() -> Result<Vec<CaptureDevice>> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let collection = unsafe { enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)? };
        let count = unsafe { collection.GetCount()? };
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = unsafe { collection.Item(index)? };
            let name = device_friendly_name(&device).unwrap_or_else(|| "<unknown>".to_string());
            devices.push(CaptureDevice {
                index,
                name,
                device,
            });
        }
        Ok(devices)
    }

    fn open_mix_format(device: &IMMDevice) -> Result<InputFormat> {
        let audio_client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        let mix_format = unsafe { MixFormatPtr::new(audio_client.GetMixFormat()?) };
        parse_wave_format(mix_format.as_ptr())
    }

    fn parse_wave_format(format_ptr: *const WAVEFORMATEX) -> Result<InputFormat> {
        if format_ptr.is_null() {
            bail!("WASAPI returned a null mix format");
        }
        let format = unsafe { ptr::read_unaligned(format_ptr) };
        let tag = u32::from(format.wFormatTag);
        let sample_rate = format.nSamplesPerSec;
        let channels = format.nChannels;
        let block_align = format.nBlockAlign;
        let mut bits_per_sample = format.wBitsPerSample;
        let encoding = match tag {
            WAVE_FORMAT_PCM => match bits_per_sample {
                16 => SampleEncoding::I16,
                24 => SampleEncoding::I24,
                32 => SampleEncoding::I32,
                other => bail!("unsupported PCM sample size: {other} bits"),
            },
            WAVE_FORMAT_IEEE_FLOAT => match bits_per_sample {
                32 => SampleEncoding::F32,
                other => bail!("unsupported float sample size: {other} bits"),
            },
            WAVE_FORMAT_EXTENSIBLE => {
                let ext = unsafe { ptr::read_unaligned(format_ptr as *const WAVEFORMATEXTENSIBLE) };
                let sub_format = ext.SubFormat;
                let valid_bits = unsafe { ext.Samples.wValidBitsPerSample };
                if valid_bits != 0 {
                    bits_per_sample = valid_bits;
                }
                if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    if bits_per_sample != 32 {
                        bail!("unsupported extensible float sample size: {bits_per_sample} bits");
                    }
                    SampleEncoding::F32
                } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
                    match bits_per_sample {
                        16 => SampleEncoding::I16,
                        24 => {
                            if format.wBitsPerSample == 32 {
                                SampleEncoding::I32
                            } else {
                                SampleEncoding::I24
                            }
                        }
                        32 => SampleEncoding::I32,
                        other => bail!("unsupported extensible PCM sample size: {other} bits"),
                    }
                } else {
                    bail!("unsupported WAVEFORMATEXTENSIBLE subformat: {sub_format:?}");
                }
            }
            other => bail!("unsupported WASAPI format tag: {other}"),
        };

        Ok(InputFormat {
            sample_rate,
            channels,
            bits_per_sample: format.wBitsPerSample,
            block_align,
            encoding,
        })
    }

    fn validate_format(format: &InputFormat, require_48k_stereo: bool) -> Result<()> {
        if format.sample_rate != SAMPLE_RATE {
            bail!(
                "w-sender currently requires {}Hz capture for receiver compatibility; selected device is {}Hz",
                SAMPLE_RATE,
                format.sample_rate
            );
        }
        if require_48k_stereo && format.channels != 2 {
            bail!(
                "--require-48k-stereo requested stereo capture, selected device has {} channels",
                format.channels
            );
        }
        if format.channels < 1 {
            bail!("capture format must have at least one channel");
        }
        if format.bytes_per_sample() == 0 {
            bail!("capture format has invalid sample size");
        }
        Ok(())
    }

    fn device_periods(audio_client: &IAudioClient) -> (i64, i64) {
        let mut default_period = 0i64;
        let mut minimum_period = 0i64;
        let _ = unsafe {
            audio_client.GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period))
        };
        (default_period, minimum_period)
    }

    fn device_friendly_name(device: &IMMDevice) -> Option<String> {
        unsafe {
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
            let result = propvariant_lpwstr(&value);
            let _ = PropVariantClear(&mut value);
            result
        }
    }

    unsafe fn propvariant_lpwstr(
        value: &windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> Option<String> {
        let prop_variant = &value.Anonymous.Anonymous;
        if prop_variant.vt != VT_LPWSTR {
            return None;
        }
        let ptr_utf16 = *(&prop_variant.Anonymous as *const _ as *const *const u16);
        if ptr_utf16.is_null() {
            return None;
        }
        const MAX_STRING_LEN: usize = 32_768;
        let mut len = 0usize;
        while len < MAX_STRING_LEN && *ptr_utf16.add(len) != 0 {
            len += 1;
        }
        if len >= MAX_STRING_LEN {
            return None;
        }
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(
            ptr_utf16, len,
        )))
    }

    fn flag_set(flags: u32, flag: i32) -> bool {
        flags & flag as u32 != 0
    }

    fn hns_to_ms(hns: i64) -> f64 {
        hns.max(0) as f64 / 10_000.0
    }

    struct CaptureDevice {
        index: u32,
        name: String,
        device: IMMDevice,
    }

    struct MixFormatPtr(*mut WAVEFORMATEX);

    impl MixFormatPtr {
        unsafe fn new(ptr: *mut WAVEFORMATEX) -> Self {
            Self(ptr)
        }

        fn as_ptr(&self) -> *const WAVEFORMATEX {
            self.0
        }
    }

    impl Drop for MixFormatPtr {
        fn drop(&mut self) {
            unsafe {
                CoTaskMemFree(Some(self.0.cast::<c_void>()));
            }
        }
    }

    struct EventHandle(HANDLE);

    impl EventHandle {
        fn new() -> Result<Self> {
            let handle = unsafe { CreateEventA(None, false, false, PCSTR::null()) }
                .context("failed to create WASAPI event")?;
            Ok(Self(handle))
        }

        fn handle(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for EventHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct ComGuard {
        uninitialize: bool,
    }

    impl ComGuard {
        fn init() -> Result<Self> {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr == RPC_E_CHANGED_MODE {
                return Ok(Self {
                    uninitialize: false,
                });
            }
            hr.ok().context("failed to initialize COM")?;
            Ok(Self { uninitialize: true })
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.uninitialize {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    struct MmcssGuard {
        handle: Option<HANDLE>,
    }

    impl MmcssGuard {
        fn enter_pro_audio() -> Self {
            let task_name: Vec<u16> = "Pro Audio".encode_utf16().chain(Some(0)).collect();
            let mut task_index = 0u32;
            match unsafe {
                AvSetMmThreadCharacteristicsW(PCWSTR(task_name.as_ptr()), &mut task_index)
            } {
                Ok(handle) => {
                    let priority_result =
                        unsafe { AvSetMmThreadPriority(handle, AVRT_PRIORITY_CRITICAL) };
                    if let Err(err) = priority_result {
                        eprintln!("w-sender: failed to set MMCSS priority: {err}");
                    }
                    println!(
                        "w-sender: capture_thread_priority=mmcss task=\"Pro Audio\" priority=critical"
                    );
                    Self {
                        handle: Some(handle),
                    }
                }
                Err(err) => {
                    eprintln!("w-sender: failed to enter MMCSS Pro Audio class: {err}");
                    Self { handle: None }
                }
            }
        }
    }

    impl Drop for MmcssGuard {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.take() {
                unsafe {
                    let _ = AvRevertMmThreadCharacteristics(handle);
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{list_capture_devices, run_capture_sender};
