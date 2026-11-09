use anyhow::{Context, Result, ensure};
use ash::vk;
use std::ffi::CStr;
use wgpu_hal::api::Vulkan;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub core_features: vk::PhysicalDeviceFeatures,
    pub decode_family: u32,
    pub encode_family: u32,
    pub name: String,
    _instance: wgpu::Instance,
}

impl Gpu {
    pub fn new() -> Result<Self> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
            .into_iter()
            .find(|adapter| adapter.get_info().name.contains("RTX 3090"))
            .context("The validated RTX 3090 Vulkan adapter is unavailable")?;
        let name = adapter.get_info().name;
        let features = wgpu::Features::CLEAR_TEXTURE
            | wgpu::Features::FLOAT32_FILTERABLE
            | wgpu::Features::SHADER_INT64;
        ensure!(
            adapter.features().contains(features),
            "Required GPU features are missing"
        );
        let limits = adapter.limits();
        let hal = unsafe { adapter.as_hal::<Vulkan>() }.context("Expected Vulkan backend")?;
        let raw_instance = hal.shared_instance().raw_instance();
        let physical = hal.raw_physical_device();
        let families =
            unsafe { raw_instance.get_physical_device_queue_family_properties(physical) };
        ensure!(
            families[0]
                .queue_flags
                .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE),
            "The wgpu queue family must support graphics and compute"
        );
        let decode_family = families
            .iter()
            .position(|family| {
                family
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_DECODE_KHR)
            })
            .context("No hardware video decode queue")? as u32;
        let encode_family = families
            .iter()
            .position(|family| {
                family
                    .queue_flags
                    .contains(vk::QueueFlags::VIDEO_ENCODE_KHR)
            })
            .context("No hardware video encode queue")? as u32;
        ensure!(
            decode_family != 0 && encode_family != 0 && decode_family != encode_family,
            "This trial requires separate video queue families"
        );
        let extra_extensions: [&'static CStr; 7] = [
            c"VK_KHR_video_queue",
            c"VK_KHR_video_decode_queue",
            c"VK_KHR_video_decode_h264",
            c"VK_KHR_video_encode_queue",
            c"VK_KHR_video_encode_h264",
            c"VK_KHR_video_maintenance1",
            c"VK_KHR_push_descriptor",
        ];
        let supported = unsafe { raw_instance.enumerate_device_extension_properties(physical) }?;
        for required in extra_extensions {
            ensure!(
                supported.iter().any(|extension| unsafe {
                    CStr::from_ptr(extension.extension_name.as_ptr()) == required
                }),
                "Missing device extension {required:?}"
            );
        }
        let mut synchronization = vk::PhysicalDeviceSynchronization2Features::default();
        let mut maintenance = vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::default();
        let mut supported_features = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut synchronization)
            .push_next(&mut maintenance);
        unsafe { raw_instance.get_physical_device_features2(physical, &mut supported_features) };
        ensure!(
            synchronization.synchronization2 != 0 && maintenance.video_maintenance1 != 0,
            "Synchronization2 and video maintenance1 are required"
        );
        let mut enabled_maintenance =
            vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::default().video_maintenance1(true);
        let mut enabled_synchronization = vk::PhysicalDeviceSynchronization2Features {
            p_next: (&mut enabled_maintenance as *mut _) as *mut _,
            synchronization2: vk::TRUE,
            ..Default::default()
        };
        let extra_chain = (&mut enabled_synchronization as *mut _) as *const std::ffi::c_void;
        let mut core_features = vk::PhysicalDeviceFeatures::default();
        let open = unsafe {
            hal.open_with_callback(
                features,
                &limits,
                &wgpu::MemoryHints::MemoryUsage,
                Some(Box::new(|args| {
                    for extension in extra_extensions {
                        if !args.extensions.contains(&extension) {
                            args.extensions.push(extension);
                        }
                    }
                    for family in [decode_family, encode_family] {
                        args.queue_create_infos.push(
                            vk::DeviceQueueCreateInfo::default()
                                .queue_family_index(family)
                                .queue_priorities(&[1.0]),
                        );
                    }
                    args.create_info.p_next = extra_chain;
                    core_features = args.device_features.get_core();
                })),
            )
        }?;
        drop(hal);
        let (device, queue) = unsafe {
            adapter.create_device_from_hal::<Vulkan>(
                open,
                &wgpu::DeviceDescriptor {
                    label: Some("chr shared video/render device"),
                    required_features: features,
                    required_limits: limits,
                    ..Default::default()
                },
            )
        }?;
        Ok(Self {
            device,
            queue,
            core_features,
            decode_family,
            encode_family,
            name,
            _instance: instance,
        })
    }

    pub fn wait(&self) -> Result<()> {
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        Ok(())
    }
}
