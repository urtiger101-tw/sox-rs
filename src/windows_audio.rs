//! Windows Core Audio control module for soundx.
//!
//! Provides bounded querying and volume/mute control over Windows audio endpoints
//! and session instances using official IMMDeviceEnumerator, IAudioEndpointVolume,
//! IAudioSessionManager2, and ISimpleAudioVolume APIs.

#[cfg(windows)]
use anyhow::Context;
use serde_json::Value;

// ============================================================================
// Public API
// ============================================================================

/// Enumerate active audio endpoints.
///
/// Returns a JSON array of active endpoint objects:
/// `[{ id, name, flow, default, volume, muted }, ...]`.
///
/// Inaccessible endpoint details are preserved with explicit error data
/// rather than silently dropping devices.
pub fn endpoints() -> anyhow::Result<Value> {
    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows Core Audio is not supported on this platform");
    }
    #[cfg(windows)]
    {
        imp::endpoints()
    }
}

/// Query or update an audio endpoint volume and mute state.
///
/// - `flow` accepts `"output"` or `"input"`.
/// - `volume` is a percentage `0..=100`, must be finite.
/// - Supplying neither `volume` nor `mute` acts as a read-only query.
/// - Passing `None` for `device_id` selects the default multimedia endpoint for `flow`.
/// - Reject embedded NUL IDs.
///
/// Returns `{ id, name, flow, volume, muted }`.
pub fn endpoint(
    device_id: Option<&str>,
    flow: &str,
    volume: Option<f32>,
    mute: Option<bool>,
) -> anyhow::Result<Value> {
    #[cfg(not(windows))]
    let _ = mute;
    validate_flow(flow)?;
    if let Some(vol) = volume {
        validate_volume(vol)?;
    }
    if let Some(id) = device_id {
        validate_id(id, "device_id")?;
    }

    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows Core Audio is not supported on this platform");
    }
    #[cfg(windows)]
    {
        imp::endpoint(device_id, flow, volume, mute)
    }
}

/// Enumerate active audio sessions for an output device.
///
/// - Passing `None` for `device_id` selects the default multimedia output endpoint.
/// - Explicit capture endpoints are rejected.
/// - Identifies sessions using stable session instance identifiers.
///
/// Returns a JSON array of session objects:
/// `[{ id, device_id, process_id, name, state, volume, muted }, ...]`.
pub fn sessions(device_id: Option<&str>) -> anyhow::Result<Value> {
    if let Some(id) = device_id {
        validate_id(id, "device_id")?;
    }

    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows Core Audio is not supported on this platform");
    }
    #[cfg(windows)]
    {
        imp::sessions(device_id)
    }
}

/// Query or update an audio session volume and mute state.
///
/// - `session_id` must match the exact stable session instance identifier.
/// - `volume` is a percentage `0..=100`, must be finite.
/// - Supplying neither `volume` nor `mute` acts as a read-only query.
/// - Session operations only operate on output devices; capture endpoints are rejected.
///
/// Returns updated readback object `{ id, device_id, process_id, name, state, volume, muted }`.
pub fn session(
    device_id: Option<&str>,
    session_id: &str,
    volume: Option<f32>,
    mute: Option<bool>,
) -> anyhow::Result<Value> {
    #[cfg(not(windows))]
    let _ = mute;
    validate_id(session_id, "session_id")?;
    if let Some(id) = device_id {
        validate_id(id, "device_id")?;
    }
    if let Some(vol) = volume {
        validate_volume(vol)?;
    }

    #[cfg(not(windows))]
    {
        anyhow::bail!("Windows Core Audio is not supported on this platform");
    }
    #[cfg(windows)]
    {
        imp::session(device_id, session_id, volume, mute)
    }
}

// ============================================================================
// Parameter Validation
// ============================================================================

fn validate_flow(flow: &str) -> anyhow::Result<&'static str> {
    if flow.eq_ignore_ascii_case("output") {
        Ok("output")
    } else if flow.eq_ignore_ascii_case("input") {
        Ok("input")
    } else {
        anyhow::bail!("Invalid flow '{flow}': expected 'output' or 'input'");
    }
}

