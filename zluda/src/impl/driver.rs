use cuda_types::cuda::*;
#[cfg(feature = "amd")]
use hip_runtime_sys::*;
#[cfg(feature = "intel")]
use ze_runtime_sys::*;
use std::{
    ffi::{CStr, CString},
    mem, slice,
    sync::OnceLock,
    cell::RefCell,
    collections::HashMap,
    ptr::NonNull,
};

use crate::r#impl::context;

use super::LiveCheck;

// Define trait for converting ze_result_t to CUresult
#[cfg(feature = "intel")]
trait ResultExt {
    fn to_cuda_result<T>(self, value: T) -> Result<T, CUerror>;
}

#[cfg(feature = "intel")]
impl ResultExt for ze_result_t {
    fn to_cuda_result<T>(self, value: T) -> Result<T, CUerror> {
        match self {
            ze_result_t::ZE_RESULT_SUCCESS => Ok(value),
            ze_result_t::ZE_RESULT_ERROR_DEVICE_LOST => Err(CUerror::DEVICE_NOT_LICENSED),
            ze_result_t::ZE_RESULT_ERROR_OUT_OF_HOST_MEMORY => Err(CUerror::OUT_OF_MEMORY),
            ze_result_t::ZE_RESULT_ERROR_OUT_OF_DEVICE_MEMORY => Err(CUerror::OUT_OF_MEMORY),
            _ => Err(CUerror::UNKNOWN),
        }
    }
}


pub(crate) struct GlobalState {
    pub devices: Vec<Device>,
}
unsafe impl Send for GlobalState {}
unsafe impl Sync for GlobalState {}

pub(crate) struct Device {
    pub(crate) _comgr_isa: CString,
    primary_context: LiveCheck<context::Context>,
}

