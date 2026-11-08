// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional cross-process NV12 transport. Its separate versioned control mapping
//! leaves the existing RGB transport usable by older app/DLL combinations.
//! Each reader owns a stable GPU snapshot. Keyed mutexes protect shared slots;
//! a busy writer never replaces a completed snapshot with an idle placeholder.

use crate::shm::{MAX_HEIGHT, MAX_WIDTH, SharedFrameMapping, fit_bgrx_aspect, fit_nv12_aspect};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, E_FAIL, E_INVALIDARG, HANDLE, HLOCAL, LUID, LocalFree, S_OK},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        Media::MediaFoundation::{
            IMF2DBuffer2, IMFDXGIBuffer, IMFMediaBuffer, IMFSample, MF2DBuffer_LockFlags_Write,
        },
        Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        },
    },
    core::{HSTRING, Interface},
};

const GLOBAL: &str = "Global\\NectarlinkVcamGpu_v1";
const LOCAL: &str = "Local\\NectarlinkVcamGpu_v1";
const ACCESS: u32 = DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0;
static NEXT_EPOCH: AtomicU64 = AtomicU64::new(0);
#[path = "gpu_scale.rs"]
mod video_scale;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Descriptor {
    pid: u32,
    slot: u32,
    epoch: u64,
    coded_w: u32,
    coded_h: u32,
    width: u32,
    height: u32,
    colors: u32,
    frame: u64,
    adapter: u64,
}
impl Descriptor {
    fn bytes(self) -> Vec<u8> {
        let mut b = Vec::with_capacity(56);
        for n in [self.pid, self.slot] {
            b.extend(n.to_le_bytes());
        }
        b.extend(self.epoch.to_le_bytes());
        for n in [self.coded_w, self.coded_h, self.width, self.height, self.colors, 0] {
            b.extend(n.to_le_bytes());
        }
        b.extend(self.frame.to_le_bytes());
        b.extend(self.adapter.to_le_bytes());
        b
    }
    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() != 56 {
            return None;
        }
        let u32_at = |at| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let d = Self {
            pid: u32_at(0),
            slot: u32_at(4),
            epoch: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            coded_w: u32_at(16),
            coded_h: u32_at(20),
            width: u32_at(24),
            height: u32_at(28),
            colors: u32_at(32),
            frame: u64::from_le_bytes(b[40..48].try_into().unwrap()),
            adapter: u64::from_le_bytes(b[48..56].try_into().unwrap()),
        };
        (d.pid != 0
            && d.slot < 3
            && d.width > 0
            && d.height > 0
            && d.width.is_multiple_of(2)
            && d.height.is_multiple_of(2)
            && d.width <= d.coded_w
            && d.height <= d.coded_h
            && d.coded_w <= MAX_WIDTH
            && d.coded_h <= MAX_HEIGHT + 32
            && d.coded_w.is_multiple_of(2)
            && d.coded_h.is_multiple_of(2)
            && d.colors <= 3)
            .then_some(d)
    }
    fn name(self) -> HSTRING {
        HSTRING::from(format!("Global\\NectarlinkVcamGpu_{}_{}_{}", self.pid, self.epoch, self.slot))
    }
}

// AcquireSync returns positive WAIT_TIMEOUT/WAIT_ABANDONED codes, which the
// generated Result wrapper treats as success. Only S_OK grants ownership.
#[allow(unsafe_code)]
fn acquire(mutex: &IDXGIKeyedMutex, key: u64) -> windows::core::Result<bool> {
    let status = unsafe { (mutex.vtable().AcquireSync)(mutex.as_raw(), key, 0) };
    if status == S_OK {
        Ok(true)
    } else if status.0 == 258 {
        Ok(false)
    } else if status.is_err() {
        Err(windows::core::Error::from_hresult(status))
    } else {
        Err(E_FAIL.into())
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
fn device() -> windows::core::Result<ID3D11Device> {
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            Default::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(|| E_FAIL.into())
}

#[allow(unsafe_code)]
fn adapter_luid(device: &ID3D11Device) -> windows::core::Result<u64> {
    let luid = unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?.AdapterLuid };
    Ok(u64::from(luid.LowPart) | ((luid.HighPart as u32 as u64) << 32))
}

#[allow(unsafe_code)]
fn device_for_adapter(luid: u64) -> windows::core::Result<ID3D11Device> {
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
        let adapter: IDXGIAdapter =
            factory.EnumAdapterByLuid(LUID { LowPart: luid as u32, HighPart: (luid >> 32) as i32 })?;
        let mut device = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            Default::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
        let device = device.ok_or(E_FAIL)?;
        if let Ok(multithread) = device.cast::<ID3D11Multithread>() {
            let _ = multithread.SetMultithreadProtected(true);
        }
        Ok(device)
    }
}