fn validate_volume(vol: f32) -> anyhow::Result<()> {
    if !vol.is_finite() {
        anyhow::bail!("Volume must be a finite number, got {vol}");
    }
    if !(0.0..=100.0).contains(&vol) {
        anyhow::bail!("Volume percentage must be in range 0..=100, got {vol}");
    }
    Ok(())
}

fn validate_id(id: &str, field_name: &str) -> anyhow::Result<()> {
    if id.contains('\0') {
        anyhow::bail!("{field_name} contains embedded NUL byte");
    }
    Ok(())
}

// ============================================================================
// Windows Implementation
// ============================================================================

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Foundation::{CloseHandle, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, AudioSessionStateExpired, AudioSessionStateInactive,
        DEVICE_STATE_ACTIVE, EDataFlow, IAudioSessionControl, IAudioSessionControl2,
        IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, IMMEndpoint, ISimpleAudioVolume,
        MMDeviceEnumerator, eAll, eCapture, eMultimedia, eRender,
    };
    use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
        CoUninitialize, STGM_READ,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::System::Variant::VT_LPWSTR;
    use windows::core::{Interface, PCWSTR, PWSTR};

    struct ComGuard {
        needs_uninit: bool,
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.needs_uninit {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    fn with_com<T, F>(f: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let guard = if hr == S_OK || hr == S_FALSE {
            ComGuard { needs_uninit: true }
        } else if hr == RPC_E_CHANGED_MODE {
            ComGuard {
                needs_uninit: false,
            }
        } else {
            anyhow::bail!(
                "Failed to initialize COM library: HRESULT {:#010x}",
                hr.0 as u32
            );
        };

        let result = f();
        drop(guard);
        result
    }

    struct CoTaskMemGuard(PWSTR);

    impl Drop for CoTaskMemGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    CoTaskMemFree(Some(self.0.as_ptr() as *const core::ffi::c_void));
                }
            }
        }
    }

    unsafe fn pwstr_to_string_and_free(pwstr: PWSTR) -> anyhow::Result<String> {
        unsafe {
            if pwstr.is_null() {
                return Ok(String::new());
            }
            let guard = CoTaskMemGuard(pwstr);
            let s = guard
                .0
                .to_string()
                .map_err(|e| anyhow::anyhow!("Invalid UTF-16 string: {e}"))?;
            Ok(s)
        }
    }

    struct PropVariantGuard(PROPVARIANT);

    impl Drop for PropVariantGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = PropVariantClear(&mut self.0);
            }
        }
    }

    unsafe fn get_device_id(device: &IMMDevice) -> anyhow::Result<String> {
        unsafe {
            let pwstr = device.GetId().context("Failed to get device ID")?;
            pwstr_to_string_and_free(pwstr)
        }
    }

    unsafe fn get_device_friendly_name(device: &IMMDevice) -> anyhow::Result<String> {
        unsafe {
            let store = device
                .OpenPropertyStore(STGM_READ)
                .context("Failed to open device property store")?;
            let propvar = store
                .GetValue(&PKEY_Device_FriendlyName)
                .context("Failed to read PKEY_Device_FriendlyName")?;
            let guard = PropVariantGuard(propvar);

            if guard.0.Anonymous.Anonymous.vt == VT_LPWSTR {
                let pwstr = guard.0.Anonymous.Anonymous.Anonymous.pwszVal;
                if !pwstr.is_null() {
                    pwstr
                        .to_string()
                        .map_err(|e| anyhow::anyhow!("Invalid UTF-16 in friendly name: {e}"))
                } else {
                    Ok(String::new())
                }
            } else {
                Ok(String::new())
            }
        }
    }

    unsafe fn get_device_flow(device: &IMMDevice) -> anyhow::Result<(&'static str, EDataFlow)> {
        unsafe {
            let endpoint: IMMEndpoint = device
                .cast()
                .context("Failed to cast IMMDevice to IMMEndpoint")?;
            let flow = endpoint
                .GetDataFlow()
                .context("Failed to get device data flow")?;
            if flow == eRender {
                Ok(("output", eRender))
            } else if flow == eCapture {
                Ok(("input", eCapture))
            } else {
                anyhow::bail!("Unsupported device flow enum value: {:?}", flow);
            }
        }
    }

    unsafe fn get_endpoint_volume_and_mute(device: &IMMDevice) -> anyhow::Result<(f64, bool)> {
        unsafe {
            let endpoint_vol: IAudioEndpointVolume = device
                .Activate(CLSCTX_ALL, None)
                .context("Failed to activate IAudioEndpointVolume")?;
            let scalar = endpoint_vol
                .GetMasterVolumeLevelScalar()
                .context("Failed to get master volume scalar")?;
            let muted = endpoint_vol
                .GetMute()
                .context("Failed to get mute status")?
                .as_bool();
            let vol_pct = f64::from(scalar) * 100.0;
            Ok((vol_pct, muted))
        }
    }

    fn to_wide_null_terminated(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(core::iter::once(0)).collect()
    }

    fn get_process_name(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let res = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            if res.is_ok() && size > 0 {
                let full_path = String::from_utf16_lossy(&buf[..size as usize]);
                let file_name = std::path::Path::new(&full_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&full_path);
                Some(file_name.to_string())
            } else {
                None
            }
        }
    }

    pub fn endpoints() -> anyhow::Result<Value> {
        with_com(|| {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .context("Failed to create MMDeviceEnumerator")?;

            let default_render_id = unsafe {
                enumerator
                    .GetDefaultAudioEndpoint(eRender, eMultimedia)
                    .ok()
                    .and_then(|dev| get_device_id(&dev).ok())
            };
            let default_capture_id = unsafe {
                enumerator
                    .GetDefaultAudioEndpoint(eCapture, eMultimedia)
                    .ok()
                    .and_then(|dev| get_device_id(&dev).ok())
            };

            let collection = unsafe { enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE) }
                .context("Failed to enumerate active audio endpoints")?;
            let count = unsafe { collection.GetCount() }.context("Failed to get device count")?;

            let mut list = Vec::with_capacity(count as usize);

            for i in 0..count {
                let device = match unsafe { collection.Item(i) } {
                    Ok(dev) => dev,
                    Err(e) => {
                        list.push(serde_json::json!({
                            "id": Value::Null,
                            "name": Value::Null,
                            "flow": "unknown",
                            "default": false,
                            "volume": Value::Null,
                            "muted": Value::Null,
                            "error": format!("Failed to access device at index {i}: {e}"),
                        }));
                        continue;
                    }
                };

                let id_res = unsafe { get_device_id(&device) };
                let name_res = unsafe { get_device_friendly_name(&device) };
                let flow_res = unsafe { get_device_flow(&device) };
                let vol_res = unsafe { get_endpoint_volume_and_mute(&device) };

                let id = id_res.as_ref().ok().cloned();
                let name = name_res.as_ref().ok().cloned();
                let flow_str = flow_res.as_ref().map(|(s, _)| *s).unwrap_or("unknown");

                let is_default = match (&id, flow_str) {
                    (Some(id_str), "output") => default_render_id.as_deref() == Some(id_str),
                    (Some(id_str), "input") => default_capture_id.as_deref() == Some(id_str),
                    _ => false,
                };

                let mut obj = serde_json::Map::new();
                obj.insert(
                    "id".to_owned(),
                    id.map(Value::String).unwrap_or(Value::Null),
                );
                obj.insert(
                    "name".to_owned(),
                    name.map(Value::String).unwrap_or(Value::Null),
                );
                obj.insert("flow".to_owned(), Value::String(flow_str.to_string()));
                obj.insert("default".to_owned(), Value::Bool(is_default));

                match vol_res {
                    Ok((vol, muted)) => {
                        obj.insert("volume".to_owned(), serde_json::json!(vol));
                        obj.insert("muted".to_owned(), Value::Bool(muted));
                    }
                    Err(e) => {
                        obj.insert("volume".to_owned(), Value::Null);
                        obj.insert("muted".to_owned(), Value::Null);
                        obj.insert(
                            "error".to_owned(),
                            Value::String(format!("Endpoint volume inaccessible: {e:#}")),
                        );
                    }
                }

                if !obj.contains_key("error") {
                    if let Err(e) = &id_res {
                        obj.insert(
                            "error".to_owned(),
                            Value::String(format!("ID inaccessible: {e:#}")),
                        );
                    } else if let Err(e) = &name_res {
                        obj.insert(
                            "error".to_owned(),
                            Value::String(format!("Name inaccessible: {e:#}")),
                        );
                    } else if let Err(e) = &flow_res {
                        obj.insert(
                            "error".to_owned(),
                            Value::String(format!("Flow inaccessible: {e:#}")),
                        );
                    }
                }

                list.push(Value::Object(obj));
            }

            Ok(Value::Array(list))
        })
    }

    pub fn endpoint(
        device_id: Option<&str>,
        flow: &str,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> anyhow::Result<Value> {
        let expected_flow_str = validate_flow(flow)?;
        let expected_dataflow = match expected_flow_str {
            "output" => eRender,
            "input" => eCapture,
            _ => unreachable!(),
        };

        with_com(|| {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                    .context("Failed to create MMDeviceEnumerator")?;

            let device = match device_id {
                Some(id) => {
                    let wide = to_wide_null_terminated(id);
                    unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
                        .with_context(|| format!("Audio endpoint not found for ID: '{id}'"))?
                }
                None => {
                    unsafe { enumerator.GetDefaultAudioEndpoint(expected_dataflow, eMultimedia) }
                        .with_context(|| {
                            format!(
                                "Failed to retrieve default multimedia {expected_flow_str} endpoint"
                            )
                        })?
                }
            };

            let (actual_flow_str, _) = unsafe { get_device_flow(&device) }?;
            if actual_flow_str != expected_flow_str {
                anyhow::bail!(
                    "Device flow mismatch: requested '{expected_flow_str}', but device is '{actual_flow_str}'"
                );
            }

            let endpoint_vol: IAudioEndpointVolume =
                unsafe { device.Activate(CLSCTX_ALL, None) }
                    .context("Failed to activate IAudioEndpointVolume")?;

            if let Some(vol) = volume {
                let scalar = vol / 100.0;
                unsafe { endpoint_vol.SetMasterVolumeLevelScalar(scalar, core::ptr::null()) }
                    .context("Failed to set master volume scalar")?;
            }
            if let Some(m) = mute {
                unsafe { endpoint_vol.SetMute(m, core::ptr::null()) }
                    .context("Failed to set mute status")?;
            }

            let dev_id = unsafe { get_device_id(&device) }?;
            let dev_name = unsafe { get_device_friendly_name(&device) }?;
            let current_scalar = unsafe { endpoint_vol.GetMasterVolumeLevelScalar() }
                .context("Failed to read master volume scalar")?;
            let current_muted = unsafe { endpoint_vol.GetMute() }
                .context("Failed to read mute state")?
                .as_bool();
            let current_vol = f64::from(current_scalar) * 100.0;

            Ok(serde_json::json!({
                "id": dev_id,
                "name": dev_name,
                "flow": actual_flow_str,
                "volume": current_vol,
                "muted": current_muted,
            }))
        })
    }

    fn output_device(
        enumerator: &IMMDeviceEnumerator,
        id: Option<&str>,
    ) -> anyhow::Result<IMMDevice> {
        let device = if let Some(id) = id {
            let wide = to_wide_null_terminated(id);
            unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
                .context("Output device not found")?
        } else {
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) }
                .context("Default output device unavailable")?
        };
        if unsafe { get_device_flow(&device) }?.1 != eRender {
            anyhow::bail!("Session operations require an output endpoint");
        }
        Ok(device)
    }

    fn session_data(control: &IAudioSessionControl, device_id: &str) -> anyhow::Result<Value> {
        let control2: IAudioSessionControl2 =
            control.cast().context("Session control unavailable")?;
        let id = unsafe { pwstr_to_string_and_free(control2.GetSessionInstanceIdentifier()?) }?;
        let pid = unsafe { control2.GetProcessId() }.context("Session process id unavailable")?;
        let state = unsafe { control.GetState() }.context("Session state unavailable")?;
        let state = if state == AudioSessionStateActive {
            "active"
        } else if state == AudioSessionStateInactive {
            "inactive"
        } else if state == AudioSessionStateExpired {
            "expired"
        } else {
            "unknown"
        };
        let display = unsafe { pwstr_to_string_and_free(control.GetDisplayName()?) }?;
        let name = if !display.is_empty() {
            display
        } else if unsafe { control2.IsSystemSoundsSession() == S_OK } {
            "System Sounds".into()
        } else {
            get_process_name(pid).unwrap_or_else(|| format!("PID {pid}"))
        };
        let volume: ISimpleAudioVolume = control.cast().context("Session volume unavailable")?;
        // Return the actual scalar without rounding so a read/restore cycle is lossless.
        let scalar =
            unsafe { volume.GetMasterVolume() }.context("Could not read session volume")?;
        let muted = unsafe { volume.GetMute() }
            .context("Could not read session mute")?
            .as_bool();
        Ok(
            serde_json::json!({"id":id,"device_id":device_id,"process_id":pid,
            "name":name,"state":state,"volume":f64::from(scalar)*100.0,"muted":muted}),
        )
    }

    pub fn sessions(device_id: Option<&str>) -> anyhow::Result<Value> {
        with_com(|| {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
            let device = output_device(&enumerator, device_id)?;
            let id = unsafe { get_device_id(&device) }?;
            let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None) }?;
            let sessions = unsafe { manager.GetSessionEnumerator() }?;
            let count = unsafe { sessions.GetCount() }?;
            let mut values = Vec::new();
            for index in 0..count {
                let result = (|| -> anyhow::Result<Value> {
                    let control = unsafe { sessions.GetSession(index) }
                        .context("Session disappeared during enumeration")?;
                    session_data(&control, &id)
                })();
                values.push(match result {
                    Ok(value) => value,
                    Err(error) => serde_json::json!({"device_id":id,"index":index,"error":format!("{error:#}")}),
                });
            }
            Ok(Value::Array(values))
        })
    }

    pub fn session(
        device_id: Option<&str>,
        session_id: &str,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> anyhow::Result<Value> {
        with_com(|| {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
            let device = output_device(&enumerator, device_id)?;
            let id = unsafe { get_device_id(&device) }?;
            let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None) }?;
            let sessions = unsafe { manager.GetSessionEnumerator() }?;
            let count = unsafe { sessions.GetCount() }?;
            for index in 0..count {
                let control = unsafe { sessions.GetSession(index) }
                    .context("Session enumeration changed; retry")?;
                let control2: IAudioSessionControl2 = control.cast()?;
                let current_id =
                    unsafe { pwstr_to_string_and_free(control2.GetSessionInstanceIdentifier()?) }?;
                if current_id != session_id {
                    continue;
                }
                let simple: ISimpleAudioVolume = control.cast()?;
                if let Some(value) = volume {
                    unsafe { simple.SetMasterVolume(value / 100.0, core::ptr::null()) }
                        .context("Could not set session volume")?;
                }
                if let Some(value) = mute {
                    unsafe { simple.SetMute(value, core::ptr::null()) }
                        .context("Could not set session mute")?;
                }
                return session_data(&control, &id);
            }
            anyhow::bail!(
                "Audio session not found; re-enumerate sessions after the application starts playback"
            )
        })
    }
}