impl Device {
    pub(crate) fn primary_context<'a>(&'a self) -> (&'a context::Context, CUcontext) {
        unsafe {
            (
                self.primary_context.data.assume_init_ref(),
                self.primary_context.as_handle(),
            )
        }
    }
}
pub(crate) fn device(dev: i32) -> Result<&'static Device, CUerror> {
    global_state()?
        .devices
        .get(dev as usize)
        .ok_or(CUerror::INVALID_DEVICE)
}
#[cfg(feature = "amd")]
pub(crate) fn global_state() -> Result<&'static GlobalState, CUerror> {
    static GLOBAL_STATE: OnceLock<Result<GlobalState, CUerror>> = OnceLock::new();
    fn cast_slice<'a>(bytes: &'a [i8]) -> &'a [u8] {
        unsafe { slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len()) }
    }
    GLOBAL_STATE
        .get_or_init(|| {
            let mut device_count = 0;
            unsafe { hipGetDeviceCount(&mut device_count) }?;
            Ok(GlobalState {
                devices: (0..device_count)
                    .map(|i| {
                        let mut props = unsafe { mem::zeroed() };
                        unsafe { hipGetDevicePropertiesR0600(&mut props, i) }?;
                        Ok::<_, CUerror>(Device {
                            _comgr_isa: CStr::from_bytes_until_nul(cast_slice(
                                &props.gcnArchName[..],
                            ))
                            .map_err(|_| CUerror::UNKNOWN)?
                            .to_owned(),
                            primary_context: LiveCheck::new(context::Context::new(i)),
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            })
        })
        .as_ref()
        .map_err(|e| *e)
}

#[cfg(feature = "intel")]
pub(crate) fn global_state() -> Result<&'static GlobalState, CUerror> {
    static GLOBAL_STATE: OnceLock<Result<GlobalState, CUerror>> = OnceLock::new();
    
    GLOBAL_STATE
        .get_or_init(|| {
            // Initialize Level Zero
            unsafe { zeInit(0) }
                .to_cuda_result(())?;
            
            // Get driver count
            let mut driver_count = 0;
            unsafe { 
                zeDriverGet(&mut driver_count, std::ptr::null_mut())
                    .to_cuda_result(())?
            };
            
            if driver_count == 0 {
                return Err(CUerror::NO_DEVICE);
            }
            
            // Get drivers
            let mut drivers = vec![std::ptr::null_mut(); driver_count as usize];
            unsafe {
                zeDriverGet(&mut driver_count, *drivers.as_mut_ptr())
                    .to_cuda_result(())?
            };
            
            // Get device count for the first driver
            let mut device_count = 0;
            unsafe {
                zeDeviceGet(*drivers[0], &mut device_count, std::ptr::null_mut())
                    .to_cuda_result(())?
            };
            
            let mut devices_vec = Vec::new();
            
            for i in 0..device_count as i32 {
                let mut devices = vec![std::ptr::null_mut(); device_count as usize];
                
                // Get the devices
                unsafe {
                    zeDeviceGet(*drivers[0], &mut device_count, *devices.as_mut_ptr())
                        .to_cuda_result(())?;
                }
                
                let device = if i < devices.len() as i32 {
                    ze_device_handle_t(devices[i as usize] as *mut _)
                } else {
                    return Err(CUerror::INVALID_DEVICE);
                };
                
                // Get device properties
                let mut props: ze_device_properties_t = unsafe { mem::zeroed() };
                props.stype = ze_structure_type_t::ZE_STRUCTURE_TYPE_DEVICE_PROPERTIES;
                
                unsafe {
                    zeDeviceGetProperties(device, &mut props)
                        .to_cuda_result(())?;
                }
                
                // Create a string from device name - ensure we only include valid characters
                let name_bytes = props.name.iter()
                    .take_while(|&&c| c != 0)
                    .map(|&c| c as u8)
                    .collect::<Vec<_>>();
                
                let comgr_isa = CString::new(name_bytes)
                    .map_err(|_| CUerror::UNKNOWN)?;
                
                // Create context
                let mut ctx = context::Context::new(device);
                
                // Initialize the context
                ctx.initialize()?;
                
                // Create the device and store it in our results
                let device_box = Box::new(Device {
                    _comgr_isa: comgr_isa,
                    primary_context: LiveCheck::new(ctx),
                });
                
                // 使用克隆，这样原始值不会被移动
                let device_box_clone = Box::new(*device_box.clone());
                
                // 存储指针到设备映射
                DEVICES_ZE.with(|map| {
                    let mut map = map.borrow_mut();
                    let device_ptr = unsafe { NonNull::new_unchecked(Box::into_raw(device_box_clone)) };
                    map.insert(device, device_ptr);
                });
                
                devices_vec.push(*device_box);
            }
            
            Ok(GlobalState {
                devices: devices_vec,
            })
        })
        .as_ref()
        .map_err(|e| *e)
}

#[cfg(feature = "amd")]
pub(crate) fn init(flags: ::core::ffi::c_uint) -> CUresult {
    unsafe { hipInit(flags) }?;
    global_state()?;
    Ok(())
}

#[cfg(feature = "intel")]
pub(crate) fn init(flags: ::core::ffi::c_uint) -> CUresult {
    unsafe { 
        // Initialize Level Zero
        zeInit(0).to_cuda_result(())?;
        
        // Ignore CUDA flags as they don't apply to Level Zero
        let _ = flags;
    }
    
    global_state()?;
    Ok(())
}

#[cfg(feature = "amd")]
pub(crate) fn get_version(version: &mut ::core::ffi::c_int) -> CUresult {
    *version = cuda_types::cuda::CUDA_VERSION as i32;
    Ok(())
}

#[cfg(feature = "intel")]
pub(crate) fn get_version(version: &mut ::core::ffi::c_int) -> CUresult {
    // Return the CUDA version same as AMD implementation
    *version = cuda_types::cuda::CUDA_VERSION as i32;
    Ok(())
}

#[cfg(feature = "intel")]
pub(crate) fn device_ze(dev: ze_device_handle_t) -> Result<&'static Device, CUerror> {
    DEVICES_ZE.with(|map| {
        let map_ref = map.borrow();
        map_ref
            .get(&dev)
            .ok_or(CUerror::INVALID_DEVICE)
            .map(|dev_ptr| unsafe { dev_ptr.as_ref() })
    })
}

#[cfg(feature = "intel")]
thread_local! {
    static DEVICES_ZE: RefCell<HashMap<ze_device_handle_t, NonNull<Device>>> = RefCell::new(HashMap::new());
}
