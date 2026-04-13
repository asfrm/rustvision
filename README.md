<div align="center">

[![Rust](https://img.shields.io/badge/Rust-stable-orange?logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows-lightgrey)]()
[![Release](https://img.shields.io/github/v/release/asfrm/rustvision?label=latest%20release)](https://github.com/asfrm/rustvision/releases)

# RustVision

**RustVision** is a lightweight, high-performance tool designed for real-time gamma adjustment in games like Rust. Built entirely in Rust for maximum efficiency and safety.

[Download Latest Release](https://github.com/asfrm/rustvision/releases)

</div>

---

## 🛡️ Security & Anti-Cheat Safety

RustVision is designed to be as non-intrusive as possible, making it safe for use with various anti-cheat systems (EAC, BattlEye, etc.):

* **No Hooks:** Does not interact with or hook into DirectX, Vulkan, or OpenGL.
* **No DLL Injection:** Operates strictly as a standalone process.
* **External Operation:** Adjusts gamma at the system/display level rather than modifying game memory.

## ✨ Features

* **On-the-fly Adjustment:** Change gamma settings instantly without restarting your game.
* **Customizable:** Manually change the target process name to support any game.
* **Minimal Footprint:** Extremely low CPU and RAM usage.

---

## 🚀 Getting Started

### Installation
1.  Navigate to the [Releases](https://github.com/asfrm/rustvision/releases) page.
2.  Download the latest `rustvision.exe`.
3.  Run the executable—no installation required.

### Build from Source
If you prefer to compile the project yourself:

1.  **Install Rust:** Ensure you have the Rust toolchain installed via [rustup.rs](https://rustup.rs/).
2.  **Clone the Repository:**
    ```bash
    git clone https://github.com/asfrm/rustvision.git
    cd rustvision
    ```
3.  **Build the Release:**
    ```bash
    cargo build --release
    ```
    The compiled binary will be located at `target/release/rustvision.exe`.

---

## 📄 License

This project is licensed under the **MIT License**. See the [LICENSE](LICENSE) file for more details.

---

## 📸 Preview



https://github.com/user-attachments/assets/7495bcf7-fcdb-4c50-9f0c-7ffcbbd67c14