struct SharedSlot {
    texture: ID3D11Texture2D,
    mutex: IDXGIKeyedMutex,
    handle: HANDLE,
    initialized: bool,
    source: Option<IMFSample>,
}
impl Drop for SharedSlot {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Publisher belongs exclusively to the webcam decoder thread.
pub struct GpuPublisher {
    mapping: Option<SharedFrameMapping>,
    mapping_global: bool,
    slots: Vec<SharedSlot>,
    shape: Option<(u32, u32, usize)>,
    epoch: u64,
    frame: u64,
    next: usize,
    checked: Option<Instant>,
    unavailable: bool,
}
impl std::fmt::Debug for GpuPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuPublisher").field("shape", &self.shape).field("frame", &self.frame).finish()
    }
}
impl Default for GpuPublisher {
    fn default() -> Self {
        Self::new()
    }
}
impl GpuPublisher {
    pub fn new() -> Self {
        Self {
            mapping: None,
            mapping_global: false,
            slots: Vec::new(),
            shape: None,
            epoch: 0,
            frame: 0,
            next: 0,
            checked: None,
            unavailable: false,
        }
    }
    pub fn idle(&self) {
        if let Some(m) = &self.mapping {
            m.set_idle("");
        }
    }
    /// Join a dedicated control mapping, for isolated transports and tests.
    pub fn new_with_mapping(name: &str) -> windows::core::Result<Self> {
        let mut publisher = Self::new();
        publisher.mapping = Some(SharedFrameMapping::open_existing_control(name)?);
        publisher.mapping_global = true;
        Ok(publisher)
    }
    pub fn disable(&mut self) {
        self.unavailable = true;
    }
    #[cfg(test)]
    pub fn stats(&self) -> Option<(u32, u32)> {
        self.mapping.as_ref().map(|m| m.control_stats())
    }
    /// Returns false to request the existing CPU/RGB fallback. No camera client
    /// (or an old DLL) is a normal case, not a fatal decoder error.
    #[allow(unsafe_code)]
    pub fn publish(
        &mut self,
        texture: &ID3D11Texture2D,
        index: u32,
        sample: &IMFSample,
        dimensions: (u32, u32),
        bt601: bool,
        full_range: bool,
    ) -> windows::core::Result<bool> {
        let (width, height) = dimensions;
        if !self.mapping_global && self.checked.is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
            self.checked = Some(Instant::now());
            if let Ok(global) = SharedFrameMapping::open_existing_control(GLOBAL) {
                self.idle();
                self.mapping = Some(global);
                self.mapping_global = true;
                self.unavailable = false;
            } else if self.mapping.is_none() {
                self.mapping = SharedFrameMapping::open_existing_control(LOCAL).ok();
            }
        }
        if self.unavailable {
            return Ok(false);
        }
        let Some(mapping) = &self.mapping else { return Ok(false) };
        if mapping.control_failed() {
            self.unavailable = true;
            let code = mapping.control_code();
            let status = windows::core::HRESULT(code);
            return Err(windows::core::Error::from_hresult(if status.is_err() { status } else { E_FAIL }));
        }
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if desc.Format != DXGI_FORMAT_NV12
                || width == 0
                || height == 0
                || !width.is_multiple_of(2)
                || !height.is_multiple_of(2)
                || width > desc.Width
                || height > desc.Height
                || desc.Width > MAX_WIDTH
                || desc.Height > MAX_HEIGHT + 32
            {
                return Ok(false);
            }
            let device = texture.GetDevice()?;
            let shape = (desc.Width, desc.Height, device.as_raw() as usize);
            if self.shape != Some(shape) {
                self.idle();
                self.slots.clear();
                self.shape = None;
                self.epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()
                    as u64
                    ^ NEXT_EPOCH.fetch_add(1, Ordering::Relaxed);
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    windows::core::w!("D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)"),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )?;
                let attrs = SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.0,
                    bInheritHandle: false.into(),
                };
                let result = (|| -> windows::core::Result<()> {
                    for slot in 0..3 {
                        let descriptor = Descriptor {
                            pid: std::process::id(),
                            slot,
                            epoch: self.epoch,
                            coded_w: desc.Width,
                            coded_h: desc.Height,
                            width,
                            height,
                            colors: 0,
                            frame: 0,
                            adapter: adapter_luid(&device)?,
                        };
                        let mut shared = None;
                        device.CreateTexture2D(
                            &D3D11_TEXTURE2D_DESC {
                                Width: desc.Width,
                                Height: desc.Height,
                                MipLevels: 1,
                                ArraySize: 1,
                                Format: DXGI_FORMAT_NV12,
                                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                                Usage: D3D11_USAGE_DEFAULT,
                                BindFlags: 0,
                                CPUAccessFlags: 0,
                                MiscFlags: (D3D11_RESOURCE_MISC_SHARED_NTHANDLE
                                    | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX)
                                    .0 as u32,
                            },
                            None,
                            Some(&mut shared),
                        )?;
                        let shared = shared.ok_or(E_FAIL)?;
                        let mutex = shared.cast()?;
                        let handle = shared.cast::<IDXGIResource1>()?.CreateSharedHandle(
                            Some(&attrs),
                            ACCESS,
                            &descriptor.name(),
                        )?;
                        self.slots.push(SharedSlot {
                            texture: shared,
                            mutex,
                            handle,
                            initialized: false,
                            source: None,
                        });
                    }
                    Ok(())
                })();
                let _ = LocalFree(Some(HLOCAL(sd.0)));
                result?;
                self.shape = Some(shape);
            }
            for _ in 0..self.slots.len() {
                let slot = self.next;
                self.next = (self.next + 1) % self.slots.len();
                let shared = &mut self.slots[slot];
                if !acquire(&shared.mutex, if shared.initialized { 1 } else { 0 })? {
                    continue;
                }
                // Reacquiring the slot also means its previous GPU work finished.
                // Retaining the decoder sample prevents its surface being recycled.
                shared.source = Some(sample.clone());
                let context = device.GetImmediateContext()?;
                context.CopySubresourceRegion(&shared.texture, 0, 0, 0, 0, texture, index, None);
                context.Flush();
                shared.mutex.ReleaseSync(1)?;
                shared.initialized = true;
                self.frame = self.frame.wrapping_add(1);
                let descriptor = Descriptor {
                    pid: std::process::id(),
                    slot: slot as u32,
                    epoch: self.epoch,
                    coded_w: desc.Width,
                    coded_h: desc.Height,
                    width,
                    height,
                    colors: u32::from(bt601) | (u32::from(full_range) << 1),
                    frame: self.frame,
                    adapter: adapter_luid(&device)?,
                };
                self.mapping.as_ref().unwrap().write_control(&descriptor.bytes());
                return Ok(true);
            }
        }
        // All slots busy: retain the last published frame, never flash idle or
        // force an expensive CPU readback just because a reader owns a slot.
        Ok(self.frame != 0)
    }
}
impl Drop for GpuPublisher {
    fn drop(&mut self) {
        self.idle();
    }
}

