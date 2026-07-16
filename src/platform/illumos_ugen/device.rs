use crate::{DeviceInfo, Error, Speed};

use super::DevfsPath;
use crate::bitset::EndpointBitSet;
use crate::descriptors::{
    parse_concatenated_config_descriptors, ConfigurationDescriptor, DeviceDescriptor,
    EndpointDescriptor, TransferType,
};
use crate::maybe_future::{blocking::Blocking, MaybeFuture};
use crate::platform::illumos_ugen::transfer::UsbResult;
use crate::platform::illumos_ugen::{errno_to_transfer_error, ugen_to_transfer_error, Errno};
use crate::platform::TransferData;
use crate::transfer::{
    internal::{take_completed_from_queue, Idle, Notify, Pending},
    Buffer, Completion, ControlIn, ControlOut, ControlType, Direction, Recipient, TransferError,
    SETUP_PACKET_SIZE,
};
use crate::ErrorKind;
use log::debug;
use rustix::{
    fd::{AsRawFd, OwnedFd},
    fs::{Mode, OFlags},
    io,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

//
// Useful USB constants. We use names that deliberately match those found in
// the header files found under usr/src/uts/common/sys/usb.
//

const USB_REQ_GET_DESCR: u8 = 0x06;
const USB_REQ_GET_CFG: u8 = 0x08;
const USB_EP_DIR_MASK: u8 = 0x80;

// These come from the USB spec, there are others but these
// are the ones necessary at the moment
enum DescriptorType {
    Device,
    Configuration { index: u8 },
    String { index: u8 },
}

impl DescriptorType {
    fn to_value(&self) -> u16 {
        match self {
            Self::Device => 1 << 8,
            Self::Configuration { index } => 2 << 8 | u16::from(*index),
            Self::String { index } => 3 << 8 | u16::from(*index),
        }
    }
}

pub(crate) struct IllumosEndpoint {
    inner: Arc<EndpointInner>,

    // Max packet size per the descriptor
    pub(crate) max_packet_size: usize,

    // A queue of pending transfers, expected to complete in order
    pending: VecDeque<Pending<super::TransferData>>,
}

impl IllumosEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.raw.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        for transfer in self.pending.iter_mut().rev() {
            transfer.cancel();
        }
    }

    /// Create a new transfer in the initial Idle state with the given Buffer
    fn make_aio_transfer(&mut self, buffer: Buffer) -> Idle<TransferData> {
        Idle::new(
            self.inner.clone(),
            match Direction::from_address(self.inner.raw.address) {
                Direction::In => super::TransferData::new_bulk_in(buffer),
                Direction::Out => super::TransferData::new_bulk_out(buffer),
            },
        )
    }

    /// Create a new transfer with the given buffer, and immediately mark it as
    /// errored and complete
    pub(crate) fn submit_err(&mut self, buffer: Buffer, error: TransferError) {
        assert_eq!(error, TransferError::InvalidArgument);
        let mut t = self.make_aio_transfer(buffer);
        *(t.status_mut()) = Some(Err(UsbResult::Errno(Errno::INVAL)));
        self.pending.push_back(t.simulate_complete());
    }

    /// Create a new transfer with the given buffer, and immediately submit it for
    /// AIO processing
    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let idle = self.make_aio_transfer(buffer);
        let pending = idle.raw_transfer(self.inner.fd.as_raw_fd(), self.inner.stat_fd.as_raw_fd());
        self.pending.push_back(pending);
    }

    /// Poll for a transfer currently in the Idle+Completed state
    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let completion = transfer.take_completion();
            Poll::Ready(completion)
        } else {
            Poll::Pending
        }
    }

    /// Perform a blocking wait for the next transfer to complete, with the given
    /// timeout
    pub(crate) fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion> {
        self.inner.notify.wait_timeout(timeout, || {
            take_completed_from_queue(&mut self.pending).map(|mut transfer| {
                let completion = transfer.take_completion();
                completion
            })
        })
    }

    pub(crate) fn clear_halt(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        // libusb had an approach to this that may not be appropriate, just remove
        // it for now and investigate later
        Blocking::new(move || {
            Err(Error::new(
                ErrorKind::Unsupported,
                "clear_halt not supported",
            ))
        })
    }
}

impl Drop for IllumosEndpoint {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            debug!(
                "Dropping endpoint {:02x} with {} pending transfers",
                self.inner.raw.address,
                self.pending.len()
            );
            self.cancel_all();
        }
    }
}

