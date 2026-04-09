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
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
};
use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;
use winapi::um::winuser::{
    GetAsyncKeyState, GetForegroundWindow, GetWindowThreadProcessId, EnumDisplayDevicesW,
};

type GammaRamp = [u16; 768];

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

struct AppState {
    settings: DisplaySettings,
    is_active: bool,
    original_ramps: HashMap<String, GammaRamp>,
    cached_ramp: GammaRamp,
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
    let mut ramp_float = [0.0f32; 256];
    let contrast_factor = (settings.contrast + 0.5).powf(2.0);

    for i in 0..256 {
        let mut val = (i as f32 / 255.0 - 0.5) * contrast_factor + 0.5;
        val += settings.brightness - 0.5;

        if val > 0.0 {
            val = val.powf(1.0 / settings.gamma.max(0.01));
        }

        ramp_float[i] = val.clamp(0.0, 1.0);
    }

    let min_val = ramp_float.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_val = ramp_float.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let spread = max_val - min_val;

    if spread < 0.05 {
        let center = (max_val + min_val) / 2.0;
        for i in 0..256 {
            let t = i as f32 / 255.0;
            ramp_float[i] = (center - 0.025 + t * 0.05).clamp(0.0, 1.0);
        }
    }

    for i in 1..256 {
        if ramp_float[i] <= ramp_float[i - 1] {
            let next_val = if i < 255 { ramp_float[i + 1] } else { 1.0 };
            ramp_float[i] = (ramp_float[i - 1] + next_val) / 2.0;
        }
    }

    let mut ramp: GammaRamp = [0; 768];
    for i in 0..256 {
        let word = (ramp_float[i] * 65535.0) as u16;
        ramp[i] = word;
        ramp[i + 256] = word;
        ramp[i + 512] = word;
    }
    ramp
}

impl AppState {
    fn save_original_ramps(&mut self) {
        unsafe {
            let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let mut dev_num = 0;

            while EnumDisplayDevicesW(null_mut(), dev_num, &mut device, 0) != 0 {
                if (device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) != 0 {
                    let hdc = CreateDCW(
                        null_mut(),
                        device.DeviceName.as_ptr(),
                        null_mut(),
                        null_mut(),
                    );
                    if !hdc.is_null() {
                        let device_name = String::from_utf16_lossy(&device.DeviceName)
                            .trim_end_matches('\0')
                            .to_string();
                        if !self.original_ramps.contains_key(&device_name) {
                            let mut current: GammaRamp = [0; 768];
                            if GetDeviceGammaRamp(hdc, current.as_mut_ptr() as *mut _) != 0 {
                                self.original_ramps.insert(device_name, current);
                            }
                        }
                        DeleteDC(hdc);
                    }
                }
                dev_num += 1;
            }
        }
    }

    fn apply_ramp(&self, ramp: &GammaRamp) {
        unsafe {
            let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let mut dev_num = 0;

            while EnumDisplayDevicesW(null_mut(), dev_num, &mut device, 0) != 0 {
                if (device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) != 0 {
                    let hdc = CreateDCW(
                        null_mut(),
                        device.DeviceName.as_ptr(),
                        null_mut(),
                        null_mut(),
                    );
                    if !hdc.is_null() {
                        SetDeviceGammaRamp(hdc, ramp.as_ptr() as *mut _);
                        DeleteDC(hdc);
                    }
                }
                dev_num += 1;
            }
        }
    }

    fn restore_original_ramps(&self) {
        if self.original_ramps.is_empty() {
            return;
        }
        unsafe {
            let mut device: DISPLAY_DEVICEW = std::mem::zeroed();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let mut dev_num = 0;

            while EnumDisplayDevicesW(null_mut(), dev_num, &mut device, 0) != 0 {
                if (device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) != 0 {
                    let hdc = CreateDCW(
                        null_mut(),
                        device.DeviceName.as_ptr(),
                        null_mut(),
                        null_mut(),
                    );
                    if !hdc.is_null() {
                        let device_name = String::from_utf16_lossy(&device.DeviceName)
                            .trim_end_matches('\0')
                            .to_string();
                        if let Some(original) = self.original_ramps.get(&device_name) {
                            SetDeviceGammaRamp(hdc, original.as_ptr() as *mut _);
                        }
                        DeleteDC(hdc);
                    }
                }
                dev_num += 1;
            }
        }
    }

    fn activate(&mut self) {
        if self.is_active {
            return;
        }
        if self.original_ramps.is_empty() {
            self.save_original_ramps();
        }
        self.apply_ramp(&self.cached_ramp);
        self.is_active = true;
    }

    fn deactivate(&mut self) {
        if !self.is_active {
            return;
        }
        self.restore_original_ramps();
        self.is_active = false;
    }

    fn refresh_ramp(&mut self) {
        self.cached_ramp = calculate_ramp(&self.settings);
        if self.is_active {
            self.apply_ramp(&self.cached_ramp);
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

            let gamma_slider =
                ui.add(egui::Slider::new(&mut self.settings.gamma, 0.5..=3.0).text("Гамма"));
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
            .with_inner_size([340.0, 420.0])
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