/// One reader per media source stream. No source pixels are read while another
/// process may write them; only the reader-owned GPU snapshot is sampled.
pub struct GpuReader {
    mapping: SharedFrameMapping,
    device: Option<ID3D11Device>,
    unshareable_device: Option<usize>,
    descriptor: Option<Descriptor>,
    snapshot: Option<ID3D11Texture2D>,
    staging: Option<ID3D11Texture2D>,
    cpu_frame: Option<u64>,
    cpu: Vec<u8>,
    rgb_frame: Option<u64>,
    rgb: Vec<u8>,
    scaled_key: Option<(Descriptor, u32, u32)>,
    scaled: Vec<u8>,
    scaler: Option<video_scale::GpuScaler>,
}
impl std::fmt::Debug for GpuReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuReader").field("descriptor", &self.descriptor).finish()
    }
}
impl GpuReader {
    pub fn new() -> windows::core::Result<Self> {
        let mapping = SharedFrameMapping::open_or_create_control(GLOBAL)
            .or_else(|_| SharedFrameMapping::open_or_create_control(LOCAL))?;
        Ok(Self::from_mapping(mapping))
    }
    /// Uses an isolated metadata mapping without changing the live camera.
    pub fn new_with_mapping(name: &str) -> windows::core::Result<Self> {
        Ok(Self::from_mapping(SharedFrameMapping::open_or_create_control(name)?))
    }
    fn from_mapping(mapping: SharedFrameMapping) -> Self {
        Self {
            mapping,
            device: None,
            unshareable_device: None,
            descriptor: None,
            snapshot: None,
            staging: None,
            cpu_frame: None,
            cpu: Vec::new(),
            rgb_frame: None,
            rgb: Vec::new(),
            scaled_key: None,
            scaled: Vec::new(),
            scaler: None,
        }
    }
    pub fn disable(&mut self) {
        self.mapping.fail_control();
        self.descriptor = None;
        self.snapshot = None;
    }
    pub fn disable_with_error(&mut self, error: &windows::core::Error) {
        self.mapping.fail_control_code(error.code().0);
        self.descriptor = None;
        self.snapshot = None;
    }
    pub fn active_device(&mut self) -> windows::core::Result<Option<ID3D11Device>> {
        if self.refresh(None)? { Ok(self.device.clone()) } else { Ok(None) }
    }
    #[allow(unsafe_code)]
    fn refresh(&mut self, preferred: Option<ID3D11Device>) -> windows::core::Result<bool> {
        if self.mapping.control_failed() {
            return Ok(false);
        }
        let Some((_, bytes)) = self.mapping.read_control() else { return Ok(self.snapshot.is_some()) };
        if bytes.is_empty() {
            self.descriptor = None;
            self.snapshot = None;
            self.cpu_frame = None;
            self.rgb_frame = None;
            return Ok(false);
        }
        let next = Descriptor::parse(&bytes).ok_or(E_INVALIDARG)?;
        let preferred_id = preferred.as_ref().map(|d| d.as_raw() as usize);
        if let Some(preferred) = preferred
            && self.unshareable_device != Some(preferred.as_raw() as usize)
            && adapter_luid(&preferred)? == next.adapter
            && self.device.as_ref().is_none_or(|d| d.as_raw() != preferred.as_raw())
        {
            self.device = Some(preferred);
            self.snapshot = None;
            self.staging = None;
            self.descriptor = None;
        }
        if self.descriptor == Some(next) && self.snapshot.is_some() {
            return Ok(true);
        }
        if self.device.as_ref().is_none_or(|device| adapter_luid(device).ok() != Some(next.adapter)) {
            self.device = Some(device_for_adapter(next.adapter)?);
            self.snapshot = None;
            self.staging = None;
            self.descriptor = None;
        }
        let mut device = self.device.as_ref().unwrap().clone();
        unsafe {
            let shared: ID3D11Texture2D =
                match device.cast::<ID3D11Device1>()?.OpenSharedResourceByName(&next.name(), ACCESS) {
                    Ok(shared) => shared,
                    Err(error) if preferred_id == Some(device.as_raw() as usize) => {
                        self.unshareable_device = preferred_id;
                        device = device_for_adapter(next.adapter)?;
                        self.device = Some(device.clone());
                        self.snapshot = None;
                        self.staging = None;
                        self.descriptor = None;
                        device
                            .cast::<ID3D11Device1>()?
                            .OpenSharedResourceByName(&next.name(), ACCESS)
                            .map_err(|_| error)?
                    }
                    Err(error) => return Err(error),
                };
            let mutex: IDXGIKeyedMutex = shared.cast()?;
            if !acquire(&mutex, 1)? {
                return Ok(self.snapshot.is_some());
            }
            let result = (|| -> windows::core::Result<bool> {
                // Validate the descriptor again after ownership; the slot may have
                // been replaced while this reader was opening its handle.
                if self.mapping.read_control().and_then(|(_, b)| Descriptor::parse(&b)) != Some(next) {
                    return Ok(self.snapshot.is_some());
                }
                let mut actual = D3D11_TEXTURE2D_DESC::default();
                shared.GetDesc(&mut actual);
                if actual.Format != DXGI_FORMAT_NV12
                    || actual.Width != next.coded_w
                    || actual.Height != next.coded_h
                    || actual.ArraySize != 1
                {
                    return Err(E_INVALIDARG.into());
                }
                let same_shape = self
                    .descriptor
                    .is_some_and(|old| old.coded_w == next.coded_w && old.coded_h == next.coded_h);
                if self.snapshot.is_none() || !same_shape {
                    let mut snapshot = None;
                    device.CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Usage: D3D11_USAGE_DEFAULT,
                            MiscFlags: 0,
                            CPUAccessFlags: 0,
                            ..actual
                        },
                        None,
                        Some(&mut snapshot),
                    )?;
                    self.snapshot = snapshot;
                    self.staging = None;
                }
                let context = device.GetImmediateContext()?;
                context.CopySubresourceRegion(self.snapshot.as_ref().unwrap(), 0, 0, 0, 0, &shared, 0, None);
                context.Flush();
                self.descriptor = Some(next);
                self.cpu_frame = None;
                self.rgb_frame = None;
                Ok(true)
            })();
            let release = mutex.ReleaseSync(1);
            release?;
            result
        }
    }
    #[allow(unsafe_code)]
    fn download(&mut self) -> windows::core::Result<()> {
        let d = self.descriptor.ok_or(E_FAIL)?;
        if self.cpu_frame == Some(d.frame) {
            return Ok(());
        }
        unsafe {
            let device = self.device.as_ref().ok_or(E_FAIL)?;
            if self.staging.is_none() {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                self.snapshot.as_ref().ok_or(E_FAIL)?.GetDesc(&mut desc);
                let mut staging = None;
                device.CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        ..desc
                    },
                    None,
                    Some(&mut staging),
                )?;
                self.staging = staging;
            }
            let context = device.GetImmediateContext()?;
            let staging = self.staging.as_ref().unwrap();
            context.CopySubresourceRegion(staging, 0, 0, 0, 0, self.snapshot.as_ref().unwrap(), 0, None);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let stride = mapped.RowPitch as usize;
            let width = d.width as usize;
            let height = d.height as usize;
            self.cpu.resize(width * height * 3 / 2, 0);
            for row in 0..height {
                std::ptr::copy_nonoverlapping(
                    mapped.pData.cast::<u8>().add(row * stride),
                    self.cpu.as_mut_ptr().add(row * width),
                    width,
                );
            }
            let uv = mapped.pData.cast::<u8>().add(stride * d.coded_h as usize);
            for row in 0..height / 2 {
                std::ptr::copy_nonoverlapping(
                    uv.add(row * stride),
                    self.cpu.as_mut_ptr().add(width * height + row * width),
                    width,
                );
            }
            context.Unmap(staging, 0);
            self.cpu_frame = Some(d.frame);
            #[cfg(test)]
            self.mapping.count_download();
            Ok(())
        }
    }
    /// Native NV12 output, using GPU copies when the allocator provides a DXGI
    /// surface and one readback otherwise. No RGB round trip for matching modes.
    #[allow(unsafe_code)]
    pub fn write_nv12(
        &mut self,
        buffer: &IMFMediaBuffer,
        width: u32,
        height: u32,
    ) -> windows::core::Result<bool> {
        unsafe {
            let destination = if let Ok(dxgi) = buffer.cast::<IMFDXGIBuffer>() {
                let mut texture: Option<ID3D11Texture2D> = None;
                dxgi.GetResource(&ID3D11Texture2D::IID, std::ptr::from_mut(&mut texture).cast())?;
                texture.map(|t| dxgi.GetSubresourceIndex().map(|i| (t, i))).transpose()?
            } else {
                None
            };
            let preferred = destination.as_ref().map(|(t, _)| t.GetDevice()).transpose()?;
            if !self.refresh(preferred)? {
                return Ok(false);
            }
            let d = self.descriptor.ok_or(E_FAIL)?;
            // Different colorimetry is normalized via the RGB fallback.
            if d.colors != 0
                || width == 0
                || height == 0
                || !width.is_multiple_of(2)
                || !height.is_multiple_of(2)
            {
                return Ok(false);
            }
            if let Some((destination, index)) = destination
                && self.device.as_ref().is_some_and(|device| {
                    destination.GetDevice().is_ok_and(|d| d.as_raw() == device.as_raw())
                })
            {
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                destination.GetDesc(&mut desc);
                if desc.Format != DXGI_FORMAT_NV12 || desc.Width < width || desc.Height < height {
                    return Ok(false);
                }
                if (d.width, d.height) != (width, height) {
                    let device = self.device.as_ref().ok_or(E_FAIL)?;
                    if self
                        .scaler
                        .as_ref()
                        .is_none_or(|s| !s.matches(device, (d.coded_w, d.coded_h), (width, height)))
                    {
                        self.scaler =
                            video_scale::GpuScaler::new(device, (d.coded_w, d.coded_h), (width, height)).ok();
                    }
                    if let Some(scaler) = &self.scaler
                        && scaler
                            .blit(
                                self.snapshot.as_ref().ok_or(E_FAIL)?,
                                &destination,
                                index,
                                (d.width, d.height),
                            )
                            .is_ok()
                    {
                        #[cfg(test)]
                        self.mapping.count_gpu_sample();
                        return Ok(true);
                    }
                } else {
                    let context = destination.GetDevice()?.GetImmediateContext()?;
                    let region =
                        D3D11_BOX { left: 0, top: 0, front: 0, right: width, bottom: height, back: 1 };
                    context.CopySubresourceRegion(
                        &destination,
                        index,
                        0,
                        0,
                        0,
                        self.snapshot.as_ref().ok_or(E_FAIL)?,
                        0,
                        Some(&region),
                    );
                    context.Flush();
                    #[cfg(test)]
                    self.mapping.count_gpu_sample();
                    return Ok(true);
                }
            }
            self.download()?;
            let data = if (d.width, d.height) == (width, height) {
                &self.cpu
            } else {
                if self.scaled_key != Some((d, width, height)) {
                    self.scaled.resize(width as usize * height as usize * 3 / 2, 0);
                    if !fit_nv12_aspect(&self.cpu, d.width, d.height, &mut self.scaled, width, height) {
                        return Err(E_INVALIDARG.into());
                    }
                    self.scaled_key = Some((d, width, height));
                }
                &self.scaled
            };
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
                let mut ptr = std::ptr::null_mut();
                let mut pitch = 0i32;
                let mut start = std::ptr::null_mut();
                let mut len = 0;
                two_d.Lock2DSize(MF2DBuffer_LockFlags_Write, &mut ptr, &mut pitch, &mut start, &mut len)?;
                let stride = pitch.unsigned_abs() as usize;
                let w = width as usize;
                let h = height as usize;
                let end = (ptr as usize)
                    .checked_sub(start as usize)
                    .and_then(|offset| offset.checked_add(stride * h * 3 / 2));
                let result: windows::core::Result<()> = if pitch <= 0
                    || stride < w
                    || end.is_none_or(|end| end > len as usize)
                    || start.is_null()
                    || ptr.is_null()
                {
                    Err(E_INVALIDARG.into())
                } else {
                    for row in 0..h {
                        std::ptr::copy_nonoverlapping(data.as_ptr().add(row * w), ptr.add(row * stride), w);
                    }
                    for row in 0..h / 2 {
                        std::ptr::copy_nonoverlapping(
                            data.as_ptr().add(w * h + row * w),
                            ptr.add(stride * h + row * stride),
                            w,
                        );
                    }
                    Ok(())
                };
                let unlock = two_d.Unlock2D();
                unlock?;
                result?;
            } else {
                if (buffer.GetMaxLength()? as usize) < data.len() {
                    return Err(E_INVALIDARG.into());
                }
                let mut ptr = std::ptr::null_mut();
                buffer.Lock(&mut ptr, None, None)?;
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
                buffer.Unlock()?;
                buffer.SetCurrentLength(data.len() as u32)?;
            }
            Ok(true)
        }
    }
    /// CPU fallback for RGB clients, scaled output, or nonstandard source colors.
    pub fn read_bgrx(&mut self, width: u32, height: u32, out: &mut [u8]) -> windows::core::Result<bool> {
        if !self.refresh(None)? {
            return Ok(false);
        }
        self.download()?;
        let d = self.descriptor.ok_or(E_FAIL)?;
        if self.rgb_frame != Some(d.frame) {
            let w = d.width as usize;
            let h = d.height as usize;
            self.rgb.resize(w * h * 4, 0);
            let full = d.colors & 2 != 0;
            let bt601 = d.colors & 1 != 0;
            let (vr, ug, vg, ub) = match (bt601, full) {
                (false, false) => (459, 55, 136, 541),
                (false, true) => (403, 48, 120, 475),
                (true, false) => (409, 100, 208, 516),
                (true, true) => (359, 88, 183, 454),
            };
            let cpu = &self.cpu;
            let convert = |first: usize, output: &mut [u8]| {
                for (row, line) in output.chunks_exact_mut(w * 4).enumerate() {
                    let y = first + row;
                    for x in 0..w {
                        let l = (i32::from(cpu[y * w + x]) - if full { 0 } else { 16 })
                            * if full { 256 } else { 298 };
                        let uv = w * h + (y / 2) * w + (x & !1);
                        let u = i32::from(cpu[uv]) - 128;
                        let v = i32::from(cpu[uv + 1]) - 128;
                        let at = x * 4;
                        line[at] = ((l + ub * u + 128) >> 8).clamp(0, 255) as u8;
                        line[at + 1] = ((l - ug * u - vg * v + 128) >> 8).clamp(0, 255) as u8;
                        line[at + 2] = ((l + vr * v + 128) >> 8).clamp(0, 255) as u8;
                        line[at + 3] = 255;
                    }
                }
            };
            let cores = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
            if w * h >= 1920 * 1080 && cores > 1 {
                let band = h.div_ceil(cores).next_multiple_of(2);
                std::thread::scope(|scope| {
                    for (i, rows) in self.rgb.chunks_mut(band * w * 4).enumerate() {
                        let convert = &convert;
                        scope.spawn(move || convert(i * band, rows));
                    }
                });
            } else {
                convert(0, &mut self.rgb);
            }
            self.rgb_frame = Some(d.frame);
        }
        let needed = width as usize * height as usize * 4;
        if out.len() < needed {
            return Err(E_INVALIDARG.into());
        }
        if (width, height) == (d.width, d.height) {
            out[..needed].copy_from_slice(&self.rgb);
        } else {
            fit_bgrx_aspect(&self.rgb, d.width, d.height, &mut out[..needed], width, height);
        }
        Ok(true)
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use windows::Win32::Media::MediaFoundation::{
        IMF2DBuffer, MF_VERSION, MFCreate2DMediaBuffer, MFCreateDXGISurfaceBuffer, MFCreateSample,
        MFSTARTUP_NOSOCKET, MFStartup, MFVideoFormat_NV12,
    };
    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn reader_with_mapping(name: &str) -> GpuReader {
        let mapping = SharedFrameMapping::open_or_create_control(name).unwrap();
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).unwrap();
        }
        GpuReader {
            mapping,
            device: None,
            unshareable_device: None,
            descriptor: None,
            snapshot: None,
            staging: None,
            cpu_frame: None,
            cpu: Vec::new(),
            rgb_frame: None,
            rgb: Vec::new(),
            scaled_key: None,
            scaled: Vec::new(),
            scaler: None,
        }
    }

    fn pair() -> (GpuPublisher, GpuReader) {
        let name = format!(
            "Local\\NectarlinkGpuTest_{}_{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let reader = reader_with_mapping(&name);
        let publisher = GpuPublisher::new_with_mapping(&name).unwrap();
        (publisher, reader)
    }
    fn texture(device: &ID3D11Device, w: u32, h: u32, y: u8) -> (ID3D11Texture2D, IMFSample) {
        unsafe {
            let mut data = vec![128u8; (w * h * 3 / 2) as usize];
            data[..(w * h) as usize].fill(y);
            let mut texture = None;
            device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: w,
                        Height: h,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_NV12,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    },
                    Some(&D3D11_SUBRESOURCE_DATA {
                        pSysMem: data.as_ptr().cast(),
                        SysMemPitch: w,
                        SysMemSlicePitch: data.len() as u32,
                    }),
                    Some(&mut texture),
                )
                .unwrap();
            let texture = texture.unwrap();
            let sample = MFCreateSample().unwrap();
            sample
                .AddBuffer(&MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture, 0, false).unwrap())
                .unwrap();
            (texture, sample)
        }
    }
    fn cpu_buffer(w: u32, h: u32) -> IMFMediaBuffer {
        unsafe { MFCreate2DMediaBuffer(w, h, MFVideoFormat_NV12.data1, false).unwrap() }
    }
    fn pixels(buffer: &IMFMediaBuffer) -> Vec<u8> {
        unsafe {
            let b: IMF2DBuffer = buffer.cast().unwrap();
            let mut bytes = vec![0; b.GetContiguousLength().unwrap() as usize];
            b.ContiguousCopyTo(&mut bytes).unwrap();
            bytes
        }
    }

    // These integration tests exercise driver-dependent D3D11 sharing and video
    // processing. Run them explicitly on a GPU-equipped Windows machine with:
    // cargo test -p nectarlink-vcam --lib -- --include-ignored

    #[test]
    #[ignore = "requires D3D11 cross-process texture sharing"]
    fn shared_gpu_cpu_output_contention_stop_and_padded_height() {
        let (mut publisher, mut reader) = pair();
        let device = device().unwrap();
        let (a, sa) = texture(&device, 160, 128, 80);
        let out = cpu_buffer(160, 120);
        assert!(publisher.publish(&a, 0, &sa, (160, 120), false, false).unwrap());
        assert!(reader.write_nv12(&out, 160, 120).unwrap());
        let data = pixels(&out);
        assert!(data[..160 * 120].iter().all(|b| *b == 80));
        assert!(data[160 * 120..].iter().all(|b| *b == 128));
        let (b, sb) = texture(&device, 160, 128, 160);
        assert!(publisher.publish(&b, 0, &sb, (160, 120), false, false).unwrap());
        let d = Descriptor::parse(&publisher.mapping.as_ref().unwrap().read_control().unwrap().1).unwrap();
        let mutex = &publisher.slots[d.slot as usize].mutex;
        assert!(acquire(mutex, 1).unwrap());
        let peer: ID3D11Texture2D = unsafe {
            reader
                .device
                .as_ref()
                .unwrap()
                .cast::<ID3D11Device1>()
                .unwrap()
                .OpenSharedResourceByName(&d.name(), ACCESS)
                .unwrap()
        };
        assert!(!acquire(&peer.cast().unwrap(), 1).unwrap());
        assert!(reader.write_nv12(&out, 160, 120).unwrap());
        assert!(pixels(&out)[..160 * 120].iter().all(|b| *b == 80));
        unsafe {
            mutex.ReleaseSync(1).unwrap();
        }
        assert!(reader.write_nv12(&out, 160, 120).unwrap());
        assert!(pixels(&out)[..160 * 120].iter().all(|b| *b == 160));
        publisher.idle();
        assert!(!reader.write_nv12(&out, 160, 120).unwrap());
    }
    #[test]
    fn shared_gpu_to_allocator_gpu_surface_preserves_pixels() {
        let (mut publisher, mut reader) = pair();
        let producer = device().unwrap();
        let consumer = device().unwrap();
        let (src, sample) = texture(&producer, 160, 128, 90);
        assert!(publisher.publish(&src, 0, &sample, (160, 120), false, false).unwrap());
        let (dst, _) = texture(&consumer, 160, 120, 16);
        let buffer = unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &dst, 0, false).unwrap() };
        assert!(reader.write_nv12(&buffer, 160, 120).unwrap());
        assert!(reader.cpu.is_empty(), "GPU output must not read pixels back to the CPU");
        let data = pixels(&buffer);
        assert!(data[..160 * 120].iter().all(|b| *b == 90));
        assert!(data[160 * 120..].iter().all(|b| *b == 128));
    }
    #[test]
    #[ignore = "requires D3D11 cross-process texture sharing"]
    fn nonstandard_colors_and_scaled_rgb_clients_keep_a_cpu_fallback() {
        let (mut publisher, mut reader) = pair();
        let producer = device().unwrap();
        let (src, sample) = texture(&producer, 160, 120, 16);
        assert!(publisher.publish(&src, 0, &sample, (160, 120), true, true).unwrap());
        assert!(!reader.write_nv12(&cpu_buffer(160, 120), 160, 120).unwrap());
        let mut rgb = vec![0; 160 * 120 * 4];
        assert!(reader.read_bgrx(160, 120, &mut rgb).unwrap());
        assert_eq!(&rgb[..4], &[16, 16, 16, 255]);
        let mut scaled = vec![0; 320 * 240 * 4];
        assert!(reader.read_bgrx(320, 240, &mut scaled).unwrap());
        assert_eq!(&scaled[..4], &[16, 16, 16, 255]);
        reader.disable();
        assert!(publisher.publish(&src, 0, &sample, (160, 120), false, false).is_err());
        assert!(!publisher.publish(&src, 0, &sample, (160, 120), false, false).unwrap());
    }

    #[test]
    #[ignore = "requires D3D11 shared textures and GPU video scaling"]
    fn gpu_video_scaling_keeps_pixels_on_gpu_and_letterboxes() {
        let (mut publisher, mut reader) = pair();
        let producer = device().unwrap();
        let consumer = device().unwrap();
        let (src, sample) = texture(&producer, 160, 128, 90);
        assert!(publisher.publish(&src, 0, &sample, (160, 120), false, false).unwrap());
        let mut destination = None;
        unsafe {
            consumer
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: 320,
                        Height: 180,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_NV12,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    },
                    None,
                    Some(&mut destination),
                )
                .unwrap();
        }
        let buffer = unsafe {
            MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &destination.unwrap(), 0, false).unwrap()
        };
        assert!(reader.write_nv12(&buffer, 320, 180).unwrap());
        assert!(reader.cpu.is_empty(), "GPU scaling must avoid CPU readback");
        let bytes = pixels(&buffer);
        assert_eq!(bytes[0], 16);
        assert_eq!(bytes[100], 90);
        assert!(bytes[320 * 180..].iter().all(|b| *b == 128));
    }
    #[test]
    fn rejects_malformed_or_oversized_descriptors() {
        assert!(Descriptor::parse(&[]).is_none());
        let mut d = Descriptor {
            pid: 1,
            slot: 0,
            epoch: 1,
            coded_w: 160,
            coded_h: 120,
            width: 160,
            height: 120,
            colors: 0,
            frame: 1,
            adapter: 1,
        };
        assert_eq!(Descriptor::parse(&d.bytes()), Some(d));
        d.width = 161;
        assert!(Descriptor::parse(&d.bytes()).is_none());
        d.width = 160;
        d.coded_w = MAX_WIDTH + 2;
        assert!(Descriptor::parse(&d.bytes()).is_none());
    }

    #[test]
    fn cross_process_gpu_producer() {
        let Ok(name) = std::env::var("NECTARLINK_GPU_TEST_CONTROL") else { return };
        let mut publisher = GpuPublisher::new();
        publisher.mapping = Some(SharedFrameMapping::open_existing_control(&name).unwrap());
        publisher.mapping_global = true;
        let device = device().unwrap();
        for frame in 1..=5 {
            let (source, sample) = texture(&device, 160, 128, frame * 30);
            assert!(publisher.publish(&source, 0, &sample, (160, 120), false, false).unwrap());
            let started = Instant::now();
            while publisher.stats().unwrap().1 < u32::from(frame) {
                assert!(started.elapsed() < Duration::from_secs(5), "peer never received GPU frame");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    #[ignore = "requires D3D11 cross-process texture sharing"]
    fn separate_process_opens_and_verifies_shared_gpu_frames() {
        let name = format!("Local\\NectarlinkGpuCrossProcess_{}", std::process::id());
        let mut reader = reader_with_mapping(&name);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .creation_flags(0x08000000)
            .args(["--exact", "gpu::tests::cross_process_gpu_producer", "--nocapture"])
            .env("NECTARLINK_GPU_TEST_CONTROL", name)
            .spawn()
            .unwrap();
        let out = cpu_buffer(160, 120);
        let mut last = 0;
        let started = Instant::now();
        while last < 150 {
            if reader.write_nv12(&out, 160, 120).unwrap() {
                let bytes = pixels(&out);
                let value = bytes[0];
                assert!(bytes[..160 * 120].iter().all(|b| *b == value));
                assert!(bytes[160 * 120..].iter().all(|b| *b == 128));
                last = value;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "cross-process GPU transfer stalled");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(child.wait().unwrap().success());
    }

    #[test]
    #[ignore = "requires D3D11 cross-process texture sharing"]
    fn native_nv12_scaling_and_letterbox_avoid_rgb_materialization() {
        let (mut publisher, mut reader) = pair();
        let producer = device().unwrap();
        let (src, sample) = texture(&producer, 160, 120, 90);
        assert!(publisher.publish(&src, 0, &sample, (160, 120), false, false).unwrap());
        let output = cpu_buffer(320, 180);
        assert!(reader.write_nv12(&output, 320, 180).unwrap());
        assert!(reader.rgb.is_empty(), "NV12 scaling must not convert through RGB");
        let bytes = pixels(&output);
        assert_eq!(bytes[0], 16);
        assert_eq!(bytes[40], 90);
        assert_eq!(bytes[279], 90);
        assert_eq!(bytes[280], 16);
        assert!(bytes[320 * 180..].iter().all(|b| *b == 128));
    }
}
