use std::{
    collections::VecDeque,
    ffi::c_void,
    io::ErrorKind,
    mem::size_of,
    ptr::addr_of,
    sync::Mutex,
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;
use log::{debug, error};
use windows_sys::Win32::{
    Devices::{
        DeviceAndDriverInstallation::{
            CM_Register_Notification, CM_Unregister_Notification, CM_NOTIFY_ACTION,
            CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL, CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL,
            CM_NOTIFY_EVENT_DATA, CM_NOTIFY_FILTER, CM_NOTIFY_FILTER_0, CM_NOTIFY_FILTER_0_2,
            CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE, CR_SUCCESS, HCMNOTIFICATION,
        },
        Properties::DEVPKEY_Device_InstanceId,
        Usb::GUID_DEVINTERFACE_USB_DEVICE,
    },
    Foundation::ERROR_SUCCESS,
};

use crate::{
    hotplug::HotplugEvent,
    platform::windows_winusb::{cfgmgr32::get_device_interface_property, util::WCString},
    DeviceId, Error,
};

use super::{enumeration::probe_device, util::WCStr};

use super::DevInst;

pub(crate) struct WindowsHotplugWatch {
    inner: *mut HotplugInner,
    registration: HCMNOTIFICATION,
}

struct HotplugInner {
    waker: AtomicWaker,
    events: Mutex<VecDeque<(Action, DevInst)>>,
}

#[derive(Debug)]
enum Action {
    Connect,
    Disconnect,
}

impl WindowsHotplugWatch {
    pub fn new() -> Result<WindowsHotplugWatch, Error> {
        let inner = Box::into_raw(Box::new(HotplugInner {
            events: Mutex::new(VecDeque::new()),
            waker: AtomicWaker::new(),
        }));

        let mut registration = 0;
        let filter = CM_NOTIFY_FILTER {
            cbSize: size_of::<CM_NOTIFY_FILTER>() as u32,
            Flags: 0,
            FilterType: CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE,
            Reserved: 0,
            u: CM_NOTIFY_FILTER_0 {
                DeviceInterface: CM_NOTIFY_FILTER_0_2 {
                    ClassGuid: GUID_DEVINTERFACE_USB_DEVICE,
                },
            },
        };

        let cr = unsafe {
            CM_Register_Notification(
                &filter,
                inner as *mut c_void,
                Some(hotplug_callback),
                &mut registration,
            )
        };

        if cr != CR_SUCCESS {
            error!("CM_Register_Notification failed: {cr}");
            return Err(Error::new(
                ErrorKind::Other,
                "Failed to initialize hotplug notifications",
            ));
        }

        Ok(WindowsHotplugWatch {
            inner,
            registration,
        })
    }

    fn inner(&self) -> &HotplugInner {
        unsafe { &*self.inner }
    }

    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<HotplugEvent> {
        self.inner().waker.register(cx.waker());
        let event = self.inner().events.lock().unwrap().pop_front();
        match event {
            Some((Action::Connect, devinst)) => {
                if let Some(dev) = probe_device(devinst) {
                    return Poll::Ready(HotplugEvent::Connected(dev));
                };
            }
            Some((Action::Disconnect, devinst)) => {
                return Poll::Ready(HotplugEvent::Disconnected(DeviceId(devinst)));
            }
            None => {}
        }
        Poll::Pending
    }
}

impl Drop for WindowsHotplugWatch {
    fn drop(&mut self) {
        unsafe {
            // According to [1], `CM_Unregister_Notification` waits for
            // callbacks to finish, so it should be safe to drop `inner`
            // immediately afterward without races.
            // [1]: https://learn.microsoft.com/en-us/windows/win32/api/cfgmgr32/nf-cfgmgr32-cm_unregister_notification
            CM_Unregister_Notification(self.registration);
            drop(Box::from_raw(self.inner));
        }
    }
}

unsafe extern "system" fn hotplug_callback(
    _hnotify: HCMNOTIFICATION,
    context: *const ::core::ffi::c_void,
    action: CM_NOTIFY_ACTION,
    eventdata: *const CM_NOTIFY_EVENT_DATA,
    _eventdatasize: u32,
) -> u32 {
    let inner = unsafe { &*(context as *const HotplugInner) };

    let action = match action {
        CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL => Action::Connect,
        CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL => Action::Disconnect,
        _ => {
            debug!("Hotplug callback: unknown action {action}");
            return ERROR_SUCCESS;
        }
    };

    let device_interface =
        unsafe { WCStr::from_ptr(addr_of!((*eventdata).u.DeviceInterface.SymbolicLink[0])) };

    let device_instance =
        get_device_interface_property::<WCString>(device_interface, DEVPKEY_Device_InstanceId)
            .unwrap();
    let devinst = DevInst::from_instance_id(&device_instance).unwrap();

    debug!("Hotplug callback: action={action:?}, instance={device_instance}");
    inner.events.lock().unwrap().push_back((action, devinst));
    inner.waker.wake();
    return ERROR_SUCCESS;
}
