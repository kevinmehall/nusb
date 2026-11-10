mod device;
mod enumeration;
mod hotplug;

pub use enumeration::{list_devices, request_device};

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
        _ => Err(TransferError::Unknown(0)),
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

fn rejection_field(value: &JsValue, field: &str) -> Option<String> {
    Reflect::get(value, &JsValue::from_str(field))
        .ok()
        .and_then(|value| value.as_string())
        .filter(|value| !value.is_empty())
}

pub fn js_value_to_error(value: JsValue) -> Error {
    // DOMException is not a JavaScript Error in every browser. Read the standard
    // fields without assuming a constructor or retaining a thread-bound JsValue.
    let name = rejection_field(&value, "name");
    let message = rejection_field(&value, "message").or_else(|| value.as_string());
    let detail = match (name, message) {
        (Some(name), Some(message)) => format!("{name}: {message}"),
        (Some(name), None) => name,
        (None, Some(message)) => message,
        (None, None) => format!("{value:?}"),
    };
    Error::new(ErrorKind::Other, format!("WebUSB error: {detail}")).log_debug()
}

pub fn js_value_to_transfer_error(value: JsValue) -> TransferError {
    log::warn!(
        "WebUSB transfer error: {}",
        js_value_to_error(value.clone())
    );
    match rejection_field(&value, "name").as_deref() {
        Some("NetworkError") => TransferError::Disconnected,
        _ => TransferError::Fault,
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn preserves_error_dom_exception_and_string_rejections() {
        let error = js_sys::Error::new("claim failed");
        assert_eq!(
            js_value_to_error(error.into()).to_string(),
            "WebUSB error: Error: claim failed"
        );
        let exception =
            js_sys::eval("new DOMException('device left while claiming', 'NetworkError')").unwrap();
        let owned = js_value_to_error(exception.clone());
        assert_eq!(
            owned.to_string(),
            "WebUSB error: NetworkError: device left while claiming"
        );
        assert_eq!(owned.clone().to_string(), owned.to_string());
        assert!(matches!(
            js_value_to_transfer_error(exception),
            TransferError::Disconnected
        ));
        assert_eq!(
            js_value_to_error(JsValue::from_str("permission denied")).to_string(),
            "WebUSB error: permission denied"
        );
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Error>();
    }

    #[wasm_bindgen_test]
    fn unusual_rejections_do_not_panic_or_erase_available_fields() {
        let error =
            js_sys::eval("({message:'retained', get name() {throw new Error('getter')}})").unwrap();
        assert_eq!(
            js_value_to_error(error).to_string(),
            "WebUSB error: retained"
        );
        assert!(js_value_to_error(JsValue::NULL)
            .to_string()
            .starts_with("WebUSB error:"));
        assert!(js_value_to_error(JsValue::UNDEFINED)
            .to_string()
            .starts_with("WebUSB error:"));
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