struct EndpointInner {
    raw: RawEndpoint,
    notify: Notify,
    interface: Arc<IllumosInterface>,
    fd: Arc<OwnedFd>,
    stat_fd: Arc<OwnedFd>,
}

impl Drop for EndpointInner {
    fn drop(&mut self) {
        let mut state = self.interface.state.lock().unwrap();
        state.endpoints.clear(self.raw.address);
    }
}

impl AsRef<Notify> for EndpointInner {
    fn as_ref(&self) -> &Notify {
        &self.notify
    }
}

#[derive(Clone)]
struct RawEndpoint {
    interface_number: u8,
    address: u8,
    transfer_type: TransferType,
    direction: Direction,
}

impl RawEndpoint {
    fn device_basename(&self) -> String {
        format!(
            "if{}{}{}",
            self.interface_number,
            match self.direction {
                Direction::In => "in",
                Direction::Out => "out",
            },
            self.address & !USB_EP_DIR_MASK,
        )
    }

    fn stat_basename(&self) -> String {
        format!("{}stat", self.device_basename())
    }

    fn open_flags(&self) -> OFlags {
        OFlags::CLOEXEC
            | match self.direction {
                Direction::In => OFlags::RDONLY,
                Direction::Out => OFlags::WRONLY,
            }
    }
}

// This is needed for full enumeration because the strings are needed
// for probe-rs to work
pub(crate) fn get_raw_string(fd: &OwnedFd, index: u8) -> Result<Vec<u8>, Error> {
    let descriptor_type = DescriptorType::String { index };

    let index = crate::descriptors::language_id::US_ENGLISH;

    let control = ControlIn {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: USB_REQ_GET_DESCR,
        value: descriptor_type.to_value(),
        index,
        length: 4096,
    };

    let packet = control.setup_packet();
    let setup_packet = packet.as_slice();

    let cnt = io::write(fd, setup_packet)
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to write", e))?;

    if cnt != setup_packet.len() {
        return Err(Error::new(ErrorKind::Other, "incomplete write"));
    }

    // We don't actually know the max length but this is a reasonable estimate that
    // other parts of nusb use
    let mut buf = vec![0u8; 4096];
    let cnt =
        io::read(fd, &mut buf).map_err(|e| Error::new_os(ErrorKind::Other, "failed to read", e))?;

    // This was a very sad string descriptor
    if cnt == 0 {
        return Err(Error::new(ErrorKind::Other, "empty string descriptor"));
    }

    buf.truncate(buf[0].into());
    Ok(buf)
}

fn get_dev_descriptor(
    fd: &OwnedFd,
    descriptor_type: DescriptorType,
    index: u16,
) -> Result<Vec<u8>, Error> {
    #[allow(non_snake_case)]
    let wValue: u16 = descriptor_type.to_value();

    let control = ControlIn {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: USB_REQ_GET_DESCR,
        value: wValue,
        index,
        length: crate::descriptors::DESCRIPTOR_LEN_DEVICE as u16,
    };

    let packet = control.setup_packet();
    let setup_packet = packet.as_slice();

    let cnt = io::write(fd, setup_packet)
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to write", e))?;

    if cnt != setup_packet.len() {
        return Err(Error::new(ErrorKind::Other, "incomplete write"));
    }

    let mut buf = vec![0u8; crate::descriptors::DESCRIPTOR_LEN_DEVICE as usize];
    let cnt =
        io::read(fd, &mut buf).map_err(|e| Error::new_os(ErrorKind::Other, "failed to read", e))?;
    if cnt != crate::descriptors::DESCRIPTOR_LEN_DEVICE as usize {
        return Err(Error::new(ErrorKind::Other, "short descriptor read"));
    }

    Ok(buf)
}

