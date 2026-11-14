//! Windows platform primitives: COM apartment and the shared D3D11 device.
#![allow(unsafe_code)]

pub mod disk;
pub mod hotkey;
pub mod notify;
pub mod shell;
pub mod startup;
pub mod system_info;
pub mod tray;
pub mod watch;

use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::core::Interface;

pub use windows::core::Error as WinError;

/// Joins the calling thread to the multithreaded COM apartment for its
/// lifetime. Must be created on, and dropped on, the same thread.
pub struct ComApartment {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComApartment {
    pub fn init_multithreaded() -> Result<Self, WinError> {
        // SAFETY: no reserved pointer; balanced by CoUninitialize in Drop on
        // success only. S_FALSE (already initialized as MTA) also needs a
        // balancing CoUninitialize, which `.ok()` treats as success.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        Ok(Self {
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: paired with the successful CoInitializeEx on this thread
        // (the type is !Send, so it cannot be dropped elsewhere).
        unsafe { CoUninitialize() };
    }
}

/// Hardware D3D11 device shared by capture (WGC frame pool) and the
/// Media Foundation encoder. Textures never leave this device, so capture to
/// encoder needs no CPU readback and no cross-adapter copy.
///
/// The device is multithread protected because Media Foundation and the WGC
/// frame pool use it from their own worker threads.
#[derive(Clone)]
pub struct D3DContext {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
}

impl D3DContext {
    /// Creates a device on the default hardware adapter with BGRA (required
    /// by WGC) and video (required by MF DXGI device manager) support.
    pub fn new() -> Result<Self, WinError> {
        let levels: [D3D_FEATURE_LEVEL; 3] = [
            D3D_FEATURE_LEVEL_11_1,
            D3D_FEATURE_LEVEL_11_0,
            D3D_FEATURE_LEVEL_10_1,
        ];
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers reference live locals; no software module.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or_else(|| WinError::from(windows::Win32::Foundation::E_POINTER))?;
        let context =
            context.ok_or_else(|| WinError::from(windows::Win32::Foundation::E_POINTER))?;

        let multithread: ID3D11Multithread = context.cast()?;
        // SAFETY: valid interface; enabling protection has no preconditions.
        // The return value is the previous state and carries no error.
        let _ = unsafe { multithread.SetMultithreadProtected(true) };

        let dxgi_device: IDXGIDevice = device.cast()?;
        // SAFETY: dxgi_device is a valid IDXGIDevice of a live D3D11 device.
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)? };
        let winrt_device: IDirect3DDevice = inspectable.cast()?;

        Ok(Self {
            device,
            context,
            winrt_device,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Immediate context. Thread-safe only because the device is
    /// multithread protected; keep calls short.
    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// WinRT wrapper of the same device, required by WGC frame pools.
    pub fn winrt_device(&self) -> &IDirect3DDevice {
        &self.winrt_device
    }
}
