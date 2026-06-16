mod device;
mod enumeration;
mod hotplug;

pub use enumeration::{list_buses, list_devices};

pub(crate) use device::WebusbDevice as Device;
pub(crate) use device::WebusbEndpoint as Endpoint;
pub(crate) use device::WebusbInterface as Interface;
pub(crate) use hotplug::WebusbHotplugWatch as HotplugWatch;

use web_sys::js_sys;
use web_sys::js_sys::Reflect;
use web_sys::wasm_bindgen::JsCast;
use web_sys::wasm_bindgen::JsValue;
use web_sys::Usb;
pub use web_sys::UsbDevice;
use web_sys::Window;
use web_sys::WorkerGlobalScope;

use crate::error::{Error, ErrorKind};
use crate::transfer::TransferError;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    pub(crate) id: usize,
}

impl DeviceId {
    pub(crate) fn from_device(device: &UsbDevice) -> Self {
        let key = JsValue::from_str("nusbUniqueId");
        static INCREMENT: std::sync::LazyLock<std::sync::Mutex<usize>> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(0));
        let id = if let Some(device_id) = Reflect::get(device, &key).ok().and_then(|v| v.as_f64()) {
            device_id as usize
        } else {
            let mut lock = INCREMENT.lock().unwrap();
            *lock += 1;
            Reflect::set(device, &key, &JsValue::from_f64(*lock as f64)).unwrap();
            *lock
        };

        DeviceId { id }
    }
}

pub fn format_os_error_code(_f: &mut std::fmt::Formatter<'_>, _code: u32) -> std::fmt::Result {
    Ok(())
}

pub(crate) fn webusb_status_to_nusb_transfer_error(
    status: web_sys::UsbTransferStatus,
) -> Result<(), TransferError> {
    match status {
        web_sys::UsbTransferStatus::Ok => Ok(()),
        web_sys::UsbTransferStatus::Stall => Err(TransferError::Stall),
        web_sys::UsbTransferStatus::Babble => Err(TransferError::Fault),
        _ => unreachable!(),
    }
}

pub(crate) fn usb() -> Result<Usb, Error> {
    let usb = if let Ok(window) = js_sys::global().dyn_into::<Window>() {
        window.navigator().usb()
    } else if let Ok(wgs) = js_sys::global().dyn_into::<WorkerGlobalScope>() {
        wgs.navigator().usb()
    } else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "Could not obtain Window or WorkerGlobalScope",
        ));
    };

    if usb.is_undefined() {
        Err(Error::new(
            ErrorKind::Unsupported,
            "WebUSB is not available",
        ))
    } else {
        Ok(usb)
    }
}

pub fn js_value_to_error(value: JsValue) -> Error {
    let err: js_sys::Error = value
        .dyn_into()
        .unwrap_or_else(|_| js_sys::Error::new("error could not be constructed"));
    let msg = err.message().as_string().unwrap_or_default();
    log::warn!("WebUSB error: {msg}");
    Error::new(ErrorKind::Other, "WebUSB error (see logs)")
}

pub fn js_value_to_transfer_error(value: JsValue) -> TransferError {
    let err: js_sys::Error = value
        .dyn_into()
        .unwrap_or_else(|_| js_sys::Error::new("error could not be constructed"));
    match err.name().as_string().as_deref() {
        Some("NetworkError") => TransferError::Disconnected,
        _ => {
            log::debug!("WebUSB transfer error: {:?}", err);
            TransferError::Fault
        }
    }
}

/// Wrap an `async` block so it implements [`MaybeFuture`][crate::MaybeFuture].
///
/// On wasm32, `MaybeFuture` only requires `IntoFuture` (no `wait()`), so this is enough.
pub(crate) struct WebFuture<F>(pub F);

impl<F: std::future::Future> std::future::IntoFuture for WebFuture<F> {
    type Output = F::Output;
    type IntoFuture = std::pin::Pin<Box<F>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.0)
    }
}

impl<F: std::future::Future> crate::MaybeFuture for WebFuture<F> {}