fn get_cfg_descriptors(
    fd: &OwnedFd,
    descriptor_type: DescriptorType,
    index: u16,
) -> Result<Vec<u8>, Error> {
    #[allow(non_snake_case)]
    let wValue: u16 = descriptor_type.to_value();

    let mut control = ControlIn {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: USB_REQ_GET_DESCR,
        value: wValue,
        index,
        length: crate::descriptors::DESCRIPTOR_LEN_CONFIGURATION as u16,
    };

    let packet = control.setup_packet();
    let setup_packet = packet.as_slice();

    let cnt = io::write(fd, setup_packet)
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to write", e))?;

    if cnt != setup_packet.len() {
        return Err(Error::new(ErrorKind::Other, "incomplete write"));
    }

    let mut buf = vec![0u8; crate::descriptors::DESCRIPTOR_LEN_CONFIGURATION as usize];
    let cnt =
        io::read(fd, &mut buf).map_err(|e| Error::new_os(ErrorKind::Other, "failed to read", e))?;
    if cnt != crate::descriptors::DESCRIPTOR_LEN_CONFIGURATION as usize {
        return Err(Error::new(ErrorKind::Other, "short descriptor read"));
    }

    let total = u16::from_le_bytes([buf[2], buf[3]]);

    // An empty descriptor will never be valid and will probably just error out
    if total == 0 {
        return Err(Error::new(ErrorKind::Other, "Empty descriptor"));
    }

    control.length = total;
    control.index = index;
    let packet = control.setup_packet();
    let setup_packet = packet.as_slice();

    let cnt = io::write(fd, setup_packet)
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to write", e))?;

    if cnt != setup_packet.len() {
        return Err(Error::new(ErrorKind::Other, "short descriptor write"));
    }

    let mut descriptors = vec![0u8; total as usize];
    let cnt = io::read(fd, &mut descriptors)
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to read", e))?;
    if cnt != total as usize {
        return Err(Error::new(ErrorKind::Other, "short descriptor read"));
    }

    Ok(descriptors)
}

fn get_configuration(fd: &OwnedFd) -> Result<u8, Error> {
    let control = ControlIn {
        control_type: ControlType::Standard,
        recipient: Recipient::Device,
        request: USB_REQ_GET_CFG,
        value: 0,
        index: 0,
        length: 1,
    };

    let mut buf = [0u8];

    let packet = control.setup_packet();
    let slice = packet.as_slice();

    let cnt =
        io::write(fd, slice).map_err(|e| Error::new_os(ErrorKind::Other, "failed to write", e))?;
    if cnt != slice.len() {
        return Err(Error::new(ErrorKind::Other, "short descriptor write"));
    }

    let cnt =
        io::read(fd, &mut buf).map_err(|e| Error::new_os(ErrorKind::Other, "failed to read", e))?;
    if cnt != buf.len() {
        return Err(Error::new(ErrorKind::Other, "short descriptor len"));
    }

    Ok(buf[0])
}

fn handle_errno_result(
    status: Result<usize, Errno>,
    stat_fd: &OwnedFd,
) -> Result<usize, TransferError> {
    const USB_LC_STAT_UNSPECIFIED_ERR: u32 = 0xe;

    let Err(errno) = status else {
        return status.map_err(|e| errno_to_transfer_error(e));
    };
    // The exact wording is that if the return value is -1 we should check the
    // stat fd
    if errno.raw_os_error() == -1 {
        let mut stat: [u8; 4] = [0; 4];
        match io::read(stat_fd, &mut stat) {
            Ok(4) => Err(ugen_to_transfer_error(u32::from_le_bytes(stat))),
            // man page example just returns the unspecified error
            Ok(_) => Err(ugen_to_transfer_error(USB_LC_STAT_UNSPECIFIED_ERR)),
            Err(errno) => Err(errno_to_transfer_error(errno)),
        }
    } else {
        // Some other error dealing with the reading/writing. Treat this
        // a a standard errno
        status.map_err(|e| errno_to_transfer_error(e))
    }
}

struct InternalFds {
    device_fd: OwnedFd,
    stat_fd: OwnedFd,
}

impl InternalFds {
    fn control_out(&mut self, out_buffer: Vec<u8>) -> Result<(), TransferError> {
        handle_errno_result(io::write(&self.device_fd, &out_buffer), &self.stat_fd).map(|_| ())
    }

    fn control_in(&mut self, out_buffer: Vec<u8>, length: usize) -> Result<Vec<u8>, TransferError> {
        handle_errno_result(io::write(&self.device_fd, &out_buffer), &self.stat_fd)?;

        let mut v = Vec::with_capacity(length);
        handle_errno_result(
            io::read(&self.device_fd, rustix::buffer::spare_capacity(&mut v)),
            &self.stat_fd,
        )?;
        Ok(v)
    }
}

