use crate::{
    hotplug::HotplugEvent,
    maybe_future::{blocking::Blocking, MaybeFuture, Ready},
    platform::linux_usbfs::device::LinuxDevice,
    transfer::internal::Notify,
    DeviceInfo, Error, ErrorKind, InterfaceInfo,
};

use std::{
    collections::VecDeque,
    future::IntoFuture,
    sync::{Arc, Mutex, OnceLock, Weak},
    task::{self, Poll, Waker},
};

use log::debug;

use jni::{
    objects::{GlobalRef, JObject},
    sys::jint,
    JNIEnv,
};
use jni_min_helper::*;

pub type DeviceId = i32;
pub type JniGlobalRef = Arc<GlobalRef>;

const USB_SERVICE: &str = "usb";
const ACTION_USB_DEVICE_ATTACHED: &str = "android.hardware.usb.action.USB_DEVICE_ATTACHED";
const ACTION_USB_DEVICE_DETACHED: &str = "android.hardware.usb.action.USB_DEVICE_DETACHED";
const EXTRA_DEVICE: &str = "device";
const ACTION_USB_PERMISSION: &str = "rust.android_usbser.USB_PERMISSION"; // custom
const EXTRA_PERMISSION_GRANTED: &str = "permission";

/// Maps *unexpected* JNI errors to `nusb::Error` of `ErrorKind::Other`.
/// Do not use this convenient conversion if error sorting is needed.
impl From<jni::errors::Error> for Error {
    fn from(err: jni::errors::Error) -> Self {
        use jni::errors::Error::*;
        if let JavaException = err {
            let _ = jni_clear_ex(err); // double ensurance
        }
        Error::new(
            ErrorKind::Other,
            "unexpected Java/JNI error, please check logcat",
        )
    }
}

/// Gets a global reference of `android.hardware.usb.UsbManager`.
fn usb_manager() -> Result<&'static jni::objects::JObject<'static>, Error> {
    static USB_MAN: OnceLock<GlobalRef> = OnceLock::new();

    if android_api_level() < 21 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "nusb requires Android API level 23 (6.0) or newer versions",
        ));
    }

    if let Some(ref_man) = USB_MAN.get() {
        return Ok(ref_man.as_obj());
    }

    let usb_man = jni_with_env(|env| {
        let context = android_context();
        let usb_service_id = USB_SERVICE.new_jobject(env)?;
        let usb_man = env
            .call_method(
                context,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[(&usb_service_id).into()],
            )
            .get_object(env)?;

        let result = if !usb_man.is_null() {
            Ok(env.new_global_ref(&usb_man)?)
        } else {
            Err(Error::new(
                ErrorKind::Unsupported,
                "USB system service not found",
            ))
        };
        Ok(result)
    })??;

    let _ = USB_MAN.set(usb_man.clone());
    Ok(USB_MAN.get().unwrap().as_obj())
}

/// *(Android-only)* Checks if the Android context is an activity opened by an intent of
/// `android.hardware.usb.action.USB_DEVICE_ATTACHED`. If so, it returns the `DeviceInfo`
/// for the caller to open the device without the need of permission request.
///
/// This requires the `ndk_context` crate to be initialized correctly.
pub fn check_startup_intent() -> Option<DeviceInfo> {
    let _ = usb_manager().ok()?;

    // Note: `getIntent()` and `setIntent()` are functions of `Activity` (not `Context`)
    let dev_info = jni_with_env(|env| {
        let activity = android_context();

        // the Intent instance is taken from Activity by getIntent()
        let intent_startup = env
            .call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])
            .get_object(env)?;
        // checks if the action of current intent is ACTION_USB_DEVICE_ATTACHED
        let action_startup = BroadcastReceiver::get_intent_action(&intent_startup, env)?;
        if action_startup.trim() != ACTION_USB_DEVICE_ATTACHED {
            return Ok(None);
        }
        Ok(get_extra_device(env, &intent_startup).ok())
    })
    .ok()??;
    if dev_info.check_connection() && dev_info.has_permission().ok()? {
        Some(dev_info)
    } else {
        None
    }
}

pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    Ready((|| {
        let usb_man = usb_manager()?;
        let mut devices = Vec::new();
        jni_with_env(|env| {
            let ref_dev_list = env
                .call_method(usb_man, "getDeviceList", "()Ljava/util/HashMap;", &[])
                .get_object(env)?;
            let map_dev = env.get_map(&ref_dev_list)?;
            let mut iter_dev = map_dev.iter(env)?;
            while let Some((name, dev)) = iter_dev.next(env)? {
                devices.push(build_device_info(env, &dev)?);
                drop((env.auto_local(name), env.auto_local(dev)));
            }
            Ok(())
        })?;
        Ok(devices.into_iter())
    })())
}

fn build_device_info(
    env: &mut JNIEnv,
    dev: &JObject<'_>,
) -> Result<DeviceInfo, jni::errors::Error> {
    // These functions call java methods without any parameter.
    fn get_int_val(
        env: &mut JNIEnv,
        dev: &JObject<'_>,
        method: &str,
    ) -> Result<jint, jni::errors::Error> {
        env.call_method(dev, method, "()I", &[]).get_int()
    }
    fn get_string_val(
        env: &mut JNIEnv,
        dev: &JObject<'_>,
        method: &str,
    ) -> Result<String, jni::errors::Error> {
        env.call_method(dev, method, "()Ljava/lang/String;", &[])
            .get_object(env)
            .and_then(|o| o.get_string(env))
    }

    // XXX: this call returns an error on old Android versions (API Level < 23)
    let version = get_string_val(env, dev, "getVersion")?;
    // Note: on PC, `bcdUSB 1.10` shown in `lsusb` corresponds to raw value `usb_version: 0x0110`,
    // but here `getVersion` returns `1.16`; is the "buggy" code below dealing with an Android bug?
    let ver_parser = |version: &str| {
        let mut ver_iter = version.split('.').map(|v| v.trim().parse());
        Some((ver_iter.next()?.ok()?, ver_iter.next()?.ok()?))
    };
    let (ver_major, ver_minor): (u16, u16) = ver_parser(&version).unwrap_or_else(|| {
        log::warn!("Unable to parse device USB version '{version}'");
        (0, 0)
    });

    Ok(DeviceInfo {
        device_id: get_int_val(env, dev, "getDeviceId")? as i32,
        jni_global_ref: Arc::new(env.new_global_ref(dev)?),
        vendor_id: get_int_val(env, dev, "getVendorId")? as u16,
        product_id: get_int_val(env, dev, "getProductId")? as u16,
        usb_version: (ver_major << 8) | ver_minor,
        class: get_int_val(env, dev, "getDeviceClass")? as u8,
        subclass: get_int_val(env, dev, "getDeviceSubclass")? as u8,
        protocol: get_int_val(env, dev, "getDeviceProtocol")? as u8,
        speed: None,
        manufacturer_string: get_string_val(env, dev, "getManufacturerName").ok(),
        product_string: get_string_val(env, dev, "getProductName").ok(),
        serial_number: Arc::new(OnceLock::new()),
        interfaces: {
            let num_interfaces = get_int_val(env, dev, "getInterfaceCount")? as u8;
            let mut interfaces = Vec::new();
            for i in 0..num_interfaces {
                let interface = env
                    .call_method(
                        dev,
                        "getInterface",
                        "(I)Landroid/hardware/usb/UsbInterface;",
                        &[(i as jint).into()],
                    )
                    .get_object(env)?;
                interfaces.push(InterfaceInfo {
                    interface_number: get_int_val(env, &interface, "getId")? as u8,
                    class: get_int_val(env, &interface, "getInterfaceClass")? as u8,
                    subclass: get_int_val(env, &interface, "getInterfaceSubclass")? as u8,
                    protocol: get_int_val(env, &interface, "getInterfaceProtocol")? as u8,
                    interface_string: get_string_val(env, &interface, "getName").ok(),
                });
            }
            interfaces.sort_unstable_by_key(|i| i.interface_number);
            interfaces
        },
    })
}

