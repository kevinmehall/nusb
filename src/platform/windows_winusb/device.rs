use std::{
    collections::{btree_map::Entry, BTreeMap, VecDeque},
    ffi::c_void,
    io,
    mem::{size_of_val, transmute},
    os::windows::{
        io::{AsRawHandle, RawHandle},
        prelude::OwnedHandle,
    },
    ptr::{self, null_mut},
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use log::{debug, error, warn};
use windows_sys::Win32::{
    Devices::Usb::{
        self, WinUsb_ControlTransfer, WinUsb_Free, WinUsb_GetAssociatedInterface,
        WinUsb_Initialize, WinUsb_ReadPipe, WinUsb_ResetPipe, WinUsb_SetCurrentAlternateSetting,
        WinUsb_SetPipePolicy, WinUsb_WritePipe, USB_DEVICE_DESCRIPTOR, WINUSB_INTERFACE_HANDLE,
        WINUSB_SETUP_PACKET,
    },
    Foundation::{
        GetLastError, ERROR_BAD_COMMAND, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND,
        ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_NO_SUCH_DEVICE, FALSE,
        HANDLE, TRUE,
    },
    System::IO::{CancelIoEx, OVERLAPPED},
};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{
        ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor, DESCRIPTOR_LEN_DEVICE,
        DESCRIPTOR_TYPE_CONFIGURATION,
    },
    maybe_future::{blocking::Blocking, Ready},
    platform::windows_winusb::util::DEFAULT_TRANSFER_TIMEOUT,
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        Buffer, Completion, ControlIn, ControlOut, Direction, Recipient, TransferError,
    },
    DeviceInfo, Error, ErrorKind, MaybeFuture, Speed,
};

use super::{
    enumeration::{
        find_usbccgp_child, get_driver_name, get_usbccgp_winusb_device_path, get_winusb_device_path,
    },
    hub::HubPort,
    transfer::TransferData,
    util::{create_file, raw_handle, WCStr},
    DevInst,
};

pub(crate) struct WindowsDevice {
    device_descriptor: DeviceDescriptor,
    config_descriptors: Vec<Vec<u8>>,
    active_config: u8,
    speed: Option<Speed>,
    devinst: DevInst,
    handles: Mutex<BTreeMap<u8, WinusbFileHandle>>,
}

