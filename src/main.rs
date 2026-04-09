#![windows_subsystem = "windows"]

use eframe::egui;
use std::collections::HashMap;
use std::ptr::null_mut;
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
    GetAsyncKeyState, GetForegroundWindow, GetWindowThreadProcessId, EnumDisplayDevicesW,
    GetMonitorInfoW, MonitorFromWindow, MONITORINFOEXW, MONITOR_DEFAULTTONULL,
};

type GammaRamp = [u16; 768];

const FADE_STEP: f32 = 0.025;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Clone, Copy, PartialEq)]
struct DisplaySettings {
    gamma: f32,
    brightness: f32,
    contrast: f32,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            gamma: 1.0,
            brightness: 0.5,
            contrast: 0.5,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum KeyTarget {
    Toggle,
    Auto,
}

#[derive(Clone, Copy, PartialEq)]
enum MonitorTarget {
    Primary,
    All,
    Active,
}

struct AppState {
    settings: DisplaySettings,
    is_active: bool,
    original_ramps: HashMap<String, GammaRamp>,
    cached_ramp: GammaRamp,
    monitor_target: MonitorTarget,
    fade_from: Option<GammaRamp>,
    fade_to: Option<GammaRamp>,
    fade_progress: f32,
    toggle_key: i32,
    auto_key: i32,
    waiting_for_key: Option<KeyTarget>,
    last_toggle_state: bool,
    last_auto_state: bool,
    auto_mode: bool,
    target_process: String,
    last_check: f64,
}

impl Default for AppState {
    fn default() -> Self {
        let settings = DisplaySettings::default();
        Self {
            settings,
            is_active: false,
            original_ramps: HashMap::new(),
            cached_ramp: calculate_ramp(&settings),
            monitor_target: MonitorTarget::Primary,
            fade_from: None,
            fade_to: None,
            fade_progress: 0.0,
            toggle_key: 0x78,
            auto_key: 0x79,
            waiting_for_key: None,
            last_toggle_state: false,
            last_auto_state: false,
            auto_mode: false,
            target_process: "RustClient.exe".to_string(),
            last_check: 0.0,
        }
    }
}

fn calculate_ramp(settings: &DisplaySettings) -> GammaRamp {
    let mut ramp: GammaRamp = [0; 768];
    let contrast_factor = (settings.contrast + 0.5).powf(2.0);

    for i in 0..256 {
        let mut val = (i as f32 / 255.0 - 0.5) * contrast_factor + 0.5;
        val += settings.brightness - 0.5;

        if val > 0.0 {
            val = val.powf(1.0 / settings.gamma.max(0.01));
        }

        let word = val.clamp(0.0, 1.0) * 65535.0;
        ramp[i] = word as u16;
        ramp[i + 256] = ramp[i];
        ramp[i + 512] = ramp[i];
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

impl AppState {
    fn get_active_monitor_device(&self) -> String {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return String::new();
            }
            let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
            if hmonitor.is_null() {
                return String::new();
            }
            let mut info: MONITORINFOEXW = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFOEXW as *mut _) != 0 {
                String::from_utf16_lossy(&info.szDevice)
                    .trim_end_matches('\0')
                    .to_string()
            } else {
                String::new()
            }
        }
    }

    fn enumerate_monitors(&self) -> Vec<(String, bool)> {
        let mut monitors = Vec::new();
        unsafe {
            let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let mut dev_num = 0;

            while EnumDisplayDevicesW(null_mut(), dev_num, &mut device, 0) != 0 {
                if (device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) != 0 {
                    let name = String::from_utf16_lossy(&device.DeviceName)
                        .trim_end_matches('\0')
                        .to_string();
                    let is_primary = (device.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) != 0;
                    monitors.push((name, is_primary));
                }
                dev_num += 1;
            }
        }
        monitors
    }

    fn should_apply_to_device(&self, device_name: &str, is_primary: bool) -> bool {
        match self.monitor_target {
            MonitorTarget::Primary => is_primary,
            MonitorTarget::All => true,
            MonitorTarget::Active => {
                let active = self.get_active_monitor_device();
                !active.is_empty() && device_name == active
            }
        }
    }