impl DeviceInfo {
    /// Checks if the device is still in the list of connected devices.
    fn check_connection(&self) -> bool {
        let Ok(usb_man) = usb_manager() else {
            return false;
        };
        jni_with_env(|env| {
            let ref_dev_list = env
                .call_method(usb_man, "getDeviceList", "()Ljava/util/HashMap;", &[])
                .get_object(env)?;
            let map_dev = env.get_map(&ref_dev_list)?;
            let mut iter_dev = map_dev.iter(env)?;
            while let Some((name, dev)) = iter_dev.next(env)? {
                let dev_id = env.call_method(&dev, "getDeviceId", "()I", &[]).get_int()?;
                drop((env.auto_local(name), env.auto_local(dev)));
                if dev_id == self.device_id {
                    return Ok(true);
                }
            }
            Ok(false)
        })
        .unwrap_or(false)
    }

    pub(crate) fn get_serial_number(&self) -> Option<&str> {
        if self.serial_number.get().is_none() {
            let _ = jni_with_env(|env| {
                let dev = self.jni_global_ref.as_obj();
                let clear_ex = if android_api_level() < 29 {
                    jni_clear_ex
                } else {
                    // Avoid printing `java.lang.SecurityException: User has not given permission...` in logcat
                    jni_clear_ex_silent
                };
                let serial_num = env
                    .call_method(dev, "getSerialNumber", "()Ljava/lang/String;", &[])
                    .map_err(clear_ex)
                    .get_object(env)
                    .and_then(|o| o.get_string(env))?;
                let _ = self.serial_number.set(serial_num);
                Ok(())
            });
        };
        self.serial_number.get().map(|s| s.as_str())
    }
}

pub fn has_permission(dev_info: &DeviceInfo) -> Result<bool, Error> {
    let usb_man = usb_manager()?;
    Ok(jni_with_env(|env| {
        env.call_method(
            usb_man,
            "hasPermission",
            "(Landroid/hardware/usb/UsbDevice;)Z",
            &[dev_info.jni_global_ref.as_obj().into()],
        )
        .get_boolean()
    })?)
}

pub fn request_permission(dev_info: &DeviceInfo) -> Result<Option<PermissionRequest>, Error> {
    if !dev_info.check_connection() {
        return Err(Error::new(
            ErrorKind::Disconnected,
            "the device has been disconnected",
        ));
    }
    if dev_info.has_permission()? {
        return Ok(None);
    }

    let usb_man = usb_manager()?;
    jni_with_env(|env| {
        let context = android_context();

        let str_perm = ACTION_USB_PERMISSION.new_jobject(env)?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[(&str_perm).into()],
            )
            .auto_local(env)?;

        let flags = if android_api_level() < 31 {
            0 // should it be FLAG_IMMUTABLE since API 23?
        } else {
            0x02000000 // FLAG_MUTABLE (since API 31, Android 12)
        };
        let pending = env
            .call_static_method(
                "android/app/PendingIntent",
                "getBroadcast",
                "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                &[context.into(), 0_i32.into(), (&intent).into(), flags.into()],
            )
            .get_object(env)?;

        env.call_method(
            usb_man,
            "requestPermission",
            "(Landroid/hardware/usb/UsbDevice;Landroid/app/PendingIntent;)V",
            &[dev_info.jni_global_ref.as_obj().into(), (&pending).into()],
        )
        .clear_ex()?;

        Ok(())
    })?;

    if dev_info.has_permission()? {
        return Ok(None); // almost impossible
    }
    Ok(Some(PermissionRequest::build(dev_info)?))
}

