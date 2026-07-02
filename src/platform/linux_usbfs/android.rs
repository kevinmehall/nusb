use crate::{
    hotplug::HotplugEvent,
    maybe_future::{MaybeFuture, Ready},
    platform::linux_usbfs::device::LinuxDevice,
    transfer::internal::Notify,
    DeviceInfo, Error, ErrorKind, InterfaceInfo,
};

use std::{
    collections::VecDeque,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    task::{self, Poll, Waker},
};

use log::{debug, error, warn};

use jni::{
    objects::{Global, JMap, JString},
    refs::Reference,
    Env,
};
use jni_min_helper::{
    android_api_level, android_app_package_name, android_context, jni_with_env, BroadcastReceiver,
    Intent,
};

pub type DeviceId = i32;
pub type JniGlobal = Arc<Global<AndroidUsbDevice<'static>>>;

const ACTION_USB_PERMISSION: &str = "rust.nusb.USB_PERMISSION";

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
        fn close(),
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
    let device_version = if android_api_level() >= 28 {
        let version = dev.get_version(env)?.to_string();
        let ver_parser = |version: &str| -> Option<(u16, u16)> {
            let mut ver_iter = version.split('.').map(|v| v.trim().parse());
            Some((ver_iter.next()?.ok()?, ver_iter.next()?.ok()?))
        };
        ver_parser(&version)
            .map(|(major, minor)| {
                let (ver_tens, ver_ones) = (major / 10, major % 10);
                let (ver_tenths, ver_hundredths) = (minor / 10, minor % 10);
                (ver_tens << 12) | (ver_ones << 8) | (ver_tenths << 4) | ver_hundredths
            })
            .unwrap_or_else(|| {
                warn!("Unable to parse device version for DeviceInfo '{version}'");
                0xFFFF
            })
    } else {
        warn!("Unable to get device_version for DeviceInfo (Android API level < 28)");
        0xFFFF
    };

    Ok(DeviceInfo {
        device_id: dev.get_device_id(env)?,
        jni_global_ref: Arc::new(env.new_global_ref(dev)?),
        vendor_id: dev.get_vendor_id(env)? as u16,
        product_id: dev.get_product_id(env)? as u16,
        device_version,
        class: dev.get_device_class(env)? as u8,
        subclass: dev.get_device_subclass(env)? as u8,
        protocol: dev.get_device_protocol(env)? as u8,
        speed: None,
        manufacturer_string: non_null_string(dev.get_manufacturer_name(env)?),
        product_string: non_null_string(dev.get_product_name(env)?),
        serial_number: dev
            .get_serial_number(env)
            .inspect_err(|_| {
                // See <https://developer.android.com/about/versions/10/privacy/changes?hl=en#usb-serial>.
                // This is usually `java.lang.SecurityException: User has not given permission...`.
                let _ = env.exception_catch();
            })
            .ok()
            .and_then(non_null_string),
        interfaces: {
            let cnt_interfaces = dev.get_interface_count(env)? as u8;
            let mut interfaces: Vec<InterfaceInfo> = Vec::new();
            for i in 0..cnt_interfaces {
                let interface = dev.get_interface(env, i as i32)?;
                let interface_number = interface.get_id(env)? as u8;
                // Get information from the first alternative setting, ignore others.
                if interfaces
                    .iter()
                    .any(|intr| intr.interface_number == interface_number)
                {
                    continue;
                }
                interfaces.push(InterfaceInfo {
                    interface_number,
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

    fn has_permission(&self) -> Result<bool, Error> {
        has_permission(self)
    }
}

pub fn has_permission(dev_info: &DeviceInfo) -> Result<bool, Error> {
    let usb_man = usb_manager()?;
    Ok(jni_with_env(|env| {
        usb_man.has_permission(env, dev_info.jni_global_ref.as_ref())
    })?)
}

pub fn request_permission(dev_info: &DeviceInfo) -> impl MaybeFuture<Output = Result<bool, Error>> {
    match PermissionRequest::build(dev_info) {
        Ok(req) => req,
        Err(e) => PermissionRequest::build_dummy(dev_info, Err(e)),
    }
}

/// Android will only show one permission request dialog at a time; forced cancellation
/// of the previous request dialog is not possible. This receiver is used to wait for the
/// completion of the permission request, and if already set, we won't start a new one.
static PENDING_PERMISSION_RECEIVER: Mutex<Option<BroadcastReceiver>> = Mutex::new(None);

/// Represents an ongoing permission request.
struct PermissionRequest {
    dev_info: DeviceInfo,
    inner: Arc<PermissionRequestInner>,
}

struct PermissionRequestInner {
    notify: Notify,
    result: OnceLock<Result<bool, Error>>,
}

impl PermissionRequest {
    fn build(dev_info: &DeviceInfo) -> Result<Self, Error> {
        if !dev_info.check_connection() {
            return Ok(Self::build_dummy(
                dev_info,
                Err(Error::new(ErrorKind::Disconnected, "device disconnected")),
            ));
        }
        if dev_info.has_permission()? {
            return Ok(Self::build_dummy(dev_info, Ok(true)));
        }

        let mut receiver_lock = PENDING_PERMISSION_RECEIVER.lock().unwrap();

        if receiver_lock.is_some() {
            return Err(Error::new(
                ErrorKind::Other,
                "another permission request is already pending",
            )
            .log_debug());
        }

        let inner = Arc::new(PermissionRequestInner {
            notify: Notify::new(),
            result: OnceLock::new(),
        });

        let receiver = {
            let inner_weak = Arc::downgrade(&inner);
            let dev_info_2 = dev_info.clone();
            let receiver = BroadcastReceiver::build(move |env, _, intent| {
                Self::on_receive(&inner_weak, &dev_info_2, env, intent)
            })?;
            receiver.register_for_action(&Self::action_usb_permission())?;
            receiver.register_for_action(
                jni_with_env(|env| {
                    AndroidUsbManager::ACTION_USB_DEVICE_DETACHED(env).map(|s| s.to_string())
                })?
                .trim(),
            )?;
            receiver
        };

        let usb_man = usb_manager()?;

        jni_with_env(|env| {
            let context = env.as_cast::<AndroidContext>(android_context())?;
            let intent = {
                let str_perm = JString::new(env, Self::action_usb_permission())?;
                let package_name = JString::new(env, android_app_package_name())?;
                let intent = Intent::new_with_action(env, str_perm)?;
                intent.set_package(env, package_name)?;
                intent
            };
            let flags = if android_api_level() < 31 {
                0
            } else {
                PendingIntent::FLAG_MUTABLE
            };
            let pending_intent = PendingIntent::get_broadcast(env, context, 0, intent, flags)?;
            usb_man.request_permission(env, dev_info.jni_global_ref.as_ref(), pending_intent)
        })?;

        *receiver_lock = Some(receiver);
        log::debug!(
            "requested user permission for device {}",
            dev_info.device_id
        );

        Ok(Self {
            dev_info: dev_info.clone(),
            inner,
        })
    }

    /// Builds a finished permission request with known result.
    fn build_dummy(dev_info: &DeviceInfo, result: Result<bool, Error>) -> Self {
        Self {
            dev_info: dev_info.clone(),
            inner: Arc::new(PermissionRequestInner {
                notify: Notify::new(),
                result: OnceLock::from(result),
            }),
        }
    }

    /// Returns a reference of the associated `DeviceInfo` which can be cloned.
    fn device_info(&self) -> &DeviceInfo {
        &self.dev_info
    }

    /// Returns the result if it is received or otherwise determined.
    fn check_result(&self) -> Option<Result<bool, Error>> {
        if !self.device_info().check_connection() {
            let res = Err(Error::new(ErrorKind::Disconnected, "device disconnected"));
            self.inner.set_result(res.clone());
            return Some(res);
        }
        if let Ok(true) = self.device_info().has_permission() {
            self.inner.set_result(Ok(true));
            return Some(Ok(true));
        }
        self.inner.result.get().cloned()
    }

    /// Called when a permission request result is received.
    fn on_receive<'a>(
        inner_weak: &Weak<PermissionRequestInner>,
        dev_expected: &DeviceInfo,
        env: &mut Env<'a>,
        intent: Intent<'a>,
    ) -> Result<(), jni::errors::Error> {
        if intent.get_action(env)?.to_string().trim() == &Self::action_usb_permission() {
            let dev_id = get_extra_device_id(env, &intent)?;
            debug!("Received permission request result of device {dev_id:?}");
            if dev_id == dev_expected.id() {
                let Some(inner) = inner_weak.upgrade() else {
                    // The Rust-side request was aborted previously; still,
                    // the actual request needs to be treated as completed.
                    drop(PENDING_PERMISSION_RECEIVER.lock().unwrap().take());
                    return Ok(());
                };
                let extra_name = AndroidUsbManager::EXTRA_PERMISSION_GRANTED(env)?;
                let granted = intent
                    .get_boolean_extra(env, extra_name, false)
                    .map_err(|_| {
                        env.exception_clear();
                        Error::new(ErrorKind::Other, "failed to get EXTRA_PERMISSION_GRANTED")
                    });
                inner.set_result(granted);
            }
        } else if !dev_expected.check_connection() {
            debug!("Received disconnection event in `PermissionRequest::on_receive`");
            if let Some(inner) = inner_weak.upgrade() {
                inner.set_result(Err(Error::new(
                    ErrorKind::Disconnected,
                    "device disconnected",
                )));
            } else {
                drop(PENDING_PERMISSION_RECEIVER.lock().unwrap().take());
            };
        }
        Ok(())
    }

    fn action_usb_permission() -> String {
        format!("{}.{}", android_app_package_name(), ACTION_USB_PERMISSION)
    }
}

impl PermissionRequestInner {
    /// The result should be set for once when it is received or otherwise determined.
    /// * When it is set, the actual request is treated as completed, thus the
    ///   `PENDING_PERMISSION_RECEIVER` is cleared to make possible of a new request.
    ///   Then the outer `PermissionRequest` is notified.
    /// * If the result is already set, this should be a no-op.
    fn set_result(&self, res: Result<bool, Error>) {
        if self.result.set(res.clone()).is_ok() {
            debug!("notifying for `PermissionRequest` with value `{res:?}`");
            drop(PENDING_PERMISSION_RECEIVER.lock().unwrap().take());
            self.notify.take_notify_state().notify();
        }
    }
}

impl std::future::Future for PermissionRequest {
    type Output = Result<bool, Error>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Self::Output> {
        self.inner.notify.subscribe(cx);
        if let Some(result) = self.check_result() {
            task::Poll::Ready(result)
        } else {
            task::Poll::Pending
        }
    }
}

impl MaybeFuture for PermissionRequest {
    fn wait(self) -> Self::Output {
        self.inner.notify.wait(|| self.check_result())
    }
}

pub fn open_device(dev: &DeviceInfo) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
    let dev = dev.clone();
    request_permission(&dev).map(move |perm_result| {
        match perm_result {
            Ok(true) => (),
            Ok(false) => {
                return Err(Error::new(
                    ErrorKind::PermissionDenied,
                    "USB device permission request refused by the user",
                ))
            }
            Err(e) => return Err(e),
        }
        jni_with_env(|env| {
            let usb_man = match usb_manager() {
                Ok(man) => man,
                Err(e) => return Ok(Err(e)),
            };
            let conn = usb_man.open_device(env, dev.jni_global_ref.as_ref())?;
            if conn.is_null() {
                return Ok(Err(Error::new(
                    ErrorKind::NotFound,
                    "`UsbManager.openDevice()` failed",
                )));
            }
            let raw_fd = conn.get_file_descriptor(env)?;

            // Safety: `close()` is not called automatically when the JNI local reference of `conn`
            // and the corresponding Java object is destroyed. (check `UsbDeviceConnection` source)
            use std::os::fd::*;
            debug!("Wrapping fd {raw_fd} as usbfs device");
            let owned_fd = unsafe { OwnedFd::from_raw_fd(raw_fd as RawFd) };
            Ok(LinuxDevice::create_inner(owned_fd))
        })?
    })
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
            let java_dev = get_extra_device(env, &intent)?;
            let dev = build_device_info(env, &java_dev)?;
            inner.events.lock().unwrap().push_back(Connected(dev));
        } else if action == action_detached.trim() {
            let id = get_extra_device_id(env, &intent)?;
            inner.events.lock().unwrap().push_back(Disconnected(id));
        }
        let waker = inner.waker.lock().unwrap().take();
        if let Some(w) = waker {
            w.wake();
        }
        Ok(())
    }
}

fn get_extra_device<'local>(
    env: &mut Env<'local>,
    intent: &Intent<'_>,
) -> Result<AndroidUsbDevice<'local>, jni::errors::Error> {
    let extra_device = AndroidUsbManager::EXTRA_DEVICE(env)?;
    let cls_dev = AndroidUsbDevice::lookup_class(env, &jni::refs::LoaderContext::None)?;
    let java_dev = intent.get_parcelable_extra(env, &extra_device, cls_dev.deref())?;
    if !java_dev.is_null() {
        AndroidUsbDevice::cast_local(env, java_dev)
    } else {
        Err(jni::errors::Error::NullPtr(
            "Unexpected: the Intent has no EXTRA_DEVICE",
        ))
    }
}

fn get_extra_device_id(
    env: &mut Env<'_>,
    intent: &Intent<'_>,
) -> Result<crate::DeviceId, jni::errors::Error> {
    Ok(crate::DeviceId(
        get_extra_device(env, intent)?.get_device_id(env)?,
    ))
}
