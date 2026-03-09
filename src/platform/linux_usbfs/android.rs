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

use log::{debug, error};

use jni::{
    jni_sig, jni_str,
    objects::{Global, JMap, JObject, JString},
    sys::jint,
    Env,
};
use jni_min_helper::{android_api_level, android_context, jni_with_env, BroadcastReceiver, Intent};

pub type DeviceId = i32;
pub type JniGlobal = Arc<Global<AndroidUsbDevice<'static>>>;

jni::bind_java_type! {
    AndroidContext => "android.content.Context",
    fields {
        #[allow(non_snake_case)]
        static USB_SERVICE { sig = JString, get = USB_SERVICE },
    },
    methods {
        fn get_system_service(name: JString) -> JObject,
    }
}

jni::bind_java_type! {
    AndroidActivity => "android.app.Activity",
    type_map = {
        AndroidContext => "android.content.Context",
        Intent => "android.content.Intent",
    },
    methods {
        fn get_intent() -> Intent,
    },
    is_instance_of = {
        AndroidContext,
    }
}

jni::bind_java_type! {
    AndroidUsbManager => "android.hardware.usb.UsbManager",
    type_map = {
        JHashMap => "java.util.HashMap",
        AndroidUsbDevice => "android.hardware.usb.UsbDevice",
        AndroidUsbDeviceConnection => "android.hardware.usb.UsbDeviceConnection",
        PendingIntent => "android.app.PendingIntent",
    },
    fields {
        #[allow(non_snake_case)]
        static ACTION_USB_DEVICE_ATTACHED { sig = JString, get = ACTION_USB_DEVICE_ATTACHED },
        #[allow(non_snake_case)]
        static ACTION_USB_DEVICE_DETACHED { sig = JString, get = ACTION_USB_DEVICE_DETACHED },
        #[allow(non_snake_case)]
        static EXTRA_DEVICE { sig = JString, get = EXTRA_DEVICE },
        #[allow(non_snake_case)]
        static EXTRA_PERMISSION_GRANTED { sig = JString, get = EXTRA_PERMISSION_GRANTED },
    },
    methods {
        fn get_device_list() -> JHashMap,
        fn has_permission {
            name = "hasPermission", sig = (device: AndroidUsbDevice) -> jboolean,
        },
        fn request_permission {
            name = "requestPermission", sig = (device: AndroidUsbDevice, pi: PendingIntent),
        },
        fn open_device(device: AndroidUsbDevice) -> AndroidUsbDeviceConnection,
    }
}

jni::bind_java_type! {
    JHashMap => "java.util.HashMap",
    is_instance_of = {
        JMap,
    }
}

jni::bind_java_type! {
    pub AndroidUsbDevice => "android.hardware.usb.UsbDevice",
    type_map = {
        AndroidUsbInterface => "android.hardware.usb.UsbInterface",
    },
    methods {
        fn get_device_id {
            name = "getDeviceId",
            sig = () -> jint,
        },
        fn get_vendor_id() -> jint,
        fn get_product_id() -> jint,
        fn get_device_class() -> jint,
        fn get_device_subclass() -> jint,
        fn get_device_protocol() -> jint,
        fn get_manufacturer_name() -> JString,
        fn get_product_name() -> JString,
        fn get_serial_number() -> JString,
        fn get_version() -> JString,
        fn get_interface_count() -> jint,
        fn get_interface(index: jint) -> AndroidUsbInterface,

    }
}

jni::bind_java_type! {
    AndroidUsbInterface => "android.hardware.usb.UsbInterface",
    methods {
        fn get_id() -> jint,
        fn get_interface_class() -> jint,
        fn get_interface_protocol() -> jint,
        fn get_interface_subclass() -> jint,
        fn get_name() -> JString,
    }
}

jni::bind_java_type! {
    AndroidUsbDeviceConnection => "android.hardware.usb.UsbDeviceConnection",
    methods {
        fn get_file_descriptor() -> jint,
    }
}

jni::bind_java_type! {
    PendingIntent => "android.app.PendingIntent",
    type_map = {
        AndroidContext => "android.content.Context",
        Intent => "android.content.Intent",
    },
    methods {
        static fn get_broadcast(ctx: AndroidContext, req_code: jint, intent: Intent, flags: jint) -> PendingIntent,
    }
}

// NOTE: this is a workaround for <github.com/jni-rs/jni-rs/issues/764>.
impl<'local> PendingIntent<'local> {
    const FLAG_MUTABLE: i32 = 0x02000000;
}

const ACTION_USB_PERMISSION: &str = "rust.android_usbser.USB_PERMISSION"; // custom