// ============================================================================
// Unit Tests (Invalid Inputs & Non-Windows Fallback only)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_flow_rejected() {
        let res = endpoint(None, "invalid_flow", None, None);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("Invalid flow"));
    }

    #[test]
    fn test_invalid_volume_rejected() {
        assert!(endpoint(None, "output", Some(-0.01), None).is_err());
        assert!(endpoint(None, "output", Some(100.01), None).is_err());
        assert!(endpoint(None, "output", Some(f32::NAN), None).is_err());
        assert!(endpoint(None, "output", Some(f32::INFINITY), None).is_err());
        assert!(endpoint(None, "output", Some(f32::NEG_INFINITY), None).is_err());

        assert!(session(None, "test-session", Some(-10.0), None).is_err());
        assert!(session(None, "test-session", Some(120.0), None).is_err());
        assert!(session(None, "test-session", Some(f32::NAN), None).is_err());
    }

    #[test]
    fn test_embedded_nul_rejected() {
        assert!(endpoint(Some("dev\0ice"), "output", None, None).is_err());
        assert!(sessions(Some("dev\0ice")).is_err());
        assert!(session(Some("dev\0ice"), "test-session", None, None).is_err());
        assert!(session(None, "sess\0ion", None, None).is_err());
    }

    #[test]
    #[cfg(not(windows))]
    fn test_non_windows_fallback() {
        assert!(endpoints().is_err());
        assert!(endpoint(None, "output", None, None).is_err());
        assert!(sessions(None).is_err());
        assert!(session(None, "id", None, None).is_err());
    }
}