/// Android-specific: Represents an ongoing permission request.
pub struct PermissionRequest {
    dev_info: DeviceInfo,
    _receiver: BroadcastReceiver, // deregisters on dropping
    inner: Arc<PermissionRequestInner>,
}

struct PermissionRequestInner {
    notify: Notify,
    result: Mutex<Option<bool>>,
}

impl PermissionRequest {
    fn build(dev_info: &DeviceInfo) -> Result<Self, jni::errors::Error> {
        let inner = Arc::new(PermissionRequestInner {
            notify: Notify::new(),
            result: Mutex::new(None),
        });

        let inner_weak = Arc::downgrade(&inner);
        let dev_info_2 = dev_info.clone();
        let receiver = BroadcastReceiver::build(move |env, _, intent| {
            Self::on_receive(&inner_weak, &dev_info_2, env, intent)
        })?;
        receiver.register_for_action(ACTION_USB_PERMISSION)?;

        Ok(Self {
            dev_info: dev_info.clone(),
            _receiver: receiver,
            inner,
        })
    }

    /// Returns a reference of the associated `DeviceInfo` which can be cloned.
    pub fn device_info(&self) -> &DeviceInfo {
        &self.dev_info
    }

    /// Checks the boolean result if the request has completed.
    pub fn responsed(&self) -> Option<bool> {
        *self.inner.result.lock().unwrap()
    }

    fn on_receive<'a>(
        inner_weak: &Weak<PermissionRequestInner>,
        dev_expected: &DeviceInfo,
        env: &mut JNIEnv<'a>,
        intent: &JObject<'a>,
    ) -> Result<(), jni::errors::Error> {
        let dev = get_extra_device(env, intent)?;
        if dev.id() == dev_expected.id() {
            let extra_name = EXTRA_PERMISSION_GRANTED.new_jobject(env)?;
            let granted = env
                .call_method(
                    intent,
                    "getBooleanExtra",
                    "(Ljava/lang/String;Z)Z",
                    &[(&extra_name).into(), false.into()],
                )
                .get_boolean()
                .unwrap_or(false);
            let Some(inner) = inner_weak.upgrade() else {
                return Ok(());
            };
            inner.result.lock().unwrap().replace(granted);
            inner.notify.notify();
        }
        Ok(())
    }
}

impl std::future::Future for PermissionRequest {
    type Output = bool;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Self::Output> {
        self.inner.notify.subscribe(cx);
        if let Some(result) = self.responsed() {
            task::Poll::Ready(result)
        } else {
            task::Poll::Pending
        }
    }
}

impl MaybeFuture for PermissionRequest {
    fn wait(self) -> Self::Output {
        self.inner.notify.wait(|| self.responsed())
    }
}

pub fn open_device(d: &DeviceInfo) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
    Ready((|| {
        if !d.check_connection() {
            return Err(Error::new(
                ErrorKind::Disconnected,
                "the device has been disconnected",
            ));
        }
        if !d.has_permission()? {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "please call `DeviceInfo::request_permission` for Android",
            ));
        }
        jni_with_env(|env| {
            let usb_man = match usb_manager() {
                Ok(man) => man,
                Err(e) => return Ok(Err(e)),
            };
            // Another thread executing `from_device_info` will block here, until the guard
            // for the current thread is dropped after `LinuxDevice::create_inner`.
            let _guard = env.lock_obj(usb_man).unwrap();

            let conn = env
                .call_method(
                    usb_man,
                    "openDevice",
                    "(Landroid/hardware/usb/UsbDevice;)Landroid/hardware/usb/UsbDeviceConnection;",
                    &[(&*d.jni_global_ref).into()],
                )
                .get_object(env)?;
            if conn.is_null() {
                return Ok(Err(Error::new(
                    ErrorKind::NotFound,
                    "`UsbManager.openDevice()` failed`",
                )));
            }
            let raw_fd = env
                .call_method(&conn, "getFileDescriptor", "()I", &[])
                .get_int()?;

            // Safety: `close()` is not called automatically when the JNI `AutoLocal` of `conn`
            // and the corresponding Java object is destroyed. (check `UsbDeviceConnection` source)
            use std::os::fd::*;
            debug!("Wrapping fd {raw_fd} as usbfs device");
            let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd as RawFd) };
            Ok(LinuxDevice::create_inner(owned_fd))
        })?
    })())
}

