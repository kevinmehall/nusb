use std::{
    collections::{btree_map::Entry, BTreeMap},
    ffi::c_void,
    io::{self, ErrorKind},
    mem::size_of_val,
    os::windows::{
        io::{AsRawHandle, RawHandle},
        prelude::OwnedHandle,
    },
    ptr::null_mut,
    sync::{Arc, Mutex},
    time::Duration,
};

use log::{debug, error, info, warn};
use windows_sys::Win32::{
    Devices::Usb::{
        WinUsb_ControlTransfer, WinUsb_Free, WinUsb_GetAssociatedInterface, WinUsb_Initialize,
        WinUsb_ResetPipe, WinUsb_SetCurrentAlternateSetting, WinUsb_SetPipePolicy,
        PIPE_TRANSFER_TIMEOUT, WINUSB_INTERFACE_HANDLE, WINUSB_SETUP_PACKET,
    },
    Foundation::{GetLastError, FALSE, TRUE},
};

use crate::{
    descriptors::{validate_config_descriptor, DESCRIPTOR_TYPE_CONFIGURATION},
    transfer::{Control, Direction, EndpointType, Recipient, TransferError, TransferHandle},
    DeviceInfo, Error,
};

use super::{
    enumeration::{
        find_usbccgp_child, get_driver_name, get_usbccgp_winusb_device_path, get_winusb_device_path,
    },
    hub::HubPort,
    util::{create_file, raw_handle, WCStr},
    DevInst,
};

pub(crate) struct WindowsDevice {
    config_descriptors: Vec<Vec<u8>>,
    active_config: u8,
    devinst: DevInst,
    handles: Mutex<BTreeMap<u8, WinusbFileHandle>>,
}

impl WindowsDevice {
    pub(crate) async fn from_device_info(d: &DeviceInfo) -> Result<Arc<WindowsDevice>, Error> {
        debug!("Creating device for {:?}", d.instance_id);

        // Look up the device again in case the DeviceInfo is stale. In
        // particular, don't trust its `port_number` because another device
        // might now be connected to that port, and we'd get its descriptors
        // instead.
        let hub_port = HubPort::by_child_devinst(d.devinst)?;
        let connection_info = hub_port.get_info()?;
        let num_configurations = connection_info.device_desc.bNumConfigurations;

        let config_descriptors = (0..num_configurations)
            .flat_map(|i| {
                let res = hub_port.get_descriptor(DESCRIPTOR_TYPE_CONFIGURATION, i, 0);
                match res {
                    Ok(v) => validate_config_descriptor(&v[..]).map(|_| v),
                    Err(e) => {
                        error!("Failed to read config descriptor {}: {}", i, e);
                        None
                    }
                }
            })
            .collect();

        Ok(Arc::new(WindowsDevice {
            config_descriptors,
            active_config: connection_info.active_config,
            devinst: d.devinst,
            handles: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.active_config
    }

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
        self.config_descriptors.iter().map(|d| &d[..])
    }

    pub(crate) async fn set_configuration(&self, _configuration: u8) -> Result<(), Error> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "set_configuration not supported by WinUSB",
        ))
    }

    pub(crate) fn get_descriptor(
        &self,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
    ) -> Result<Vec<u8>, Error> {
        HubPort::by_child_devinst(self.devinst)?.get_descriptor(desc_type, desc_index, language_id)
    }

    pub(crate) async fn reset(&self) -> Result<(), Error> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "reset not supported by WinUSB",
        ))
    }

    pub(crate) async fn claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        let driver = get_driver_name(self.devinst);

        let mut handles = self.handles.lock().unwrap();

        if driver.eq_ignore_ascii_case("winusb") {
            match handles.entry(0) {
                Entry::Occupied(mut e) => e.get_mut().claim_interface(self, interface_number).await,
                Entry::Vacant(e) => {
                    let path = get_winusb_device_path(self.devinst)?;
                    let mut handle = WinusbFileHandle::new(&path, 0)?;
                    let intf = handle.claim_interface(self, interface_number).await?;
                    e.insert(handle);
                    Ok(intf)
                }
            }
        } else if driver.eq_ignore_ascii_case("usbccgp") {
            let (first_interface, child_dev) =
                find_usbccgp_child(self.devinst, interface_number)
                    .ok_or_else(|| Error::new(ErrorKind::NotFound, "Interface not found"))?;

            if first_interface != interface_number {
                debug!("Guessing that interface {interface_number} is an associated interface of {first_interface}");
            }

            match handles.entry(first_interface) {
                Entry::Occupied(mut e) => e.get_mut().claim_interface(self, interface_number).await,
                Entry::Vacant(e) => {
                    let path = get_usbccgp_winusb_device_path(child_dev)?;
                    let mut handle = WinusbFileHandle::new(&path, first_interface)?;
                    let intf = handle.claim_interface(self, interface_number).await?;
                    e.insert(handle);
                    Ok(intf)
                }
            }
        } else {
            Err(Error::new(
                ErrorKind::Unsupported,
                format!("Device driver is {driver:?}, not WinUSB or USBCCGP"),
            ))
        }
    }

    pub(crate) async fn detach_and_claim_interface(
        self: &Arc<Self>,
        interface: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        self.claim_interface(interface).await
    }
}

