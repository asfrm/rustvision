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
    EnumDisplayDevicesW, GetAsyncKeyState, GetForegroundWindow, GetWindowThreadProcessId,
};

type GammaRamp = [u16; 768];
const FADE_STEPS: f32 = 40.0;

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct MonitorInfo {
    name: String,
    is_primary: bool,
    label: String,
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

#[derive(PartialEq)]
enum Lang {
    En,
    Ru,
}

struct I18n;

impl I18n {
    fn tr<'a>(lang: &'a Lang, key: &'a str) -> &'a str {
        match (lang, key) {
            (Lang::En, "autoMode") => "Auto mode",
            (Lang::Ru, "autoMode") => "Авто-режим",
            (Lang::En, "bind") => "Bind:",
            (Lang::Ru, "bind") => "Бинд:",
            (Lang::En, "process") => "Process:",
            (Lang::Ru, "process") => "Процесс:",
            (Lang::En, "toggleBind") => "Toggle bind:",
            (Lang::Ru, "toggleBind") => "Бинд вкл/выкл:",
            (Lang::En, "monitors") => "Monitors:",
            (Lang::Ru, "monitors") => "Мониторы:",
            (Lang::En, "gamma") => "Gamma",
            (Lang::Ru, "gamma") => "Гамма",
            (Lang::En, "brightness") => "Brightness",
            (Lang::Ru, "brightness") => "Яркость",
            (Lang::En, "contrast") => "Contrast",
            (Lang::Ru, "contrast") => "Контрастность",
            (Lang::En, "reset") => "Reset",
            (Lang::Ru, "reset") => "Сброс",
            (Lang::En, "waiting") => "Waiting..",
            (Lang::Ru, "waiting") => "Жду..",
            (Lang::En, "active") => "Active..",
            (Lang::Ru, "active") => "Работаю..",
            (Lang::En, "listening") => "...",
            (Lang::Ru, "listening") => "...",
            (Lang::En, "monitorAll") => "All",
            (Lang::Ru, "monitorAll") => "Все",
            _ => key,
        }
    }
}

struct AppState {
    settings: DisplaySettings,
    is_active: bool,
    original_ramps: HashMap<String, GammaRamp>,
    cached_ramp: GammaRamp,
    fade_from: Option<GammaRamp>,
    fade_to: Option<GammaRamp>,
    fade_progress: f32,
    monitors: Vec<MonitorInfo>,
    selected_monitors: Vec<bool>,
    select_all: bool,
    toggle_key: i32,
    auto_key: i32,
    waiting_for_key: Option<KeyTarget>,
    last_toggle_state: bool,
    last_auto_state: bool,
    auto_mode: bool,
    target_process: String,
    last_check: f64,
    lang: Lang,
}

impl Default for AppState {
    fn default() -> Self {
        let settings = DisplaySettings::default();
        let mut me = Self {
            settings,
            is_active: false,
            original_ramps: HashMap::new(),
            cached_ramp: calculate_ramp(&settings),
            fade_from: None,
            fade_to: None,
            fade_progress: 1.0,
            monitors: Vec::new(),
            selected_monitors: Vec::new(),
            select_all: true,
            toggle_key: 0x78,
            auto_key: 0x79,
            waiting_for_key: None,
            last_toggle_state: false,
            last_auto_state: false,
            auto_mode: false,
            target_process: "RustClient.exe".to_string(),
            last_check: 0.0,
            lang: Lang::En,
        };
        me.enumerate_monitors();
        me
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

impl AppState {
    fn enumerate_monitors(&mut self) {
        self.monitors.clear();
        self.selected_monitors.clear();
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
                    self.monitors.push(MonitorInfo { name, is_primary, label });
                    self.selected_monitors.push(is_primary);
                }
                dev_num += 1;
            }
        }
        if self.monitors.is_empty() {
            self.monitors.push(MonitorInfo {
                name: "DISPLAY".to_string(),
                is_primary: true,
                label: "Default Display".to_string(),
            });
            self.selected_monitors.push(true);
        }
    }

    fn get_selected_devices(&self) -> Vec<String> {
        if self.select_all {
            self.monitors.iter().map(|m| m.name.clone()).collect()
        } else {
            self.monitors
                .iter()
                .zip(self.selected_monitors.iter())
                .filter(|(_, &sel)| sel)
                .map(|(m, _)| m.name.clone())
                .collect()
        }
    }

    fn save_original_ramps(&mut self, devices: &[String]) {
        for name in devices {
            if self.original_ramps.contains_key(name) {
                continue;
            }
            unsafe {
                let wide = to_wide(name);
                let hdc = CreateDCW(null_mut(), wide.as_ptr(), null_mut(), null_mut());
                if !hdc.is_null() {
                    let mut current: GammaRamp = [0; 768];
                    if GetDeviceGammaRamp(hdc, current.as_mut_ptr() as *mut _) != 0 {
                        self.original_ramps.insert(name.clone(), current);
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

    fn apply_ramp(&self, ramp: &GammaRamp, devices: &[String]) {
        for info in &self.monitors {
            if devices.contains(&info.name) {
                self.apply_ramp_to_device(ramp, &info.name);
            }
        }
    }

    fn restore_originals(&self, devices: &[String]) {
        for info in &self.monitors {
            if devices.contains(&info.name) {
                if let Some(original) = self.original_ramps.get(&info.name) {
                    self.apply_ramp_to_device(original, &info.name);
                }
            }
        }
    }

    fn activate(&mut self) {
        if self.is_active {
            return;
        }
        let devices = self.get_selected_devices();
        self.save_original_ramps(&devices);

        let from = self.original_ramps.values().next().copied().unwrap_or(calculate_ramp(&DisplaySettings::default()));
        self.fade_from = Some(from);
        self.fade_to = Some(self.cached_ramp);
        self.fade_progress = 0.0;
        self.is_active = true;
    }

    fn deactivate(&mut self) {
        if !self.is_active {
            return;
        }
        let devices = self.get_selected_devices();

        let current = if self.fade_progress < 1.0 {
            match (&self.fade_from, &self.fade_to) {
                (Some(from), Some(to)) => lerp_ramp(from, to, self.fade_progress),
                _ => self.cached_ramp,
            }
        } else {
            self.cached_ramp
        };

        self.save_original_ramps(&devices);

        if let Some(original) = self.original_ramps.values().next().copied() {
            self.fade_from = Some(current);
            self.fade_to = Some(original);
            self.fade_progress = 0.0;
        } else {
            self.restore_originals(&devices);
            self.fade_from = None;
            self.fade_to = None;
            self.fade_progress = 1.0;
        }
        self.is_active = false;
    }

    fn refresh_ramp(&mut self) {
        self.cached_ramp = calculate_ramp(&self.settings);
        if self.is_active {
            let current = if self.fade_progress < 1.0 {
                match (&self.fade_from, &self.fade_to) {
                    (Some(from), Some(to)) => lerp_ramp(from, to, self.fade_progress),
                    _ => self.cached_ramp,
                }
            } else {
                self.cached_ramp
            };
            self.fade_from = Some(current);
            self.fade_to = Some(self.cached_ramp);
            self.fade_progress = 0.0;
        }
    }

    fn reset(&mut self) {
        self.settings = DisplaySettings::default();
        self.refresh_ramp();
    }

    fn tick_fade(&mut self) {
        if self.fade_progress >= 1.0 {
            return;
        }
        self.fade_progress += 1.0 / FADE_STEPS;
        if self.fade_progress >= 1.0 {
            self.fade_progress = 1.0;
            if let Some(to) = self.fade_to {
                let devices = self.get_selected_devices();
                self.apply_ramp(&to, &devices);
            }
        } else {
            let ramp = lerp_ramp(
                &self.fade_from.unwrap_or(self.cached_ramp),
                &self.fade_to.unwrap_or(self.cached_ramp),
                self.fade_progress,
            );
            let devices = self.get_selected_devices();
            self.apply_ramp(&ramp, &devices);
        }
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

    fn key_binding_button(&mut self, ui: &mut egui::Ui, target: KeyTarget) {
        ui.horizontal(|ui| {
            let label = match target {
                KeyTarget::Toggle => I18n::tr(&self.lang, "toggleBind"),
                KeyTarget::Auto => I18n::tr(&self.lang, "bind"),
            };
            ui.label(label);
            let btn_label = match self.waiting_for_key {
                Some(t) if t == target => I18n::tr(&self.lang, "listening").to_string(),
                _ => match target {
                    KeyTarget::Toggle => format_key(self.toggle_key),
                    KeyTarget::Auto => format_key(self.auto_key),
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
            let toggle_down = unsafe { GetAsyncKeyState(self.toggle_key) } as u16 & 0x8000 != 0;
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
                if !self.auto_mode && self.is_active {
                    self.deactivate();
                }
            }
            self.last_toggle_state = toggle_down;
            self.last_auto_state = auto_down;
        }

        if self.auto_mode && now - self.last_check > 0.3 {
            let proc = self.get_foreground_process();
            let is_target = !proc.is_empty() && proc.eq_ignore_ascii_case(&self.target_process);
            if is_target && !self.is_active {
                self.activate();
            } else if !is_target && self.is_active {
                self.deactivate();
            }
            self.last_check = now;
        }

        self.tick_fade();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.heading("RustVision");
            });
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.label("");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.lang == Lang::Ru, "RU").clicked() {
                        self.lang = Lang::Ru;
                    }
                    ui.add_space(4.0);
                    if ui.selectable_label(self.lang == Lang::En, "EN").clicked() {
                        self.lang = Lang::En;
                    }
                });
            });

            ui.add_space(4.0);

            ui.group(|ui| {
                ui.horizontal(|ui| {
                    let cb = ui.checkbox(&mut self.auto_mode, I18n::tr(&self.lang, "autoMode"));
                    if cb.changed() && !self.auto_mode && self.is_active {
                        self.deactivate();
                    }
                    self.key_binding_button(ui, KeyTarget::Auto);
                });
                ui.horizontal(|ui| {
                    ui.label(I18n::tr(&self.lang, "process"));
                    ui.text_edit_singleline(&mut self.target_process);
                });
            });

            ui.group(|ui| {
                ui.set_enabled(!self.auto_mode);
                self.key_binding_button(ui, KeyTarget::Toggle);
            });

            ui.add_space(6.0);

            ui.group(|ui| {
                ui.label(I18n::tr(&self.lang, "monitors"));

                let mut select_all = self.select_all;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut select_all, I18n::tr(&self.lang, "monitorAll")).changed() {
                        self.select_all = select_all;
                        if select_all && self.is_active {
                            self.deactivate();
                            self.activate();
                        }
                    }
                });

                if !self.select_all {
                    let labels: Vec<String> = self.monitors.iter().map(|m| m.label.clone()).collect();
                    let mut monitor_changed = false;
                    for (i, label) in labels.iter().enumerate() {
                        let r = ui.checkbox(&mut self.selected_monitors[i], label);
                        if r.changed() {
                            monitor_changed = true;
                        }
                    }
                    if monitor_changed && self.is_active {
                        self.deactivate();
                        self.activate();
                    }
                }
            });

            ui.add_space(6.0);

            let gamma_changed = ui.add(
                egui::Slider::new(&mut self.settings.gamma, 0.1..=10.0)
                    .text(I18n::tr(&self.lang, "gamma"))
            ).changed();

            let brightness_changed = ui.add(
                egui::Slider::new(&mut self.settings.brightness, 0.0..=1.0)
                    .text(I18n::tr(&self.lang, "brightness"))
            ).changed();

            let contrast_changed = ui.add(
                egui::Slider::new(&mut self.settings.contrast, 0.0..=1.0)
                    .text(I18n::tr(&self.lang, "contrast"))
            ).changed();

            if gamma_changed || brightness_changed || contrast_changed {
                self.refresh_ramp();
            }

            ui.add_space(6.0);

            ui.horizontal(|ui| {
                if ui.button(I18n::tr(&self.lang, "reset")).clicked() {
                    self.reset();
                }
                let (status, color) = if self.is_active {
                    (I18n::tr(&self.lang, "active"), egui::Color32::LIGHT_GREEN)
                } else {
                    (I18n::tr(&self.lang, "waiting"), egui::Color32::DARK_GRAY)
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
            .with_inner_size([370.0, 500.0])
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