pub(crate) struct IllumosDevice {
    // control transfers involve a write and a read on the same file descriptor
    // mutex protects against interleaved submission
    fds: Mutex<InternalFds>,
    device_descriptor: DeviceDescriptor,
    config_descriptors: Vec<u8>,
    active_config: u8,
    paths: DevfsPath,
    interfaces: HashMap<u8, Vec<RawEndpoint>>,
}

impl IllumosDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<IllumosDevice>, Error>> {
        //
        // We are going to open our control FD, and ask for descriptor
        // information.  (We expect this information to match that that's
        // already in the devinfo tree as the `usb-raw-cfg-descriptors`
        // property, but we don't cache that in `DeviceInfo`.)
        //
        let dpath = d.path.clone();

        Blocking::new(move || {
            let path = Path::new(
                dpath
                    .device_paths
                    .get("cntrl0")
                    .ok_or(Error::new(ErrorKind::Other, "not ugen").log_debug())?,
            );

            let device_fd = rustix::fs::open(path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
                .map_err(Error::from)?;

            let stat_path = Path::new(
                dpath
                    .device_paths
                    .get("cntrl0stat")
                    .ok_or(Error::new(ErrorKind::Other, "not ugen").log_debug())?,
            );

            let stat_fd =
                rustix::fs::open(stat_path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
                    .map_err(Error::from)?;

            let device_descriptor =
                DeviceDescriptor::new(&get_dev_descriptor(&device_fd, DescriptorType::Device, 0)?)
                    .ok_or(Error::new(ErrorKind::Other, "Invalid device descriptor"))?;
            let active_config = get_configuration(&device_fd)?;

            #[rustfmt::skip]
            let config_descriptors = get_cfg_descriptors(
                &device_fd, DescriptorType::Configuration { index: 0 }, 0
            )?;

            let c = ConfigurationDescriptor::new(&config_descriptors)
                .ok_or(Error::new(ErrorKind::Other, "Invalid config desc"))?;

            let interfaces = c
                .interfaces()
                .map(|i| {
                    let alt = i.first_alt_setting();
                    let interface_number = alt.interface_number();
                    let alt_endpoints = alt
                        .endpoints()
                        .map(|ep| RawEndpoint {
                            interface_number,
                            address: ep.address(),
                            direction: ep.direction(),
                            transfer_type: ep.transfer_type(),
                        })
                        .collect::<Vec<_>>();
                    (interface_number, alt_endpoints)
                })
                .collect::<HashMap<_, _>>();

            Ok(Arc::new(Self {
                fds: Mutex::new(InternalFds { device_fd, stat_fd }),
                device_descriptor,
                config_descriptors,
                active_config,
                paths: dpath.clone(),
                interfaces,
            }))
        })
    }

    pub(crate) fn device_descriptor(&self) -> DeviceDescriptor {
        self.device_descriptor.clone()
    }

    /// Perform a Control-In transfer with the given data. This transfer
    /// is performed in a blocking manner, on a background worker thread.
    ///
    /// NOTE: `_timeout` is currently not honored!
    pub(crate) fn control_in(
        self: Arc<Self>,
        data: ControlIn,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let mut out_buffer = Vec::with_capacity(SETUP_PACKET_SIZE);
        out_buffer.extend_from_slice(&data.setup_packet());

        Blocking::new(move || {
            let mut fds = self.fds.lock().unwrap();
            fds.control_in(out_buffer, data.length.into())
        })
    }

    /// Perform a Control-Out transfer with the given data. This transfer
    /// is performed in a blocking manner, on a background worker thread.
    ///
    /// NOTE: `_timeout` is currently not honored!
    pub(crate) fn control_out(
        self: Arc<Self>,
        data: ControlOut,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let mut out_buffer =
            Vec::with_capacity(SETUP_PACKET_SIZE.checked_add(data.data.len()).unwrap());
        out_buffer.extend_from_slice(&data.setup_packet());
        out_buffer.extend_from_slice(data.data);

        Blocking::new(move || {
            let mut fds = self.fds.lock().unwrap();
            fds.control_out(out_buffer)
        })
    }

    pub(crate) fn configuration_descriptors(
        &self,
    ) -> impl Iterator<Item = ConfigurationDescriptor<'_>> {
        parse_concatenated_config_descriptors(&self.config_descriptors)
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.active_config
    }