struct BitSet256([u64; 4]);

impl BitSet256 {
    fn new() -> Self {
        Self([0; 4])
    }

    fn idx(bit: u8) -> usize {
        (bit / 64) as usize
    }

    fn mask(bit: u8) -> u64 {
        1u64 << (bit % 64)
    }

    fn is_set(&mut self, bit: u8) -> bool {
        self.0[Self::idx(bit)] & Self::mask(bit) != 0
    }

    fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }

    fn set(&mut self, bit: u8) {
        self.0[Self::idx(bit)] |= Self::mask(bit)
    }

    fn clear(&mut self, bit: u8) {
        self.0[Self::idx(bit)] &= !Self::mask(bit)
    }
}

/// A file handle and the WinUSB handle for the first interface.
pub(crate) struct WinusbFileHandle {
    first_interface: u8,
    handle: OwnedHandle,
    winusb_handle: WINUSB_INTERFACE_HANDLE,
    claimed_interfaces: BitSet256,
}

impl WinusbFileHandle {
    fn new(path: &WCStr, first_interface: u8) -> Result<Self, Error> {
        let handle = create_file(&path)?;
        super::events::register(&handle)?;

        let winusb_handle = unsafe {
            let mut h = 0;
            if WinUsb_Initialize(raw_handle(&handle), &mut h) == FALSE {
                error!("WinUsb_Initialize failed: {:?}", io::Error::last_os_error());
                return Err(io::Error::last_os_error());
            }
            h
        };

        debug!("Opened WinUSB handle for {path} (interface {first_interface})");

        Ok(WinusbFileHandle {
            first_interface,
            handle,
            winusb_handle,
            claimed_interfaces: BitSet256::new(),
        })
    }

    async fn claim_interface(
        &mut self,
        device: &Arc<WindowsDevice>,
        interface_number: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        assert!(interface_number >= self.first_interface);

        if self.claimed_interfaces.is_set(interface_number) {
            return Err(Error::new(
                ErrorKind::AddrInUse,
                "Interface is already claimed",
            ));
        }

        let winusb_handle = if self.first_interface == interface_number {
            self.winusb_handle
        } else {
            unsafe {
                let mut out_handle = 0;
                let idx = interface_number - self.first_interface - 1;
                if WinUsb_GetAssociatedInterface(self.winusb_handle, idx, &mut out_handle) == FALSE
                {
                    error!(
                        "WinUsb_GetAssociatedInterface for {} on {} failed: {:?}",
                        interface_number,
                        self.first_interface,
                        io::Error::last_os_error()
                    );
                    return Err(io::Error::last_os_error());
                }
                out_handle
            }
        };

        log::debug!(
            "Claiming interface {interface_number} using handle for {}",
            self.first_interface
        );

        self.claimed_interfaces.set(interface_number);

        Ok(Arc::new(WindowsInterface {
            handle: self.handle.as_raw_handle(),
            device: device.clone(),
            interface_number,
            first_interface_number: self.first_interface,
            winusb_handle,
        }))
    }
}

impl Drop for WinusbFileHandle {
    fn drop(&mut self) {
        log::debug!(
            "Closing WinUSB handle for interface {}",
            self.first_interface
        );
        unsafe {
            WinUsb_Free(self.winusb_handle);
        }
    }
}

pub(crate) struct WindowsInterface {
    pub(crate) handle: RawHandle,
    pub(crate) device: Arc<WindowsDevice>,
    pub(crate) first_interface_number: u8,
    pub(crate) interface_number: u8,
    pub(crate) winusb_handle: WINUSB_INTERFACE_HANDLE,
}

