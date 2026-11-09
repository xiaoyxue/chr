use crate::gpu::Gpu;
use anyhow::{Context, Result, ensure};
use ash::vk::{self, Handle};
use std::ffi::{CString, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::path::Path;
use wgpu_hal::api::Vulkan;

#[repr(C)]
#[derive(Default, Debug)]
pub struct FrameInfo {
    pub pts: i64,
    pub time_base_num: i32,
    pub time_base_den: i32,
    pub width: u32,
    pub height: u32,
    pub color_mode: u32,
    pub(crate) reserved: u32,
}

unsafe extern "C" {
    fn chr_open(
        input: *const c_char,
        instance: u64,
        physical: u64,
        device: u64,
        get_proc_addr: usize,
        instance_extensions: *const *const c_char,
        instance_count: c_int,
        device_extensions: *const *const c_char,
        device_count: c_int,
        decode_family: u32,
        encode_family: u32,
        features: *const vk::PhysicalDeviceFeatures,
    ) -> *mut c_void;
    fn chr_next(media: *mut c_void, info: *mut FrameInfo) -> c_int;
    fn chr_rewind(media: *mut c_void) -> c_int;
    fn chr_open_output(media: *mut c_void, url: *const c_char, fps: c_int, bitrate: c_int)
    -> c_int;
    fn chr_copy_input(media: *mut c_void, y_image: u64, uv_image: u64) -> c_int;
    fn chr_encode(media: *mut c_void, buffer: u64, sequence: i64) -> c_int;
    fn chr_finish(media: *mut c_void) -> c_int;
    fn chr_lock_queue(media: *mut c_void);
    fn chr_unlock_queue(media: *mut c_void);
    fn chr_stopped() -> c_int;
    fn chr_verify_source(path: *const c_char, sha256: *const c_char) -> c_int;
    fn chr_close(media: *mut c_void);
}

pub struct Media<'gpu> {
    raw: *mut c_void,
    _gpu: PhantomData<&'gpu Gpu>,
}

fn checked(result: c_int, operation: &str) -> Result<()> {
    ensure!(
        result >= 0,
        "{operation} failed ({result}); see FFmpeg diagnostics"
    );
    Ok(())
}

pub fn verify_source(path: &Path, sha256: &str) -> Result<()> {
    let path = CString::new(path.to_str().context("Non-UTF8 video path")?)?;
    let sha256 = CString::new(sha256)?;
    checked(
        unsafe { chr_verify_source(path.as_ptr(), sha256.as_ptr()) },
        "Compressed source SHA256 verification",
    )
}

impl<'gpu> Media<'gpu> {
    pub fn open(gpu: &'gpu Gpu, path: &Path) -> Result<Self> {
        let path = CString::new(path.to_str().context("Non-UTF8 video path")?)?;
        let device = unsafe { gpu.device.as_hal::<Vulkan>() }.context("Expected Vulkan device")?;
        let instance = device.shared_instance();
        let instance_extensions: Vec<_> =
            instance.extensions().iter().map(|s| s.as_ptr()).collect();
        let device_extensions: Vec<_> = device
            .enabled_device_extensions()
            .iter()
            .map(|s| s.as_ptr())
            .collect();
        let raw = unsafe {
            chr_open(
                path.as_ptr(),
                instance.raw_instance().handle().as_raw(),
                device.raw_physical_device().as_raw(),
                device.raw_device().handle().as_raw(),
                instance.entry().static_fn().get_instance_proc_addr as usize,
                instance_extensions.as_ptr(),
                instance_extensions.len() as c_int,
                device_extensions.as_ptr(),
                device_extensions.len() as c_int,
                gpu.decode_family,
                gpu.encode_family,
                &gpu.core_features,
            )
        };
        ensure!(
            !raw.is_null(),
            "Could not initialize GPU-resident FFmpeg decoding"
        );
        Ok(Self {
            raw,
            _gpu: PhantomData,
        })
    }

    pub fn output(&self, url: &str) -> Result<()> {
        let url = CString::new(url)?;
        checked(
            unsafe { chr_open_output(self.raw, url.as_ptr(), 30, 8_000_000) },
            "Output setup",
        )
    }

    pub fn next(&self) -> Result<Option<FrameInfo>> {
        let mut info = FrameInfo::default();
        let result = unsafe { chr_next(self.raw, &mut info) };
        checked(result, "Hardware decode")?;
        Ok((result == 1).then_some(info))
    }

    pub fn rewind(&self) -> Result<()> {
        checked(unsafe { chr_rewind(self.raw) }, "Source rewind")
    }

    pub fn copy_input(&self, y: &wgpu::Texture, uv: &wgpu::Texture) -> Result<()> {
        // A device's as_hal resource guards share a non-recursive lock.
        let y = {
            let handle = unsafe { y.as_hal::<Vulkan>() }.context("Expected Vulkan luma texture")?;
            unsafe { handle.raw_handle().as_raw() }
        };
        let uv = {
            let handle =
                unsafe { uv.as_hal::<Vulkan>() }.context("Expected Vulkan chroma texture")?;
            unsafe { handle.raw_handle().as_raw() }
        };
        checked(
            unsafe { chr_copy_input(self.raw, y, uv) },
            "GPU decoder-to-renderer plane copy",
        )
    }

    pub fn encode(&self, buffer: &wgpu::Buffer, sequence: u64) -> Result<()> {
        let buffer =
            unsafe { buffer.as_hal::<Vulkan>() }.context("Expected Vulkan output buffer")?;
        checked(
            unsafe { chr_encode(self.raw, buffer.raw_handle().as_raw(), sequence as i64) },
            "GPU renderer-to-encoder copy and encode",
        )
    }

    pub fn submit(&self, gpu: &Gpu, command: wgpu::CommandBuffer) {
        unsafe { chr_lock_queue(self.raw) };
        gpu.queue.submit([command]);
        unsafe { chr_unlock_queue(self.raw) };
    }

    pub fn finish(&self) -> Result<()> {
        checked(unsafe { chr_finish(self.raw) }, "Encoder flush")
    }

    pub fn stopped() -> bool {
        unsafe { chr_stopped() != 0 }
    }
}

impl Drop for Media<'_> {
    fn drop(&mut self) {
        unsafe { chr_close(self.raw) };
    }
}
