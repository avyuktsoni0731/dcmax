use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1,
};

#[derive(Debug, Clone)]
pub struct DxgiAdapterProbe {
    pub adapter_name: String,
    pub vendor_id: u32,
    pub dedicated_video_memory_mb: u64,
    pub shared_system_memory_mb: u64,
}

fn decode_adapter_name(desc: &DXGI_ADAPTER_DESC1) -> String {
    let end = desc
        .Description
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(desc.Description.len());
    String::from_utf16_lossy(&desc.Description[..end])
}

pub fn probe_primary_adapter() -> Result<DxgiAdapterProbe> {
    // Safe wrapper around DXGI COM calls; required to inspect hardware adapter capabilities.
    let probe = unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().context("CreateDXGIFactory1 failed")?;
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(0)
            .context("failed to enumerate primary DXGI adapter")?;
        let desc = adapter
            .GetDesc1()
            .context("failed to query DXGI adapter description")?;

        DxgiAdapterProbe {
            adapter_name: decode_adapter_name(&desc),
            vendor_id: desc.VendorId,
            dedicated_video_memory_mb: (desc.DedicatedVideoMemory / (1024 * 1024)) as u64,
            shared_system_memory_mb: (desc.SharedSystemMemory / (1024 * 1024)) as u64,
        }
    };

    Ok(probe)
}