unsafe impl Send for WindowsInterface {}
unsafe impl Sync for WindowsInterface {}

impl Drop for WindowsInterface {
    fn drop(&mut self) {
        // The WinUSB handle for the first interface is owned by WinusbFileHandle
        // because it is used to open subsequent interfaces.
        let is_first_interface = self.interface_number == self.first_interface_number;
        if !is_first_interface {
            log::debug!(
                "Closing WinUSB handle for associated interface {}",
                self.interface_number
            );
            unsafe {
                WinUsb_Free(self.winusb_handle);
            }
        }

        let mut handles = self.device.handles.lock().unwrap();
        let Entry::Occupied(mut entry) = handles.entry(self.first_interface_number) else {
            panic!("missing handle that should be open")
        };

        entry
            .get_mut()
            .claimed_interfaces
            .clear(self.interface_number);

        if entry.get().claimed_interfaces.is_empty() {
            entry.remove();
        } else if is_first_interface {
            log::debug!(
                "Released interface {}, but retaining handle for shared use",
                self.interface_number
            );
        }
    }
}

impl WindowsInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(self.clone(), endpoint, ep_type))
    }

    /// SAFETY: `data` must be valid for `len` bytes to read or write, depending on `Direction`
    unsafe fn control_blocking(
        &self,
        direction: Direction,
        control: Control,
        data: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        info!("Blocking control {direction:?}, {len} bytes");

        if control.recipient == Recipient::Interface && control.index as u8 != self.interface_number
        {
            warn!("WinUSB sends interface number instead of passed `index` when performing a control transfer with `Recipient::Interface`");
        }

        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let r = WinUsb_SetPipePolicy(
            self.winusb_handle,
            0,
            PIPE_TRANSFER_TIMEOUT,
            size_of_val(&timeout_ms) as u32,
            &timeout_ms as *const u32 as *const c_void,
        );

        if r != TRUE {
            error!(
                "WinUsb_SetPipePolicy PIPE_TRANSFER_TIMEOUT failed: {}",
                io::Error::last_os_error()
            );
        }

        let pkt = WINUSB_SETUP_PACKET {
            RequestType: control.request_type(direction),
            Request: control.request,
            Value: control.value,
            Index: control.index,
            Length: len.try_into().expect("request size too large"),
        };

        let mut actual_len = 0;

        let r = WinUsb_ControlTransfer(
            self.winusb_handle,
            pkt,
            data,
            len.try_into().expect("request size too large"),
            &mut actual_len,
            null_mut(),
        );

        if r == TRUE {
            Ok(actual_len as usize)
        } else {
            error!(
                "WinUsb_ControlTransfer failed: {}",
                io::Error::last_os_error()
            );
            Err(super::transfer::map_error(GetLastError()))
        }
    }

    pub fn control_in_blocking(
        &self,
        control: Control,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        unsafe {
            self.control_blocking(
                Direction::In,
                control,
                data.as_mut_ptr(),
                data.len(),
                timeout,
            )
        }
    }

    pub fn control_out_blocking(
        &self,
        control: Control,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        // When passed a pointer to read-only memory (e.g. a constant slice),
        // WinUSB fails with "Invalid access to memory location. (os error 998)".
        // I assume the kernel is checking the pointer for write access
        // regardless of the transfer direction. Copy the data to the stack to ensure
        // we give it a pointer to writable memory.
        let mut buf = [0; 4096];
        let Some(buf) = buf.get_mut(..data.len()) else {
            error!(
                "Control transfer length {} exceeds limit of 4096",
                data.len()
            );
            return Err(TransferError::Unknown);
        };
        buf.copy_from_slice(data);

        unsafe {
            self.control_blocking(
                Direction::Out,
                control,
                buf.as_mut_ptr(),
                buf.len(),
                timeout,
            )
        }
    }

    pub async fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        unsafe {
            let r = WinUsb_SetCurrentAlternateSetting(self.winusb_handle, alt_setting.into());
            if r == TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }

    pub async fn clear_halt(&self, endpoint: u8) -> Result<(), Error> {
        debug!("Clear halt, endpoint {endpoint:02x}");
        unsafe {
            let r = WinUsb_ResetPipe(self.winusb_handle, endpoint);
            if r == TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}
