#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::Mutex;
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

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Clone, Serialize)]
struct MonitorInfo {
    name: String,
    is_primary: bool,
    label: String,
}

#[derive(Clone, Serialize)]
struct AppState {
    monitors: Vec<MonitorInfo>,
    selected_monitors: Vec<String>,
    is_active: bool,
    gamma: f32,
    brightness: f32,
    contrast: f32,
    toggle_key: i32,
    auto_key: i32,
    auto_mode: bool,
    target_process: String,
}

#[derive(Deserialize)]
struct Settings {
    gamma: f32,
    brightness: f32,
    contrast: f32,
    selected_monitors: Vec<String>,
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
}

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

fn save_original_ramps(original_ramps: &mut HashMap<String, GammaRamp>, devices: &[String]) {
    for name in devices {
        if original_ramps.contains_key(name) {
            continue;
        }
        unsafe {
            let wide = to_wide(name);
            let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
            if !hdc.is_null() {
                let mut current: GammaRamp = [0; 768];
                if GetDeviceGammaRamp(hdc, current.as_mut_ptr() as *mut _) != 0 {
                    original_ramps.insert(name.clone(), current);
                }
                DeleteDC(hdc);
            }
        }
    }
}

fn apply_to_monitors(
    ramp: &GammaRamp,
    selected: &[String],
    original_ramps: &mut HashMap<String, GammaRamp>,
) {
    let all_monitors = enumerate_monitors();
    let devices: Vec<String> = if selected.is_empty() {
        all_monitors.iter().map(|m| m.name.clone()).collect()
    } else {
        selected.to_vec()
    };

    save_original_ramps(original_ramps, &devices);

    for info in &all_monitors {
        if devices.contains(&info.name) {
            apply_ramp_to_device(ramp, &info.name);
        }
    }
}

fn restore_originals(
    selected: &[String],
    original_ramps: &HashMap<String, GammaRamp>,
) {
    let all_monitors = enumerate_monitors();
    let devices: Vec<String> = if selected.is_empty() {
        all_monitors.iter().map(|m| m.name.clone()).collect()
    } else {
        selected.to_vec()
    };

    for info in &all_monitors {
        if devices.contains(&info.name) {
            if let Some(original) = original_ramps.get(&info.name) {
                apply_ramp_to_device(original, &info.name);
            }
        }
    }
}

#[tauri::command]
fn get_monitors() -> Vec<MonitorInfo> {
    enumerate_monitors()
}

#[tauri::command]
fn get_state(data: State<Mutex<AppData>>) -> AppState {
    let d = data.lock().unwrap();
    AppState {
        monitors: enumerate_monitors(),
        selected_monitors: Vec::new(),
        is_active: d.is_active,
        gamma: 1.0,
        brightness: 0.5,
        contrast: 0.5,
        toggle_key: d.toggle_key,
        auto_key: d.auto_key,
        auto_mode: false,
        target_process: String::new(),
    }
}

#[tauri::command]
fn apply_settings(settings: Settings, data: State<Mutex<AppData>>) -> bool {
    let mut d = data.lock().unwrap();
    d.cached_ramp = calculate_ramp(settings.gamma, settings.brightness, settings.contrast);
    d.toggle_key = 0x78;
    d.auto_key = 0x79;

    if d.is_active || d.is_fading {
        let from = if d.is_fading {
            d.fade_from.unwrap_or(d.cached_ramp)
        } else {
            d.cached_ramp
        };
        d.fade_from = Some(from);
        d.fade_to = Some(d.cached_ramp);
        d.fade_step = 0;
        d.is_fading = true;
    }

    d.is_active
}

#[tauri::command]
fn toggle(data: State<Mutex<AppData>>) -> bool {
    let mut d = data.lock().unwrap();
    if d.is_active {
        deactivate_internal(&mut d, &Vec::new());
        false
    } else {
        activate_internal(&mut d, &Vec::new());
        true
    }
}

#[tauri::command]
fn set_active(active: bool, settings: Settings, data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    d.cached_ramp = calculate_ramp(settings.gamma, settings.brightness, settings.contrast);
    if active {
        activate_internal(&mut d, &settings.selected_monitors);
    } else {
        deactivate_internal(&mut d, &settings.selected_monitors);
    }
}

