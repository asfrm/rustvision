#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::Mutex;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use winapi::shared::minwindef::DWORD;
use winapi::um::handleapi::CloseHandle;
use winapi::um::processthreadsapi::OpenProcess;
use winapi::um::psapi::GetProcessImageFileNameW;
use winapi::um::wingdi::{
    CreateDCW, DeleteDC, GetDeviceGammaRamp, SetDeviceGammaRamp, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE,
};
use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
use winapi::um::winuser::{
    EnumDisplayDevicesW, GetAsyncKeyState, GetForegroundWindow, GetWindowThreadProcessId,
};

type GammaRamp = [u16; 768];
const FADE_STEPS: u32 = 40;
const TICK_MS: u64 = 50;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Clone, Serialize)]
struct MonitorInfo {
    name: String,
    is_primary: bool,
    label: String,
}

#[derive(Deserialize)]
struct ApplySettings {
    gamma: f32,
    brightness: f32,
    contrast: f32,
    selected_monitors: Vec<String>,
}

#[derive(Clone, Copy)]
enum KeyCapture {
    None,
    Toggle,
    Auto,
}

struct AppData {
    original_ramps: HashMap<String, GammaRamp>,
    cached_ramp: GammaRamp,
    fade_from: Option<GammaRamp>,
    fade_to: Option<GammaRamp>,
    fade_step: u32,
    is_fading: bool,
    is_active: bool,
    toggle_key: i32,
    auto_key: i32,
    last_toggle: bool,
    last_auto: bool,
    selected_monitors: Vec<String>,
    auto_mode: bool,
    target_process: String,
    last_auto_state: Option<bool>,
    key_capture: KeyCapture,
}

#[allow(dead_code)]
impl AppData {
    fn new() -> Self {
        Self {
            original_ramps: HashMap::new(),
            cached_ramp: calculate_ramp(1.0, 0.5, 0.5),
            fade_from: None,
            fade_to: None,
            fade_step: 0,
            is_fading: false,
            is_active: false,
            toggle_key: 0x78,
            auto_key: 0x79,
            last_toggle: false,
            last_auto: false,
            selected_monitors: Vec::new(),
            auto_mode: false,
            target_process: "RustClient.exe".to_string(),
            last_auto_state: None,
            key_capture: KeyCapture::None,
        }
    }
}

fn calculate_ramp(gamma: f32, brightness: f32, contrast: f32) -> GammaRamp {
    let mut ramp: GammaRamp = [0; 768];
    let contrast_factor = (contrast + 0.5).powf(2.0);

    for i in 0..256 {
        let mut val = (i as f32 / 255.0 - 0.5) * contrast_factor + 0.5;
        val += brightness - 0.5;

        if val > 0.0 {
            val = val.powf(1.0 / gamma.max(0.01));
        }

        let word = (val.clamp(0.0, 1.0) * 65535.0) as u16;
        ramp[i] = word;
        ramp[i + 256] = word;
        ramp[i + 512] = word;
    }
    ramp
}

fn lerp_ramp(from: &GammaRamp, to: &GammaRamp, t: f32) -> GammaRamp {
    let mut result: GammaRamp = [0; 768];
    for i in 0..768 {
        result[i] = (from[i] as f32 + (to[i] as f32 - from[i] as f32) * t) as u16;
    }
    result
}

fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut monitors = Vec::new();
    let mut dev_num = 0;
    unsafe {
        let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
        device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;

        while EnumDisplayDevicesW(null_mut(), dev_num, &mut device, 0) != 0 {
            if (device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) != 0 {
                let name = String::from_utf16_lossy(&device.DeviceName)
                    .trim_end_matches('\0')
                    .to_string();
                let is_primary = (device.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
                let label = if is_primary {
                    format!("Monitor {} (Primary)", dev_num + 1)
                } else {
                    format!("Monitor {}", dev_num + 1)
                };
                monitors.push(MonitorInfo {
                    name,
                    is_primary,
                    label,
                });
            }
            dev_num += 1;
        }
    }
    if monitors.is_empty() {
        monitors.push(MonitorInfo {
            name: "DISPLAY".to_string(),
            is_primary: true,
            label: "Default Display".to_string(),
        });
    }
    monitors
}

