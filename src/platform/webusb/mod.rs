mod device;
mod enumeration;
mod hotplug;
mod transfer;

pub(crate) use transfer::TransferData;

pub use enumeration::{list_buses, list_devices};

pub(crate) use device::WebusbDevice as Device;
pub(crate) use device::WebusbInterface as Interface;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub(crate) id: usize,
}

impl DeviceId {
    pub(crate) fn from_device(device: &UsbDevice) -> Self {
        let key = JsValue::from_str("nusbUniqueId");
        static INCREMENT: std::sync::LazyLock<std::sync::Mutex<usize>> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(0));
        let id = if let Ok(device_id) = Reflect::get(device, &key) {
            device_id
                .as_f64()
                .expect("Expected an integer ID. This is a bug. Please report it.")
                as usize
        } else {
            let mut lock = INCREMENT
                .lock()
                .expect("this should never be poisoned as we do not have multiple threads");
            *lock += 1;
            Reflect::set(device, &key, &JsValue::from_f64(*lock as f64))
                .expect("Could not set ID on JS object. This is a bug. Please report it.");
            *lock
        };

        DeviceId { id }
    }
}

pub(crate) use hotplug::WebusbHotplugWatch as HotplugWatch;
use web_sys::js_sys::Reflect;
use web_sys::wasm_bindgen::JsValue;
use web_sys::UsbDevice;