    fn save_original_ramps(&mut self) {
        for (name, _) in self.enumerate_monitors() {
            if self.original_ramps.contains_key(&name) {
                continue;
            }
            unsafe {
                let wide = to_wide(&name);
                let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
                if !hdc.is_null() {
                    let mut current: GammaRamp = [0; 768];
                    if GetDeviceGammaRamp(hdc, current.as_mut_ptr() as *mut _) != 0 {
                        self.original_ramps.insert(name, current);
                    }
                    DeleteDC(hdc);
                }
            }
        }
    }

    fn apply_ramp_to_device(&self, ramp: &GammaRamp, device_name: &str) {
        unsafe {
            let wide = to_wide(device_name);
            let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
            if !hdc.is_null() {
                SetDeviceGammaRamp(hdc, ramp.as_ptr() as *mut _);
                DeleteDC(hdc);
            }
        }
    }

    fn apply_ramp(&self, ramp: &GammaRamp) {
        for (name, is_primary) in self.enumerate_monitors() {
            if self.should_apply_to_device(&name, is_primary) {
                self.apply_ramp_to_device(ramp, &name);
            }
        }
    }

    fn restore_original_ramps(&self) {
        for (name, is_primary) in self.enumerate_monitors() {
            if self.should_apply_to_device(&name, is_primary) {
                if let Some(original) = self.original_ramps.get(&name) {
                    self.apply_ramp_to_device(original, &name);
                }
            }
        }
    }

    fn start_fade(&mut self, from: GammaRamp, to: GammaRamp) {
        self.fade_from = Some(from);
        self.fade_to = Some(to);
        self.fade_progress = 0.0;
    }

    fn tick_fade(&mut self) {
        if let (Some(from), Some(to)) = (&self.fade_from, &self.fade_to) {
            if self.fade_progress >= 1.0 {
                self.apply_ramp(to);
                self.fade_from = None;
                self.fade_to = None;
                self.fade_progress = 0.0;
                return;
            }
            let ramp = lerp_ramp(from, to, self.fade_progress);
            self.apply_ramp(&ramp);
            self.fade_progress += FADE_STEP;
        }
    }

    fn activate(&mut self) {
        if self.is_active {
            return;
        }
        self.save_original_ramps();

        let from = self.original_ramps.values().next().copied().unwrap_or_else(|| {
            calculate_ramp(&DisplaySettings::default())
        });

        self.start_fade(from, self.cached_ramp);
        self.is_active = true;
    }

    fn deactivate(&mut self) {
        if !self.is_active {
            return;
        }

        let current = match (&self.fade_from, &self.fade_to) {
            (Some(from), Some(to)) => {
                lerp_ramp(from, to, self.fade_progress.min(1.0))
            }
            _ => self.cached_ramp,
        };

        self.save_original_ramps();

        if let Some(original) = self.original_ramps.values().next().copied() {
            self.start_fade(current, original);
        } else {
            self.restore_original_ramps();
        }
        self.is_active = false;
    }

    fn refresh_ramp(&mut self) {
        self.cached_ramp = calculate_ramp(&self.settings);
        if self.is_active {
            let current = match (&self.fade_from, &self.fade_to) {
                (Some(from), Some(to)) => {
                    lerp_ramp(from, to, self.fade_progress.min(1.0))
                }
                _ => self.cached_ramp,
            };
            self.start_fade(current, self.cached_ramp);
        }
    }

    fn reset(&mut self) {
        self.settings = DisplaySettings::default();
        self.refresh_ramp();
    }