fn get_foreground_process() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return String::new();
        }
        let mut pid: DWORD = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !handle.is_null() {
            let mut buf = [0u16; 512];
            let len = GetProcessImageFileNameW(handle, buf.as_mut_ptr(), 512);
            CloseHandle(handle);
            if len > 0 {
                return String::from_utf16_lossy(&buf[..len as usize])
                    .split('\\')
                    .last()
                    .unwrap_or_default()
                    .to_string();
            }
        }
        String::new()
    }
}

fn apply_ramp_to_device(ramp: &GammaRamp, device_name: &str) {
    unsafe {
        let wide = to_wide(device_name);
        let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
        if !hdc.is_null() {
            SetDeviceGammaRamp(hdc, ramp.as_ptr() as *mut _);
            DeleteDC(hdc);
        }
    }
}

fn ensure_originals(d: &mut AppData) {
    if !d.original_ramps.is_empty() {
        return;
    }
    let monitors = enumerate_monitors();
    for info in &monitors {
        unsafe {
            let wide = to_wide(&info.name);
            let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
            if !hdc.is_null() {
                let mut current: GammaRamp = [0; 768];
                if GetDeviceGammaRamp(hdc, current.as_mut_ptr() as *mut _) != 0 {
                    d.original_ramps.insert(info.name.clone(), current);
                }
                DeleteDC(hdc);
            }
        }
    }
}

fn get_devices(d: &AppData) -> Vec<String> {
    if d.selected_monitors.is_empty() {
        enumerate_monitors().into_iter().map(|m| m.name).collect()
    } else {
        d.selected_monitors.clone()
    }
}

fn activate_internal(d: &mut AppData, app: &tauri::AppHandle) {
    if d.is_active {
        return;
    }
    ensure_originals(d);

    let from = d.original_ramps.values().next().copied().unwrap_or(calculate_ramp(1.0, 0.5, 0.5));
    d.fade_from = Some(from);
    d.fade_to = Some(d.cached_ramp);
    d.fade_step = 0;
    d.is_fading = true;
    d.is_active = true;

    let _ = app.emit("status_change", true);
}

fn deactivate_internal(d: &mut AppData, app: &tauri::AppHandle) {
    if !d.is_active {
        return;
    }
    ensure_originals(d);

    let current = if d.is_fading {
        match (&d.fade_from, &d.fade_to) {
            (Some(from), Some(to)) => lerp_ramp(from, to, d.fade_step as f32 / FADE_STEPS as f32),
            _ => d.cached_ramp,
        }
    } else {
        d.cached_ramp
    };

    if let Some(original) = d.original_ramps.values().next().copied() {
        d.fade_from = Some(current);
        d.fade_to = Some(original);
        d.fade_step = 0;
        d.is_fading = true;
    } else {
        let devices = get_devices(d);
        for name in &devices {
            if let Some(orig) = d.original_ramps.get(name) {
                apply_ramp_to_device(orig, name);
            }
        }
    }
    d.is_active = false;

    let _ = app.emit("status_change", false);
}