    #[allow(unused)]
    pub(crate) fn set_configuration(
        &self,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        // It doesn't look like libusb does this either since the model
        // of how ugen works doesn't match this
        Blocking::new(move || {
            Err(Error::new(
                ErrorKind::Unsupported,
                "set_configuration not supported",
            ))
        })
    }

    pub(crate) fn reset(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        // Another API that isn't as easily exposed via ugen
        Blocking::new(move || Err(Error::new(ErrorKind::Unsupported, "reset not supported")))
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<IllumosInterface>, Error>> {
        Blocking::new(move || {
            let Some(eps) = self.interfaces.get(&interface_number) else {
                return Err(Error::new(ErrorKind::Other, "invalid interface number").log_error());
            };

            let mut fds = HashMap::new();

            for ep in eps {
                let devname = ep.device_basename();

                let Some(path) = self.paths.device_paths.get(&devname) else {
                    return Err(Error::new(ErrorKind::Other, "bad device").log_error());
                };

                let fd =
                    rustix::fs::open(path, ep.open_flags(), Mode::empty()).map_err(Error::from)?;

                let statname = ep.stat_basename();
                let Some(path) = self.paths.device_paths.get(&statname) else {
                    return Err(Error::new(ErrorKind::Other, "bad device stat").log_error());
                };

                let stat_fd =
                    rustix::fs::open(path, ep.open_flags(), Mode::empty()).map_err(Error::from)?;

                fds.insert(
                    ep.address,
                    IllumosUsbFds {
                        fd: Arc::new(fd),
                        stat_fd: Arc::new(stat_fd),
                    },
                );
            }

            Ok(Arc::new(IllumosInterface {
                interface_number,
                fds,
                device: self.clone(),
                state: Mutex::new(InterfaceState::default()),
            }))
        })
    }

    #[allow(unused)]
    pub(crate) fn detach_and_claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<IllumosInterface>, Error>> {
        // We may eventually want to do something here to detach but this
        // is okay for now
        self.clone().claim_interface(interface_number)
    }

    pub(crate) fn speed(&self) -> Option<Speed> {
        None
    }
}

#[derive(Default)]
struct InterfaceState {
    alt_setting: u8,
    endpoints: EndpointBitSet,
}

struct IllumosUsbFds {
    fd: Arc<OwnedFd>,
    stat_fd: Arc<OwnedFd>,
}

pub(crate) struct IllumosInterface {
    pub(crate) interface_number: u8,
    pub(crate) device: Arc<IllumosDevice>,
    fds: HashMap<u8, IllumosUsbFds>,
    state: Mutex<InterfaceState>,
}

impl IllumosInterface {
    pub fn control_in(
        &self,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        self.device.clone().control_in(data, timeout)
    }

    pub fn control_out(
        self: Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.device.clone().control_out(data, timeout)
    }

    pub fn set_alt_setting(
        self: Arc<Self>,
        _alt_setting: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        // doesn't work exactly the same here
        Blocking::new(move || {
            Err(Error::new(
                ErrorKind::Unsupported,
                "set_alt_setting not supported",
            ))
        })
    }

    pub fn get_alt_setting(&self) -> u8 {
        self.state.lock().unwrap().alt_setting
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<IllumosEndpoint, Error> {
        let address = descriptor.address();
        let ep_type = descriptor.transfer_type();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        if state.endpoints.is_set(address) {
            return Err(Error::new(ErrorKind::Busy, "endpoint already in use").log_error());
        }
        let raw = self
            .device
            .interfaces
            .get(&self.interface_number)
            .ok_or(Error::new(ErrorKind::Other, "invalid interface number"))?
            .iter()
            .find(|x| x.address == address && x.transfer_type == ep_type)
            .ok_or(Error::new(ErrorKind::Other, "couldn't find the endpoint"))?;

        state.endpoints.set(address);
        let fds = self
            .fds
            .get(&address)
            .ok_or(Error::new(ErrorKind::Other, "couldn't find ep address"))?;
        Ok(IllumosEndpoint {
            inner: Arc::new(EndpointInner {
                raw: raw.clone(),
                interface: self.clone(),
                fd: fds.fd.clone(),
                stat_fd: fds.stat_fd.clone(),
                notify: Notify::new(),
            }),
            max_packet_size,

            pending: VecDeque::new(),
        })
    }
}

impl Drop for IllumosInterface {
    fn drop(&mut self) {
        // Nothing right now
    }
}
