//! Memory available for running models (system RAM, dedicated video memory per GPU) and what
//! the GPUs are, for picking the right llama.cpp build.

use serde::Serialize;
use windows::{
    Win32::{
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIDevice, IDXGIFactory1},
        System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
    },
    core::Interface,
};

pub const VENDOR_NVIDIA: u32 = 0x10DE;
/// Qualcomm reports either its PCI id or the "QCOM" tag.
pub const VENDOR_QUALCOMM: [u32; 2] = [0x5143, 0x4D4F_4351];

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    /// Dedicated video memory in bytes (shared system memory is not counted).
    pub vram: u64,
    #[serde(skip)]
    pub vendor: u32,
    /// Driver version as the vendor shows it (NVIDIA: 581.80); None if unknown.
    #[serde(skip)]
    pub driver: Option<f64>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HardwareInfo {
    /// Physical RAM in bytes.
    ram: u64,
    /// Hardware adapters, largest video memory first.
    gpus: Vec<Gpu>,
}

fn total_ram() -> u64 {
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a properly sized MEMORYSTATUSEX with dwLength set.
    match unsafe { GlobalMemoryStatusEx(&mut status) } {
        Ok(()) => status.ullTotalPhys,
        Err(_) => 0,
    }
}

/// NVIDIA packs its driver version into the last digits of the Windows driver version:
/// 32.0.15.8180 is 581.80.
fn nvidia_driver(umd: i64) -> f64 {
    let sub = (umd >> 16) & 0xFFFF;
    let build = umd & 0xFFFF;
    ((sub % 10) * 10_000 + build) as f64 / 100.0
}

/// Hardware GPUs, largest video memory first.
pub fn gpus() -> Vec<Gpu> {
    // SAFETY: plain DXGI enumeration; every call's result is checked.
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else {
        return Vec::new();
    };
    let mut gpus = Vec::new();
    for index in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else { break };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue; // Microsoft Basic Render Driver
        }
        let umd = unsafe { adapter.CheckInterfaceSupport(&IDXGIDevice::IID) }.ok();
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        gpus.push(Gpu {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            vram: desc.DedicatedVideoMemory as u64,
            vendor: desc.VendorId,
            driver: umd.filter(|_| desc.VendorId == VENDOR_NVIDIA).map(nvidia_driver),
        });
    }
    gpus.sort_by_key(|gpu| std::cmp::Reverse(gpu.vram));
    gpus
}

/// RAM and GPUs (blocking).
pub fn info() -> HardwareInfo {
    HardwareInfo { ram: total_ram(), gpus: gpus() }
}