fn tick(d: &mut AppData, app: &tauri::AppHandle) {
    // Key capture mode: scan all keys for binding
    if !matches!(d.key_capture, KeyCapture::None) {
        for code in 8..255 {
            if unsafe { GetAsyncKeyState(code) } as u16 & 0x8000 != 0 {
                match d.key_capture {
                    KeyCapture::Toggle => {
                        d.toggle_key = code;
                        let _ = app.emit("key_captured", serde_json::json!({"target": "toggle", "code": code}));
                    }
                    KeyCapture::Auto => {
                        d.auto_key = code;
                        let _ = app.emit("key_captured", serde_json::json!({"target": "auto", "code": code}));
                    }
                    KeyCapture::None => {}
                }
                d.key_capture = KeyCapture::None;
                break;
            }
        }
        return; // skip hotkey checks while capturing
    }

    // Fade
    if d.is_fading {
        if d.fade_step >= FADE_STEPS {
            if let Some(to) = d.fade_to {
                let devices = get_devices(d);
                for name in &devices {
                    apply_ramp_to_device(&to, name);
                }
            }
            d.fade_from = None;
            d.fade_to = None;
            d.fade_step = 0;
            d.is_fading = false;
        } else {
            let t = d.fade_step as f32 / FADE_STEPS as f32;
            let ramp = lerp_ramp(
                &d.fade_from.unwrap_or(d.cached_ramp),
                &d.fade_to.unwrap_or(d.cached_ramp),
                t,
            );
            let devices = get_devices(d);
            for name in &devices {
                apply_ramp_to_device(&ramp, name);
            }
            d.fade_step += 1;
        }
    }

    // Toggle hotkey
    let toggle_down = unsafe { GetAsyncKeyState(d.toggle_key) } as u16 & 0x8000 != 0;
    if toggle_down && !d.last_toggle {
        if d.is_active {
            deactivate_internal(d, app);
        } else {
            activate_internal(d, app);
        }
    }
    d.last_toggle = toggle_down;

    // Auto hotkey
    let auto_down = unsafe { GetAsyncKeyState(d.auto_key) } as u16 & 0x8000 != 0;
    if auto_down && !d.last_auto {
        d.auto_mode = !d.auto_mode;
        if !d.auto_mode && d.is_active {
            deactivate_internal(d, app);
        }
        let _ = app.emit("auto_mode_change", d.auto_mode);
    }
    d.last_auto = auto_down;

    // Auto mode: check foreground process
    if d.auto_mode {
        let proc = get_foreground_process();
        let is_target = !proc.is_empty() && proc.eq_ignore_ascii_case(&d.target_process);
        if d.last_auto_state != Some(is_target) {
            d.last_auto_state = Some(is_target);
            if is_target {
                activate_internal(d, app);
            } else {
                deactivate_internal(d, app);
            }
        }
    }
}

#[tauri::command]
fn get_monitors() -> Vec<MonitorInfo> {
    enumerate_monitors()
}

#[tauri::command]
fn apply_settings(settings: ApplySettings, data: State<Mutex<AppData>>, _app: tauri::AppHandle) {
    let mut d = data.lock().unwrap();
    d.selected_monitors = settings.selected_monitors;
    d.cached_ramp = calculate_ramp(settings.gamma, settings.brightness, settings.contrast);
    if d.is_active {
        let current = if d.is_fading {
            match (&d.fade_from, &d.fade_to) {
                (Some(from), Some(to)) => lerp_ramp(from, to, d.fade_step as f32 / FADE_STEPS as f32),
                _ => d.cached_ramp,
            }
        } else {
            d.cached_ramp
        };
        d.fade_from = Some(current);
        d.fade_to = Some(d.cached_ramp);
        d.fade_step = 0;
        d.is_fading = true;
    }
}

#[tauri::command]
fn start_key_capture(target: String, data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    d.key_capture = match target.as_str() {
        "toggle" => KeyCapture::Toggle,
        "auto" => KeyCapture::Auto,
        _ => KeyCapture::None,
    };
}

#[tauri::command]
fn cancel_key_capture(data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    d.key_capture = KeyCapture::None;
}

#[tauri::command]
fn set_auto_mode(enabled: bool, data: State<Mutex<AppData>>, app: tauri::AppHandle) {
    let mut d = data.lock().unwrap();
    d.auto_mode = enabled;
    if !enabled && d.is_active {
        deactivate_internal(&mut d, &app);
    }
    d.last_auto_state = None;
}

#[tauri::command]
fn set_target_process(target: String, data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    d.target_process = target;
}

#[tauri::command]
fn toggle(data: State<Mutex<AppData>>, app: tauri::AppHandle) {
    let mut d = data.lock().unwrap();
    if d.is_active {
        deactivate_internal(&mut d, &app);
    } else {
        activate_internal(&mut d, &app);
    }
}

#[tauri::command]
fn restore_all(data: State<Mutex<AppData>>, app: tauri::AppHandle) {
    let mut d = data.lock().unwrap();
    let devices = get_devices(&d);
    for name in &devices {
        if let Some(orig) = d.original_ramps.get(name) {
            apply_ramp_to_device(orig, name);
        }
    }
    d.is_active = false;
    d.is_fading = false;
    d.fade_from = None;
    d.fade_to = None;
    let _ = app.emit("status_change", false);
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
                if let Ok(mut d) = handle.state::<Mutex<AppData>>().try_lock() {
                    tick(&mut d, &handle);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_monitors,
            apply_settings,
            start_key_capture,
            cancel_key_capture,
            set_auto_mode,
            set_target_process,
            toggle,
            restore_all,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