#[tauri::command]
fn check_hotkeys(data: State<Mutex<AppData>>) -> Option<bool> {
    let mut d = data.lock().unwrap();
    let toggle_down = unsafe { GetAsyncKeyState(d.toggle_key) } as u16 & 0x8000 != 0;
    let auto_down = unsafe { GetAsyncKeyState(d.auto_key) } as u16 & 0x8000 != 0;

    let mut result = None;
    if toggle_down && !d.last_toggle {
        d.last_toggle = toggle_down;
        if d.is_active {
            deactivate_internal(&mut d, &Vec::new());
            result = Some(false);
        } else {
            activate_internal(&mut d, &Vec::new());
            result = Some(true);
        }
    } else {
        d.last_toggle = toggle_down;
    }

    if auto_down && !d.last_auto {
        d.last_auto = auto_down;
    } else {
        d.last_auto = auto_down;
    }

    result
}

#[tauri::command]
fn check_auto_mode(target: String) -> Option<bool> {
    let proc = get_foreground_process();
    if proc.eq_ignore_ascii_case(&target) {
        Some(true)
    } else {
        Some(false)
    }
}

fn activate_internal(d: &mut AppData, selected: &[String]) {
    if d.is_active {
        return;
    }
    let from = d.original_ramps.values().next().copied().unwrap_or([0u16; 768]);
    if from[0] == 0 && from[255] == 0 {
        save_original_ramps(&mut d.original_ramps, selected);
        let from = d.original_ramps.values().next().copied().unwrap_or([0u16; 768]);
        d.fade_from = Some(from);
    } else {
        d.fade_from = Some(from);
    }
    d.fade_to = Some(d.cached_ramp);
    d.fade_step = 0;
    d.is_fading = true;
    d.is_active = true;
}

fn deactivate_internal(d: &mut AppData, selected: &[String]) {
    if !d.is_active {
        return;
    }
    let current = if d.is_fading {
        match (&d.fade_from, &d.fade_to) {
            (Some(from), Some(to)) => lerp_ramp(from, to, d.fade_step as f32 / FADE_STEPS as f32),
            _ => d.cached_ramp,
        }
    } else {
        d.cached_ramp
    };

    save_original_ramps(&mut d.original_ramps, selected);

    if let Some(original) = d.original_ramps.values().next().copied() {
        d.fade_from = Some(current);
        d.fade_to = Some(original);
        d.fade_step = 0;
        d.is_fading = true;
    } else {
        restore_originals(selected, &d.original_ramps);
    }
    d.is_active = false;
}

#[tauri::command]
fn tick_fade(data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    if !d.is_fading {
        return;
    }

    if d.fade_step >= FADE_STEPS {
        if let Some(to) = d.fade_to {
            apply_to_monitors(&to, &Vec::new(), &mut d.original_ramps);
        }
        d.fade_from = None;
        d.fade_to = None;
        d.fade_step = 0;
        d.is_fading = false;
        return;
    }

    let t = d.fade_step as f32 / FADE_STEPS as f32;
    let ramp = lerp_ramp(
        &d.fade_from.unwrap_or(d.cached_ramp),
        &d.fade_to.unwrap_or(d.cached_ramp),
        t,
    );
    apply_to_monitors(&ramp, &Vec::new(), &mut d.original_ramps);
    d.fade_step += 1;
}

#[tauri::command]
fn restore_all(data: State<Mutex<AppData>>) {
    let mut d = data.lock().unwrap();
    restore_originals(&Vec::new(), &d.original_ramps);
    d.is_active = false;
    d.is_fading = false;
    d.fade_from = None;
    d.fade_to = None;
}

fn main() {
    tauri::Builder::default()
        .manage(Mutex::new(AppData::new()))
        .invoke_handler(tauri::generate_handler![
            get_monitors,
            get_state,
            apply_settings,
            toggle,
            set_active,
            check_hotkeys,
            check_auto_mode,
            tick_fade,
            restore_all,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