impl WindowsDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<WindowsDevice>, Error>> {
        let instance_id = d.instance_id.clone();
        let devinst = d.devinst;
        Blocking::new(move || {
            debug!("Creating device for {:?}", instance_id);

            // Look up the device again in case the DeviceInfo is stale. In
            // particular, don't trust its `port_number` because another device
            // might now be connected to that port, and we'd get its descriptors
            // instead.
            let hub_port = HubPort::by_child_devinst(devinst)?;
            let connection_info = hub_port.get_info()?;

            // Safety: Windows API struct is repr(C), packed, and we're assuming Windows is little-endian
            let device_descriptor = unsafe {
                &transmute::<USB_DEVICE_DESCRIPTOR, [u8; DESCRIPTOR_LEN_DEVICE as usize]>(
                    connection_info.device_desc,
                )
            };
            let device_descriptor = DeviceDescriptor::new(device_descriptor)
                .ok_or_else(|| Error::new(ErrorKind::Other, "invalid device descriptor"))?;

            let num_configurations = connection_info.device_desc.bNumConfigurations;
            let config_descriptors = (0..num_configurations)
                .flat_map(|i| {
                    let d = hub_port
                        .get_descriptor(
                            DESCRIPTOR_TYPE_CONFIGURATION,
                            i,
                            0,
                            DEFAULT_TRANSFER_TIMEOUT,
                        )
                        .inspect_err(|e| error!("Failed to read config descriptor {}: {}", i, e))
                        .ok()?;

                    ConfigurationDescriptor::new(&d).is_some().then_some(d)
                })
                .collect();

            Ok(Arc::new(WindowsDevice {
                device_descriptor,
                config_descriptors,
                speed: connection_info.speed,
                active_config: connection_info.active_config,
                devinst,
                handles: Mutex::new(BTreeMap::new()),
            }))
        })
    }

    pub(crate) fn device_descriptor(&self) -> DeviceDescriptor {
        self.device_descriptor.clone()
    }

    pub(crate) fn speed(&self) -> Option<Speed> {
        self.speed
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.active_config
    }

    pub(crate) fn configuration_descriptors(
        &self,
    ) -> impl Iterator<Item = ConfigurationDescriptor> {
        self.config_descriptors
            .iter()
            .map(|d| ConfigurationDescriptor::new_unchecked(&d[..]))
    }

    pub(crate) fn set_configuration(
        &self,
        _configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Ready(Err(Error::new(
            ErrorKind::Unsupported,
            "set_configuration not supported by WinUSB",
        )))
    }

    pub(crate) fn get_descriptor(
        self: Arc<Self>,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
        timeout: std::time::Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        Blocking::new(move || {
            fn to_transfer_error(e: Error) -> TransferError {
                match e.kind() {
                    ErrorKind::Disconnected => TransferError::Disconnected,
                    _ => TransferError::Unknown(e.os_error().unwrap_or(0)),
                }
            }

            HubPort::by_child_devinst(self.devinst)
                .map_err(to_transfer_error)?
                .get_descriptor(desc_type, desc_index, language_id, timeout)
                .map_err(to_transfer_error)
        })
    }

    pub(crate) fn reset(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        Ready(Err(Error::new(
            ErrorKind::Unsupported,
            "reset not supported by WinUSB",
        )))
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<WindowsInterface>, Error>> {
        Blocking::new(move || {
            let driver = get_driver_name(self.devinst);

            let mut handles = self.handles.lock().unwrap();

            if driver.eq_ignore_ascii_case("winusb") {
                match handles.entry(0) {
                    Entry::Occupied(mut e) => e.get_mut().claim_interface(&self, interface_number),
                    Entry::Vacant(e) => {
                        let path = get_winusb_device_path(self.devinst)?;
                        let mut handle = WinusbFileHandle::new(&path, 0)?;
                        let intf = handle.claim_interface(&self, interface_number)?;
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
                    Entry::Occupied(mut e) => e.get_mut().claim_interface(&self, interface_number),
                    Entry::Vacant(e) => {
                        let path = get_usbccgp_winusb_device_path(child_dev)?;
                        let mut handle = WinusbFileHandle::new(&path, first_interface)?;
                        let intf = handle.claim_interface(&self, interface_number)?;
                        e.insert(handle);
                        Ok(intf)
                    }
                }
            } else {
                debug!("Device driver is {driver:?}, not WinUSB or USBCCGP");
                Err(Error::new(
                    ErrorKind::Unsupported,
                    "incompatible driver is installed for this device",
                ))
            }
        })
    }

    pub(crate) fn detach_and_claim_interface(
        self: Arc<Self>,
        interface: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<WindowsInterface>, Error>> {
        self.claim_interface(interface)
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

    fn is_set(&self, bit: u8) -> bool {
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

// SAFETY: WinUSB methods on the interface handle are thread-safe
unsafe impl Send for WinusbFileHandle {}
unsafe impl Sync for WinusbFileHandle {}

impl WinusbFileHandle {
    fn new(path: &WCStr, first_interface: u8) -> Result<Self, Error> {
        let handle = create_file(path)
            .map_err(|e| Error::new_os(ErrorKind::Other, "failed to open device", e).log_debug())?;
        super::events::register(&handle)?;

        let winusb_handle = unsafe {
            let mut h = ptr::null_mut();
            if WinUsb_Initialize(raw_handle(&handle), &mut h) == FALSE {
                return Err(Error::new_os(
                    ErrorKind::Other,
                    "failed to initialize WinUSB",
                    GetLastError(),
                )
                .log_debug());
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

    fn claim_interface(
        &mut self,
        device: &Arc<WindowsDevice>,
        interface_number: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        assert!(interface_number >= self.first_interface);

        if self.claimed_interfaces.is_set(interface_number) {
            return Err(Error::new(ErrorKind::Busy, "interface is already claimed"));
        }

        let winusb_handle = if self.first_interface == interface_number {
            self.winusb_handle
        } else {
            unsafe {
                let mut out_handle = ptr::null_mut();
                let idx = interface_number - self.first_interface - 1;
                if WinUsb_GetAssociatedInterface(self.winusb_handle, idx, &mut out_handle) == FALSE
                {
                    let err = GetLastError();
                    debug!(
                        "WinUsb_GetAssociatedInterface for interface {} using handle for {} failed: {:?}",
                        interface_number, self.first_interface, err
                    );

                    return Err(match err {
                        ERROR_NO_MORE_ITEMS => {
                            Error::new_os(ErrorKind::NotFound, "interface not found", err)
                        }
                        _ => Error::new_os(
                            ErrorKind::Other,
                            "failed to initialize WinUSB for associated interface",
                            err,
                        ),
                    });
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
            state: Mutex::new(InterfaceState::default()),
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
    state: Mutex<InterfaceState>,
}

#[derive(Default)]
struct InterfaceState {
    alt_setting: u8,
    endpoints: EndpointBitSet,
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
    pub fn control_in(
        self: &Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let mut t = TransferData::new(0x80);
        t.set_buffer(Buffer::new(data.length as usize));

        let pkt = WINUSB_SETUP_PACKET {
            RequestType: data.request_type(),
            Request: data.request,
            Value: data.value,
            Index: data.index,
            Length: data.length,
        };

        let intf = self.clone();

        TransferFuture::new(t, |t| self.submit_control(t, pkt)).map(move |mut t| {
            let c = t.take_completion(&intf);
            c.status?;
            Ok(c.buffer.into_vec())
        })
    }

    pub fn control_out(
        self: &Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let mut t = TransferData::new(0x00);
        t.set_buffer(Buffer::from(data.data.to_vec()));

        let pkt = WINUSB_SETUP_PACKET {
            RequestType: data.request_type(),
            Request: data.request,
            Value: data.value,
            Index: data.index,
            Length: data.data.len().try_into().expect("transfer too large"),
        };

        let intf = self.clone();

        TransferFuture::new(t, |t| self.submit_control(t, pkt)).map(move |mut t| {
            let c = t.take_completion(&intf);
            c.status
        })
    }

    pub fn set_alt_setting(
        self: Arc<Self>,
        alt_setting: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || unsafe {
            let mut state = self.state.lock().unwrap();
            if !state.endpoints.is_empty() {
                return Err(Error::new(
                    ErrorKind::Busy,
                    "can't change alternate setting while endpoints are in use",
                ));
            }
            let r = WinUsb_SetCurrentAlternateSetting(self.winusb_handle, alt_setting);
            if r == TRUE {
                debug!(
                    "Set interface {} alt setting to {alt_setting}",
                    self.interface_number
                );
                state.alt_setting = alt_setting;
                Ok(())
            } else {
                Err(match GetLastError() {
                    e @ ERROR_NOT_FOUND => {
                        Error::new_os(ErrorKind::NotFound, "alternate setting not found", e)
                    }
                    e @ ERROR_BAD_COMMAND => {
                        Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                    }
                    e => Error::new_os(ErrorKind::Other, "failed to set alternate setting", e),
                })
            }
        })
    }

    pub fn get_alt_setting(&self) -> u8 {
        self.state.lock().unwrap().alt_setting
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<WindowsEndpoint, Error> {
        let address = descriptor.address();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        if state.endpoints.is_set(address) {
            return Err(Error::new(ErrorKind::Busy, "endpoint already in use"));
        }
        state.endpoints.set(address);

        if Direction::from_address(address) == Direction::In {
            unsafe {
                let enable: u8 = 1;
                let r = WinUsb_SetPipePolicy(
                    self.winusb_handle,
                    address,
                    Usb::RAW_IO,
                    size_of_val(&enable) as u32,
                    &enable as *const _ as *const c_void,
                );
                if r != TRUE {
                    let err = GetLastError();
                    warn!("Failed to enable RAW_IO on endpoint {address:02X}: error {err:x}",);
                }
            }
        }

        Ok(WindowsEndpoint {
            inner: Arc::new(EndpointInner {
                address,
                interface: self.clone(),
                notify: Notify::new(),
            }),
            max_packet_size,
            pending: VecDeque::new(),
            idle_transfer: None,
        })
    }

    fn submit(&self, mut t: Idle<TransferData>) -> Pending<TransferData> {
        let endpoint = t.endpoint;
        let dir = Direction::from_address(endpoint);
        let len = t.request_len;
        let buf = t.buf;
        t.overlapped.InternalHigh = 0;
        t.error_from_submit = Ok(());

        let t = t.pre_submit();
        let ptr = t.as_ptr();

        debug!("Submit transfer {ptr:?} on endpoint {endpoint:02X} for {len} bytes {dir:?}");

        let r = unsafe {
            match dir {
                Direction::Out => WinUsb_WritePipe(
                    self.winusb_handle,
                    endpoint,
                    buf,
                    len,
                    null_mut(),
                    ptr as *mut OVERLAPPED,
                ),
                Direction::In => WinUsb_ReadPipe(
                    self.winusb_handle,
                    endpoint,
                    buf,
                    len,
                    null_mut(),
                    ptr as *mut OVERLAPPED,
                ),
            }
        };

        self.post_submit(r, t)
    }

    fn submit_control(
        &self,
        mut t: Idle<TransferData>,
        pkt: WINUSB_SETUP_PACKET,
    ) -> Pending<TransferData> {
        let endpoint = t.endpoint;
        let dir = Direction::from_address(endpoint);
        let len = t.request_len;
        let buf = t.buf;
        t.overlapped.InternalHigh = 0;
        t.error_from_submit = Ok(());

        if pkt.RequestType & 0x1f == Recipient::Interface as u8
            && pkt.Index as u8 != self.interface_number
        {
            warn!("WinUSB requires control transfer with `Recipient::Interface` to pass the interface number in `index`");
            t.error_from_submit = Err(TransferError::InvalidArgument);
            return t.simulate_complete();
        }

        let t = t.pre_submit();
        let ptr = t.as_ptr();

        debug!("Submit control {dir:?} transfer {ptr:?} for {len} bytes");

        let r = unsafe {
            WinUsb_ControlTransfer(
                self.winusb_handle,
                pkt,
                buf,
                len,
                null_mut(),
                ptr as *mut OVERLAPPED,
            )
        };

        self.post_submit(r, t)
    }

    fn post_submit(&self, r: i32, t: Pending<TransferData>) -> Pending<TransferData> {
        if r == TRUE {
            error!("Transfer submit completed synchronously")
        }

        let err = unsafe { GetLastError() };

        if err != ERROR_IO_PENDING {
            error!("submit failed: {}", io::Error::from_raw_os_error(err as _));

            // Safety: Transfer was not submitted, so we still own it
            // and must complete it in place of the event thread.
            unsafe {
                (*t.as_ptr()).error_from_submit = match err {
                    ERROR_BAD_COMMAND
                    | ERROR_FILE_NOT_FOUND
                    | ERROR_DEVICE_NOT_CONNECTED
                    | ERROR_NO_SUCH_DEVICE => Err(TransferError::Disconnected),
                    other => Err(TransferError::Unknown(other)),
                };
                notify_completion::<TransferData>(t.as_ptr());
            }
        }

        t
    }

    fn cancel(&self, t: &mut Pending<TransferData>) {
        debug!("Cancelling transfer {:?}", t.as_ptr());
        unsafe {
            let r = CancelIoEx(self.handle as HANDLE, t.as_ptr() as *mut OVERLAPPED);
            if r == 0 {
                let err = GetLastError();
                if err != ERROR_NOT_FOUND {
                    error!(
                        "CancelIoEx failed: {}",
                        io::Error::from_raw_os_error(err as i32)
                    );
                }
            }
        }
    }
}

pub(crate) struct WindowsEndpoint {
    inner: Arc<EndpointInner>,

    pub(crate) max_packet_size: usize,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<Pending<TransferData>>,

    idle_transfer: Option<Idle<TransferData>>,
}

struct EndpointInner {
    interface: Arc<WindowsInterface>,
    address: u8,
    notify: Notify,
}

impl WindowsEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        for transfer in self.pending.iter_mut().rev() {
            self.inner.interface.cancel(transfer);
        }
    }

    fn make_transfer(&mut self, buffer: Buffer) -> Idle<TransferData> {
        let mut t = self.idle_transfer.take().unwrap_or_else(|| {
            Idle::new(self.inner.clone(), TransferData::new(self.inner.address))
        });
        t.set_buffer(buffer);
        t
    }

    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let t = self.make_transfer(buffer);
        let t = self.inner.interface.submit(t);
        self.pending.push_back(t);
    }

    pub(crate) fn submit_err(&mut self, buffer: Buffer, err: TransferError) {
        let mut t = self.make_transfer(buffer);
        t.error_from_submit = Err(err);
        self.pending.push_back(t.simulate_complete());
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let completion = transfer.take_completion(&self.inner.interface);
            self.idle_transfer = Some(transfer);
            Poll::Ready(completion)
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion> {
        self.inner.notify.wait_timeout(timeout, || {
            take_completed_from_queue(&mut self.pending).map(|mut transfer| {
                let completion = transfer.take_completion(&self.inner.interface);
                self.idle_transfer = Some(transfer);
                completion
            })
        })
    }

    pub(crate) fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let inner = self.inner.clone();
        Blocking::new(move || {
            let endpoint = inner.address;
            debug!("Clear halt, endpoint {endpoint:02x}");
            unsafe {
                if WinUsb_ResetPipe(inner.interface.winusb_handle, endpoint) == TRUE {
                    Ok(())
                } else {
                    Err(match GetLastError() {
                        e => Error::new_os(ErrorKind::Other, "failed to clear halt", e),
                    })
                }
            }
        })
    }
}

impl Drop for WindowsEndpoint {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            debug!(
                "Dropping endpoint {:02x} with {} pending transfers",
                self.inner.address,
                self.pending.len()
            );
            self.cancel_all();
        }
    }
}

impl AsRef<Notify> for EndpointInner {
    fn as_ref(&self) -> &Notify {
        &self.notify
    }
}

impl Drop for EndpointInner {
    fn drop(&mut self) {
        let mut state = self.interface.state.lock().unwrap();
        state.endpoints.clear(self.address);
    }
}