/// Maps *unexpected* JNI errors to `nusb::Error` of `ErrorKind::Other`.
/// Do not use this convenient conversion if error sorting is needed.
impl From<jni::errors::Error> for Error {
    fn from(err: jni::errors::Error) -> Self {
        use jni::errors::Error::*;
        match &err {
            CaughtJavaException { .. } => {
                error!("{err:#?}");
                Error::new(
                    ErrorKind::Other,
                    "unexpected Java error, please check logcat",
                )
            }
            JavaException => {
                if let Some(err) = jni_with_env(|env| env.exception_catch()).err() {
                    error!("{err:#?}");
                    Error::new(
                        ErrorKind::Other,
                        "unexpected pending Java error, please check logcat",
                    )
                } else {
                    Error::new(ErrorKind::Other, "unexpected pending Java error")
                }
            }
            _ => {
                error!("{err:#?}");
                Error::new(
                    ErrorKind::Other,
                    "unexpected JNI error, please check logcat",
                )
            }
        }
    }
}

/// Gets a global reference of `android.hardware.usb.UsbManager`.
fn usb_manager() -> Result<&'static AndroidUsbManager<'static>, Error> {
    static USB_MAN: OnceLock<Global<AndroidUsbManager<'static>>> = OnceLock::new();

    if android_api_level() < 23 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "nusb requires Android API level 23 (6.0) or newer versions",
        ));
    }

    if let Some(ref_man) = USB_MAN.get() {
        return Ok(ref_man.as_ref());
    }

    let usb_man = jni_with_env(|env| {
        let context = env.as_cast::<AndroidContext>(android_context())?;
        let usb_service_id = AndroidContext::USB_SERVICE(env)?;
        let usb_man = context.get_system_service(env, usb_service_id)?;
        let result = if !usb_man.is_null() {
            let usb_man = AndroidUsbManager::cast_local(env, usb_man)?;
            Ok(env.new_global_ref(&usb_man)?)
        } else {
            Err(Error::new(
                ErrorKind::Unsupported,
                "USB system service not found",
            ))
        };
        Ok(result)
    })??;

    let _ = USB_MAN.set(usb_man);
    Ok(USB_MAN.get().unwrap().as_ref())
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
        let activity = env.as_cast::<AndroidActivity>(android_context())?;
        let intent_startup = activity.get_intent(env)?;
        // checks if the action of current intent is ACTION_USB_DEVICE_ATTACHED
        let action_startup = intent_startup.get_action(env)?.to_string();
        if action_startup.trim()
            != AndroidUsbManager::ACTION_USB_DEVICE_ATTACHED(env)?
                .to_string()
                .trim()
        {
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
            let hashmap_dev_list = usb_man.get_device_list(env)?;
            let jmap_dev_list: &JMap<'_> = hashmap_dev_list.as_ref();
            let mut iter_dev = jmap_dev_list.iter(env)?;
            while let Some(dev_entry) = iter_dev.next(env)? {
                let val = dev_entry.value(env)?;
                let dev = AndroidUsbDevice::cast_local(env, val)?;
                devices.push(build_device_info(env, &dev)?);
            }
            Ok(())
        })?;
        Ok(devices.into_iter())
    })())
}

fn build_device_info(
    env: &mut Env,
    dev: &AndroidUsbDevice<'_>,
) -> Result<DeviceInfo, jni::errors::Error> {
    let version = dev.get_version(env)?.to_string();
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
        device_id: dev.get_device_id(env)?,
        jni_global_ref: Arc::new(env.new_global_ref(dev)?),
        vendor_id: dev.get_vendor_id(env)? as u16,
        product_id: dev.get_product_id(env)? as u16,
        usb_version: (ver_major << 8) | ver_minor,
        class: dev.get_device_class(env)? as u8,
        subclass: dev.get_device_subclass(env)? as u8,
        protocol: dev.get_device_protocol(env)? as u8,
        speed: None,
        manufacturer_string: non_null_string(dev.get_manufacturer_name(env)?),
        product_string: non_null_string(dev.get_product_name(env)?),
        serial_number: Arc::new(OnceLock::new()),
        interfaces: {
            let num_interfaces = dev.get_interface_count(env)? as u8;
            let mut interfaces = Vec::new();
            for i in 0..num_interfaces {
                let interface = dev.get_interface(env, i as i32)?;
                interfaces.push(InterfaceInfo {
                    interface_number: interface.get_id(env)? as u8,
                    class: interface.get_interface_class(env)? as u8,
                    subclass: interface.get_interface_subclass(env)? as u8,
                    protocol: interface.get_interface_protocol(env)? as u8,
                    interface_string: non_null_string(interface.get_name(env)?),
                });
            }
            interfaces.sort_unstable_by_key(|i| i.interface_number);
            interfaces
        },
    })
}

fn non_null_string(s: JString<'_>) -> Option<String> {
    if !s.is_null() {
        Some(s.to_string())
    } else {
        None
    }
}

