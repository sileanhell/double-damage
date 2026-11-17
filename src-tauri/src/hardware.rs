//! Memory available for running models (system RAM, dedicated video memory per GPU), for
//! planning how a local model is started. (Picking the llama.cpp build is dd-core's job.)

use serde::Serialize;
use windows::{
    Win32::{
        Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
        System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
    },
};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    pub name: String,
    /// Dedicated video memory in bytes (shared system memory is not counted).
    pub vram: u64,
}

#[derive(Serialize)]
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

/// Hardware GPUs, largest video memory first.
fn gpus() -> Vec<Gpu> {
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
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        gpus.push(Gpu {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            vram: desc.DedicatedVideoMemory as u64,
        });
    }
    gpus.sort_by_key(|gpu| std::cmp::Reverse(gpu.vram));
    gpus
}

#[tauri::command]
pub async fn hardware_info() -> HardwareInfo {
    tauri::async_runtime::spawn_blocking(|| HardwareInfo { ram: total_ram(), gpus: gpus() })
        .await
        .unwrap_or(HardwareInfo { ram: 0, gpus: Vec::new() })
}