#[derive(Debug)]
pub struct HotplugWatch {
    _receiver: BroadcastReceiver, // deregisters on dropping
    inner: Arc<HotplugWatchInner>,
}

#[derive(Debug)]
struct HotplugWatchInner {
    waker: Mutex<Option<Waker>>,
    events: Mutex<VecDeque<HotplugEvent>>,
}

impl HotplugWatch {
    pub(crate) fn new() -> Result<Self, Error> {
        let inner = Arc::new(HotplugWatchInner {
            waker: Mutex::new(None),
            events: Mutex::new(VecDeque::new()),
        });
        let inner_weak = Arc::downgrade(&inner);
        let receiver = BroadcastReceiver::build(move |env, _, intent| {
            Self::on_receive(&inner_weak, env, intent)
        })?;
        receiver.register_for_action(ACTION_USB_DEVICE_ATTACHED)?;
        receiver.register_for_action(ACTION_USB_DEVICE_DETACHED)?;
        Ok(Self {
            _receiver: receiver,
            inner,
        })
    }

    pub(crate) fn poll_next(&mut self, cx: &mut std::task::Context<'_>) -> Poll<HotplugEvent> {
        self.inner.waker.lock().unwrap().replace(cx.waker().clone());
        let event = self.inner.events.lock().unwrap().pop_front();
        match event {
            Some(event) => Poll::Ready(event),
            None => Poll::Pending,
        }
    }

    // Note: `BroadcastReceiver` ignores any `jni::errors::Error` returned from here;
    // but the closure required by `BroadcastReceiver` needs to return a result with such error type,
    // this is merely designed for JNI calls made inside the closure to get rid of `unwrap()` calls.
    fn on_receive<'a>(
        inner_weak: &Weak<HotplugWatchInner>,
        env: &mut JNIEnv<'a>,
        intent: &JObject<'a>,
    ) -> Result<(), jni::errors::Error> {
        if intent.is_null() {
            return Ok(()); // almost impossible
        }
        let Some(inner) = inner_weak.upgrade() else {
            return Ok(());
        };
        let Ok(action) = BroadcastReceiver::get_intent_action(intent, env) else {
            return Ok(()); // almost impossible
        };
        use HotplugEvent::*;
        match action.trim() {
            ACTION_USB_DEVICE_ATTACHED => {
                let dev = get_extra_device(env, intent)?;
                inner.events.lock().unwrap().push_back(Connected(dev));
            }
            ACTION_USB_DEVICE_DETACHED => {
                let id = get_extra_device(env, intent)?.id();
                inner.events.lock().unwrap().push_back(Disconnected(id));
            }
            _ => (),
        }
        if let Some(w) = inner.waker.lock().unwrap().take() {
            w.wake()
        }
        Ok(())
    }
}

fn get_extra_device(
    env: &mut JNIEnv<'_>,
    intent: &JObject<'_>,
) -> Result<DeviceInfo, jni::errors::Error> {
    let extra_device = EXTRA_DEVICE.new_jobject(env)?;
    let java_dev = env
        .call_method(
            intent,
            "getParcelableExtra",
            // XXX: this is deprecated in API 33 and above without the class parameter.
            "(Ljava/lang/String;)Landroid/os/Parcelable;",
            &[(&extra_device).into()],
        )
        .get_object(env)?;

    if !java_dev.is_null() {
        build_device_info(env, &java_dev)
    } else {
        Err(jni::errors::Error::NullPtr(
            "Unexpected: the Intent has no EXTRA_DEVICE",
        ))
    }
}