impl DeviceInfo {
    /// Checks if the device is still in the list of connected devices.
    fn check_connection(&self) -> bool {
        let Ok(usb_man) = usb_manager() else {
            return false;
        };
        jni_with_env(|env| {
            let hashmap_dev_list = usb_man.get_device_list(env)?;
            let jmap_dev_list: &JMap<'_> = hashmap_dev_list.as_ref();
            let mut iter_dev = jmap_dev_list.iter(env)?;
            while let Some(dev_entry) = iter_dev.next(env)? {
                let val = dev_entry.value(env)?;
                let dev = AndroidUsbDevice::cast_local(env, val)?;
                if dev.get_device_id(env)? == self.device_id {
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
                if let Some(serial_no) =
                    non_null_string(self.jni_global_ref.get_serial_number(env)?)
                {
                    let _ = self.serial_number.set(serial_no);
                }
                Ok(())
            });
        };
        self.serial_number.get().map(|s| s.as_str())
    }
}

pub fn has_permission(dev_info: &DeviceInfo) -> Result<bool, Error> {
    let usb_man = usb_manager()?;
    Ok(jni_with_env(|env| {
        usb_man.has_permission(env, dev_info.jni_global_ref.as_ref())
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
        let context = env.as_cast::<AndroidContext>(android_context())?;

        let str_perm = JString::new(env, ACTION_USB_PERMISSION)?;
        let intent = Intent::new_with_action(env, str_perm)?;

        let flags = if android_api_level() < 31 {
            0
        } else {
            PendingIntent::FLAG_MUTABLE
        };
        let pending_intent = PendingIntent::get_broadcast(env, context, 0, intent, flags)?;
        usb_man.request_permission(env, dev_info.jni_global_ref.as_ref(), pending_intent)?;
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
        env: &mut Env<'a>,
        intent: Intent<'a>,
    ) -> Result<(), jni::errors::Error> {
        let dev = get_extra_device(env, &intent)?;
        if dev.id() == dev_expected.id() {
            let extra_name = AndroidUsbManager::EXTRA_PERMISSION_GRANTED(env)?;
            let granted = intent
                .get_boolean_extra(env, extra_name, false)
                .unwrap_or(false);
            let Some(inner) = inner_weak.upgrade() else {
                return Ok(());
            };
            inner.result.lock().unwrap().replace(granted);
            inner.notify.take_notify_state().notify();
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
            let conn = usb_man.open_device(env, d.jni_global_ref.as_ref())?;
            if conn.is_null() {
                return Ok(Err(Error::new(
                    ErrorKind::NotFound,
                    "`UsbManager.openDevice()` failed`",
                )));
            }
            let raw_fd = conn.get_file_descriptor(env)?;

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
        let receiver = jni_with_env(|env| {
            let receiver = BroadcastReceiver::build(move |env, _, intent| {
                Self::on_receive(&inner_weak, env, intent)
            })?;
            receiver.register_for_action(
                AndroidUsbManager::ACTION_USB_DEVICE_ATTACHED(env)?
                    .to_string()
                    .trim(),
            )?;
            receiver.register_for_action(
                AndroidUsbManager::ACTION_USB_DEVICE_DETACHED(env)?
                    .to_string()
                    .trim(),
            )?;
            Ok(receiver)
        })?;
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
        env: &mut Env<'a>,
        intent: Intent<'a>,
    ) -> Result<(), jni::errors::Error> {
        if intent.is_null() {
            return Ok(()); // almost impossible
        }
        let Some(inner) = inner_weak.upgrade() else {
            return Ok(());
        };
        let Ok(action) = intent.get_action(env).map(|act| act.to_string()) else {
            return Ok(()); // almost impossible
        };
        let action = action.trim();
        use HotplugEvent::*;
        let action_attached = AndroidUsbManager::ACTION_USB_DEVICE_ATTACHED(env)?.to_string();
        let action_detached = AndroidUsbManager::ACTION_USB_DEVICE_DETACHED(env)?.to_string();
        if action == action_attached.trim() {
            let dev = get_extra_device(env, &intent)?;
            inner.events.lock().unwrap().push_back(Connected(dev));
        } else if action == action_detached.trim() {
            let id = get_extra_device(env, &intent)?.id();
            inner.events.lock().unwrap().push_back(Disconnected(id));
        }
        if let Some(w) = inner.waker.lock().unwrap().take() {
            w.wake()
        }
        Ok(())
    }
}

fn get_extra_device(
    env: &mut Env<'_>,
    intent: &Intent<'_>,
) -> Result<DeviceInfo, jni::errors::Error> {
    let extra_device = AndroidUsbManager::EXTRA_DEVICE(env)?;
    let java_dev = env
        .call_method(
            intent,
            jni_str!("getParcelableExtra"),
            // XXX: this is deprecated in API 33 and above without the class parameter.
            jni_sig!((JString) -> android.os.Parcelable),
            &[(&extra_device).into()],
        )?
        .l()?;
    if !java_dev.is_null() {
        let java_dev = AndroidUsbDevice::cast_local(env, java_dev)?;
        build_device_info(env, &java_dev)
    } else {
        Err(jni::errors::Error::NullPtr(
            "Unexpected: the Intent has no EXTRA_DEVICE",
        ))
    }
}