    fn get_foreground_process(&self) -> String {
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

    fn format_key(key: i32) -> String {
        match key {
            0x70..=0x87 => format!("F{}", key - 0x6F),
            0x08 => "BACK".into(),
            0x20 => "SPACE".into(),
            k if (0x30..=0x39).contains(&k) => ((k as u8) as char).to_string(),
            k if (0x41..=0x5A).contains(&k) => ((k as u8) as char).to_string(),
            _ => format!("0x{:X}", key),
        }
    }

    fn key_binding_button(&mut self, ui: &mut egui::Ui, target: KeyTarget) {
        ui.horizontal(|ui| {
            let label = match target {
                KeyTarget::Toggle => "Бинд вкл/выкл:",
                KeyTarget::Auto => "Бинд:",
            };
            ui.label(label);
            let btn_label = match self.waiting_for_key {
                Some(t) if t == target => "...".into(),
                _ => match target {
                    KeyTarget::Toggle => Self::format_key(self.toggle_key),
                    KeyTarget::Auto => Self::format_key(self.auto_key),
                },
            };
            if ui.button(btn_label).clicked() {
                self.waiting_for_key = Some(target);
            }
        });
    }
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = ctx.input(|i| i.time);

        if let Some(target) = &self.waiting_for_key {
            for key_code in 8..255 {
                if unsafe { GetAsyncKeyState(key_code) } as u16 & 0x8000 != 0 {
                    match target {
                        KeyTarget::Toggle => self.toggle_key = key_code,
                        KeyTarget::Auto => self.auto_key = key_code,
                    }
                    self.waiting_for_key = None;
                    break;
                }
            }
        } else {
            let toggle_down =
                unsafe { GetAsyncKeyState(self.toggle_key) } as u16 & 0x8000 != 0;
            let auto_down = unsafe { GetAsyncKeyState(self.auto_key) } as u16 & 0x8000 != 0;

            if toggle_down && !self.last_toggle_state && !self.auto_mode {
                if self.is_active {
                    self.deactivate();
                } else {
                    self.activate();
                }
            }
            if auto_down && !self.last_auto_state {
                self.auto_mode = !self.auto_mode;
                if !self.auto_mode {
                    self.deactivate();
                }
            }
            self.last_toggle_state = toggle_down;
            self.last_auto_state = auto_down;
        }

        if self.auto_mode && now - self.last_check > 0.3 {
            let proc = self.get_foreground_process();
            let is_target = proc.eq_ignore_ascii_case(&self.target_process);
            if is_target {
                self.activate();
            } else {
                self.deactivate();
            }
            self.last_check = now;
        }

        self.tick_fade();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("RustVision");
            });
            ui.add_space(8.0);

            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let cb = ui.checkbox(&mut self.auto_mode, "Авто-режим");
                    if cb.changed() && !self.auto_mode {
                        self.deactivate();
                    }
                    self.key_binding_button(ui, KeyTarget::Auto);
                });
                ui.horizontal(|ui| {
                    ui.label("Процесс:");
                    ui.text_edit_singleline(&mut self.target_process);
                });
            });

            ui.group(|ui| {
                ui.set_enabled(!self.auto_mode);
                self.key_binding_button(ui, KeyTarget::Toggle);
            });

            ui.add_space(10.0);

            ui.group(|ui| {
                ui.label("Монитор:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.monitor_target, MonitorTarget::Primary, "Основной");
                    ui.radio_value(&mut self.monitor_target, MonitorTarget::All, "Все");
                    ui.radio_value(&mut self.monitor_target, MonitorTarget::Active, "Активный");
                });
            });

            ui.add_space(10.0);

            let gamma_slider =
                ui.add(egui::Slider::new(&mut self.settings.gamma, 0.1..=10.0).text("Гамма"));
            let brightness_slider =
                ui.add(egui::Slider::new(&mut self.settings.brightness, 0.0..=1.0).text("Яркость"));
            let contrast_slider =
                ui.add(egui::Slider::new(&mut self.settings.contrast, 0.0..=1.0).text("Контрастность"));

            if gamma_slider.changed() || brightness_slider.changed() || contrast_slider.changed() {
                self.refresh_ramp();
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Сброс").clicked() {
                    self.reset();
                }
                let (status, color) = if self.is_active {
                    ("Работаю..", egui::Color32::LIGHT_GREEN)
                } else {
                    ("Жду..", egui::Color32::DARK_GRAY)
                };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(status).color(color).strong());
                });
            });
        });
        ctx.request_repaint();
    }
}

fn load_icon() -> Option<egui::IconData> {
    let icon_bytes = include_bytes!("../icon.ico");
    let img = image::load_from_memory(icon_bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn main() -> Result<(), eframe::Error> {
    let icon_data = load_icon().unwrap_or_default();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([370.0, 480.0])
            .with_resizable(false)
            .with_icon(icon_data),
        ..Default::default()
    };

    eframe::run_native(
        "RustVision",
        options,
        Box::new(|_cc| Box::new(AppState::default())),
    )
}
