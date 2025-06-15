//! FFI types we're including here, as they're missing from io-kit-sys.
//! You may not want to stare too closely at this; it's tweaked bindgen output.
//!
//! Based on Kate Temkin's [usrs](https://github.com/ktemkin/usrs)
//! licensed under MIT OR Apache-2.0.
#![allow(
    non_camel_case_types,
    non_snake_case,
    dead_code,
    non_upper_case_globals
)]

use std::ffi::{c_int, c_void};

use core_foundation_sys::{
    base::{kCFAllocatorSystemDefault, mach_port_t, SInt32},
    dictionary::CFDictionaryRef,
    mach_port::CFAllocatorRef,
    runloop::CFRunLoopSourceRef,
    uuid::{CFUUIDBytes, CFUUIDRef},
};
use io_kit_sys::{
    ret::IOReturn,
    types::{io_iterator_t, io_service_t, IOByteCount},
    IOAsyncCallback1, IOAsyncCallback2,
};

//
// Constants.
//
const SYS_IOKIT: c_int = ((0x38) & 0x3f) << 26;
const SUB_IOKIT_USB: c_int = ((1) & 0xfff) << 14;

pub(crate) const kIOUSBUnknownPipeErr: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x61; // 0xe0004061  Pipe ref not recognized
pub(crate) const kIOUSBTooManyPipesErr: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x60; // 0xe0004060  Too many pipes
pub(crate) const kIOUSBNoAsyncPortErr: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x5f; // 0xe000405f  no async port
pub(crate) const kIOUSBNotEnoughPipesErr: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x5e; // 0xe000405e  not enough pipes in interface
pub(crate) const kIOUSBNotEnoughPowerErr: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x5d; // 0xe000405d  not enough power for selected configuration
pub(crate) const kIOUSBEndpointNotFound: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x57; // 0xe0004057  Endpoint Not found
pub(crate) const kIOUSBConfigNotFound: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x56; // 0xe0004056  Configuration Not found
pub(crate) const kIOUSBPortWasSuspended: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x52; // 0xe0004052  The transaction was returned because the port was suspended
pub(crate) const kIOUSBPipeStalled: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x4f; // 0xe000404f  Pipe has stalled, error needs to be cleared
pub(crate) const kIOUSBInterfaceNotFound: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x4e; // 0xe000404e  Interface ref not recognized
pub(crate) const kIOUSBLowLatencyBufferNotPreviouslyAllocated: c_int =
    SYS_IOKIT | SUB_IOKIT_USB | 0x4d; // 0xe000404d  Attempted to use user land low latency isoc calls w/out calling PrepareBuffer (on the data buffer) first
pub(crate) const kIOUSBLowLatencyFrameListNotPreviouslyAllocated: c_int =
    SYS_IOKIT | SUB_IOKIT_USB | 0x4c; // 0xe000404c  Attempted to use user land low latency isoc calls w/out calling PrepareBuffer (on the frame list) first
pub(crate) const kIOUSBHighSpeedSplitError: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x4b; // 0xe000404b  Error to hub on high speed bus trying to do split transaction
pub(crate) const kIOUSBSyncRequestOnWLThread: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x4a; // 0xe000404a  A synchronous USB request was made on the workloop thread (from a callback?).  Only async requests are permitted in that case
pub(crate) const kIOUSBDeviceNotHighSpeed: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x49; // 0xe0004049  Name is deprecated, see below
pub(crate) const kIOUSBDeviceTransferredToCompanion: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x49; // 0xe0004049  The device has been tranferred to another controller for enumeration
pub(crate) const kIOUSBClearPipeStallNotRecursive: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x48; // 0xe0004048  IOUSBPipe::ClearPipeStall should not be called recursively
pub(crate) const kIOUSBDevicePortWasNotSuspended: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x47; // 0xe0004047  Port was not suspended
pub(crate) const kIOUSBEndpointCountExceeded: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x46; // 0xe0004046  The endpoint was not created because the controller cannot support more endpoints
pub(crate) const kIOUSBDeviceCountExceeded: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x45; // 0xe0004045  The device cannot be enumerated because the controller cannot support more devices
pub(crate) const kIOUSBStreamsNotSupported: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x44; // 0xe0004044  The request cannot be completed because the XHCI controller does not support streams
pub(crate) const kIOUSBInvalidSSEndpoint: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x43; // 0xe0004043  An endpoint found in a SuperSpeed device is invalid (usually because there is no Endpoint Companion Descriptor)
pub(crate) const kIOUSBTooManyTransactionsPending: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x42; // 0xe0004042  The transaction cannot be submitted because it would exceed the allowed number of pending transactions
pub(crate) const kIOUSBTransactionReturned: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x50;
pub(crate) const kIOUSBTransactionTimeout: c_int = SYS_IOKIT | SUB_IOKIT_USB | 0x51;

pub(crate) const kIOUSBFindInterfaceDontCare: UInt16 = 0xFFFF;

//
// Type aliases.
//
pub(crate) type REFIID = CFUUIDBytes;
pub(crate) type LPVOID = *mut c_void;
pub(crate) type HRESULT = SInt32;
pub(crate) type UInt8 = ::std::os::raw::c_uchar;
pub(crate) type UInt16 = ::std::os::raw::c_ushort;
pub(crate) type UInt32 = ::std::os::raw::c_uint;
pub(crate) type UInt64 = ::std::os::raw::c_ulonglong;
pub(crate) type ULONG = ::std::os::raw::c_ulong;
pub(crate) type kern_return_t = ::std::os::raw::c_int;
pub(crate) type USBDeviceAddress = UInt16;
pub(crate) type AbsoluteTime = UnsignedWide;
pub(crate) type Boolean = std::os::raw::c_uchar;

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBEndpointProperties {
    pub bVersion: UInt8,
    pub bAlternateSetting: UInt8,
    pub bDirection: UInt8,
    pub bEndpointNumber: UInt8,
    pub bTransferType: UInt8,
    pub bUsageType: UInt8,
    pub bSyncType: UInt8,
    pub bInterval: UInt8,
    pub wMaxPacketSize: UInt16,
    pub bMaxBurst: UInt8,
    pub bMaxStreams: UInt8,
    pub bMult: UInt8,
    pub wBytesPerInterval: UInt16,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct NumVersion {
    pub nonRelRev: UInt8,
    pub stage: UInt8,
    pub minorAndBugRev: UInt8,
    pub majorRev: UInt8,
}

#[repr(C, packed(2))]
#[derive(Debug, Copy, Clone)]
pub struct UnsignedWide {
    pub lo: UInt32,
    pub hi: UInt32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBDevRequest {
    pub bmRequestType: UInt8,
    pub bRequest: UInt8,
    pub wValue: UInt16,
    pub wIndex: UInt16,
    pub wLength: UInt16,
    pub pData: *mut ::std::os::raw::c_void,
    pub wLenDone: UInt32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBFindInterfaceRequest {
    pub bInterfaceClass: UInt16,
    pub bInterfaceSubClass: UInt16,
    pub bInterfaceProtocol: UInt16,
    pub bAlternateSetting: UInt16,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBDevRequestTO {
    pub bmRequestType: UInt8,
    pub bRequest: UInt8,
    pub wValue: UInt16,
    pub wIndex: UInt16,
    pub wLength: UInt16,
    pub pData: *mut ::std::os::raw::c_void,
    pub wLenDone: UInt32,
    pub noDataTimeout: UInt32,
    pub completionTimeout: UInt32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBIsocFrame {
    pub frStatus: IOReturn,
    pub frReqCount: UInt16,
    pub frActCount: UInt16,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBLowLatencyIsocFrame {
    pub frStatus: IOReturn,
    pub frReqCount: UInt16,
    pub frActCount: UInt16,
    pub frTimeStamp: AbsoluteTime,
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBDescriptorHeader {
    pub bLength: u8,
    pub bDescriptorType: u8,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOCFPlugInInterfaceStruct {
    pub _reserved: *mut ::std::os::raw::c_void,
    pub QueryInterface: ::std::option::Option<
        unsafe extern "C" fn(
            thisPointer: *mut ::std::os::raw::c_void,
            iid: REFIID,
            ppv: *mut LPVOID,
        ) -> HRESULT,
    >,
    pub AddRef: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub Release: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub version: UInt16,
    pub revision: UInt16,
    pub Probe: ::std::option::Option<
        unsafe extern "C" fn(
            thisPointer: *mut ::std::os::raw::c_void,
            propertyTable: CFDictionaryRef,
            service: io_service_t,
            order: *mut SInt32,
        ) -> IOReturn,
    >,
    pub Start: ::std::option::Option<
        unsafe extern "C" fn(
            thisPointer: *mut ::std::os::raw::c_void,
            propertyTable: CFDictionaryRef,
            service: io_service_t,
        ) -> IOReturn,
    >,
    pub Stop: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> IOReturn,
    >,
}
pub type IOCFPlugInInterface = IOCFPlugInInterfaceStruct;

extern "C" {
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;

    pub fn IOCreatePlugInInterfaceForService(
        service: io_service_t,
        pluginType: CFUUIDRef,
        interfaceType: CFUUIDRef,
        theInterface: *mut *mut *mut IOCFPlugInInterface,
        theScore: *mut SInt32,
    ) -> kern_return_t;

    pub fn CFUUIDGetConstantUUIDWithBytes(
        alloc: CFAllocatorRef,
        byte0: UInt8,
        byte1: UInt8,
        byte2: UInt8,
        byte3: UInt8,
        byte4: UInt8,
        byte5: UInt8,
        byte6: UInt8,
        byte7: UInt8,
        byte8: UInt8,
        byte9: UInt8,
        byte10: UInt8,
        byte11: UInt8,
        byte12: UInt8,
        byte13: UInt8,
        byte14: UInt8,
        byte15: UInt8,
    ) -> CFUUIDRef;

}

pub fn kIOUsbDeviceUserClientTypeID() -> CFUUIDRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0x9d,
            0xc7,
            0xb7,
            0x80,
            0x9e,
            0xc0,
            0x11,
            0xD4,
            0xa5,
            0x4f,
            0x00,
            0x0a,
            0x27,
            0x05,
            0x28,
            0x61,
        )
    }
}

pub fn kIOUsbInterfaceUserClientTypeID() -> CFUUIDRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0x2d,
            0x97,
            0x86,
            0xc6,
            0x9e,
            0xf3,
            0x11,
            0xD4,
            0xad,
            0x51,
            0x00,
            0x0a,
            0x27,
            0x05,
            0x28,
            0x61,
        )
    }
}

pub fn kIOCFPlugInInterfaceID() -> CFUUIDRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            std::ptr::null(),
            0xC2,
            0x44,
            0xE8,
            0x58,
            0x10,
            0x9C,
            0x11,
            0xD4,
            0x91,
            0xD4,
            0x00,
            0x50,
            0xE4,
            0xC6,
            0x42,
            0x6F,
        )
    }
}

pub fn kIOUSBDeviceInterfaceID650() -> CFUUIDRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            kCFAllocatorSystemDefault,
            0x4A,
            0xAC,
            0x1B,
            0x2E,
            0x24,
            0xC2,
            0x47,
            0x6A,
            0x96,
            0x4D,
            0x91,
            0x33,
            0x35,
            0x34,
            0xF2,
            0xCC,
        )
    }
}

pub fn kIOUSBInterfaceInterfaceID700() -> CFUUIDRef {
    unsafe {
        CFUUIDGetConstantUUIDWithBytes(
            kCFAllocatorSystemDefault,
            0x17,
            0xF9,
            0xE5,
            0x9C,
            0xB0,
            0xA1,
            0x40,
            0x1D,
            0x9A,
            0xC0,
            0x8D,
            0xE2,
            0x7A,
            0xC6,
            0x04,
            0x7E,
        )
    }
}

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBConfigurationDescriptor {
    pub bLength: u8,
    pub bDescriptorType: u8,
    pub wTotalLength: u16,
    pub bNumInterfaces: u8,
    pub bConfigurationValue: u8,
    pub iConfiguration: u8,
    pub bmAttributes: u8,
    pub MaxPower: u8,
}
pub type IOUSBConfigurationDescriptorPtr = *mut IOUSBConfigurationDescriptor;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBDeviceStruct650 {
    pub _reserved: *mut ::std::os::raw::c_void,
    pub QueryInterface: ::std::option::Option<
        unsafe extern "C" fn(
            thisPointer: *mut ::std::os::raw::c_void,
            iid: REFIID,
            ppv: *mut LPVOID,
        ) -> HRESULT,
    >,
    pub AddRef: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub Release: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub CreateDeviceAsyncEventSource: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            source: *mut CFRunLoopSourceRef,
        ) -> IOReturn,
    >,
    pub GetDeviceAsyncEventSource: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> CFRunLoopSourceRef,
    >,
    pub CreateDeviceAsyncPort: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            port: *mut mach_port_t,
        ) -> IOReturn,
    >,
    pub GetDeviceAsyncPort: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> mach_port_t,
    >,
    pub USBDeviceOpen:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub USBDeviceClose:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub GetDeviceClass: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, devClass: *mut UInt8) -> IOReturn,
    >,
    pub GetDeviceSubClass: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devSubClass: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetDeviceProtocol: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devProtocol: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetDeviceVendor: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devVendor: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetDeviceProduct: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devProduct: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetDeviceReleaseNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devRelNum: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetDeviceAddress: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            addr: *mut USBDeviceAddress,
        ) -> IOReturn,
    >,
    pub GetDeviceBusPowerAvailable: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            powerAvailable: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetDeviceSpeed: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, devSpeed: *mut UInt8) -> IOReturn,
    >,
    pub GetNumberOfConfigurations: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, numConfig: *mut UInt8) -> IOReturn,
    >,
    pub GetLocationID: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            locationID: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetConfigurationDescriptorPtr: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            configIndex: UInt8,
            desc: *mut IOUSBConfigurationDescriptorPtr,
        ) -> IOReturn,
    >,
    pub GetConfiguration: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, configNum: *mut UInt8) -> IOReturn,
    >,
    pub SetConfiguration: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, configNum: UInt8) -> IOReturn,
    >,
    pub GetBusFrameNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            frame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub ResetDevice:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub DeviceRequest: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            req: *mut IOUSBDevRequest,
        ) -> IOReturn,
    >,
    pub DeviceRequestAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            req: *mut IOUSBDevRequest,
            callback: IOAsyncCallback1,
            refCon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub CreateInterfaceIterator: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            req: *mut IOUSBFindInterfaceRequest,
            iter: *mut io_iterator_t,
        ) -> IOReturn,
    >,
    pub USBDeviceOpenSeize:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub DeviceRequestTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            req: *mut IOUSBDevRequestTO,
        ) -> IOReturn,
    >,
    pub DeviceRequestAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            req: *mut IOUSBDevRequestTO,
            callback: IOAsyncCallback1,
            refCon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub USBDeviceSuspend: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, suspend: Boolean) -> IOReturn,
    >,
    pub USBDeviceAbortPipeZero:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub USBGetManufacturerStringIndex: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, msi: *mut UInt8) -> IOReturn,
    >,
    pub USBGetProductStringIndex: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, psi: *mut UInt8) -> IOReturn,
    >,
    pub USBGetSerialNumberStringIndex: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, snsi: *mut UInt8) -> IOReturn,
    >,
    pub USBDeviceReEnumerate: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, options: UInt32) -> IOReturn,
    >,
    pub GetBusMicroFrameNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            microFrame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub GetIOUSBLibVersion: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            ioUSBLibVersion: *mut NumVersion,
            usbFamilyVersion: *mut NumVersion,
        ) -> IOReturn,
    >,
    pub GetBusFrameNumberWithTime: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            frame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub GetUSBDeviceInformation: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, info: *mut UInt32) -> IOReturn,
    >,
    pub RequestExtraPower: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            type_: UInt32,
            requestedPower: UInt32,
            powerAvailable: *mut UInt32,
        ) -> IOReturn,
    >,
    pub ReturnExtraPower: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            type_: UInt32,
            powerReturned: UInt32,
        ) -> IOReturn,
    >,
    pub GetExtraPowerAllocated: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            type_: UInt32,
            powerAllocated: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetBandwidthAvailableForDevice: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            bandwidth: *mut UInt32,
        ) -> IOReturn,
    >,
    pub SetConfigurationV2: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            configNum: UInt8,
            startInterfaceMatching: bool,
            issueRemoteWakeup: bool,
        ) -> IOReturn,
    >,
    pub RegisterForNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            notificationMask: UInt64,
            callback: IOAsyncCallback2,
            refCon: *mut ::std::os::raw::c_void,
            pRegistrationToken: *mut UInt64,
        ) -> IOReturn,
    >,
    pub UnregisterNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            registrationToken: UInt64,
        ) -> IOReturn,
    >,
    pub AcknowledgeNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            notificationToken: UInt64,
        ) -> IOReturn,
    >,
}
pub type IOUSBDeviceInterface650 = IOUSBDeviceStruct650;

// Tweak: these are just function pointers to thread-safe functions,
// so add send and sync to the C-type. (Calling these from multiple threads
// may cause odd behavior on the USB bus, though, so we'll still want to wrap the
// device in Mutex somewhere up from here.)
unsafe impl Send for IOUSBDeviceInterface650 {}
unsafe impl Sync for IOUSBDeviceInterface650 {}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct IOUSBInterfaceStruct700 {
    pub _reserved: *mut ::std::os::raw::c_void,
    pub QueryInterface: ::std::option::Option<
        unsafe extern "C" fn(
            thisPointer: *mut ::std::os::raw::c_void,
            iid: REFIID,
            ppv: *mut LPVOID,
        ) -> HRESULT,
    >,
    pub AddRef: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub Release: ::std::option::Option<
        unsafe extern "C" fn(thisPointer: *mut ::std::os::raw::c_void) -> ULONG,
    >,
    pub CreateInterfaceAsyncEventSource: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            source: *mut CFRunLoopSourceRef,
        ) -> IOReturn,
    >,
    pub GetInterfaceAsyncEventSource: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> CFRunLoopSourceRef,
    >,
    pub CreateInterfaceAsyncPort: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            port: *mut mach_port_t,
        ) -> IOReturn,
    >,
    pub GetInterfaceAsyncPort: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> mach_port_t,
    >,
    pub USBInterfaceOpen:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub USBInterfaceClose:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub GetInterfaceClass: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, intfClass: *mut UInt8) -> IOReturn,
    >,
    pub GetInterfaceSubClass: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            intfSubClass: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetInterfaceProtocol: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            intfProtocol: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetDeviceVendor: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devVendor: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetDeviceProduct: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devProduct: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetDeviceReleaseNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            devRelNum: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetConfigurationValue: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, configVal: *mut UInt8) -> IOReturn,
    >,
    pub GetInterfaceNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            intfNumber: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetAlternateSetting: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            intfAltSetting: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetNumEndpoints: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            intfNumEndpoints: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetLocationID: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            locationID: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetDevice: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            device: *mut io_service_t,
        ) -> IOReturn,
    >,
    pub SetAlternateInterface: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            alternateSetting: UInt8,
        ) -> IOReturn,
    >,
    pub GetBusFrameNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            frame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub ControlRequest: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            req: *mut IOUSBDevRequest,
        ) -> IOReturn,
    >,
    pub ControlRequestAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            req: *mut IOUSBDevRequest,
            callback: IOAsyncCallback1,
            refCon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub GetPipeProperties: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            direction: *mut UInt8,
            number: *mut UInt8,
            transferType: *mut UInt8,
            maxPacketSize: *mut UInt16,
            interval: *mut UInt8,
        ) -> IOReturn,
    >,
    pub GetPipeStatus: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, pipeRef: UInt8) -> IOReturn,
    >,
    pub AbortPipe: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, pipeRef: UInt8) -> IOReturn,
    >,
    pub ResetPipe: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, pipeRef: UInt8) -> IOReturn,
    >,
    pub ClearPipeStall: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, pipeRef: UInt8) -> IOReturn,
    >,
    pub ReadPipe: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: *mut UInt32,
        ) -> IOReturn,
    >,
    pub WritePipe: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
        ) -> IOReturn,
    >,
    pub ReadPipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub WritePipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub ReadIsochPipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            frameStart: UInt64,
            numFrames: UInt32,
            frameList: *mut IOUSBIsocFrame,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub WriteIsochPipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            frameStart: UInt64,
            numFrames: UInt32,
            frameList: *mut IOUSBIsocFrame,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub ControlRequestTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            req: *mut IOUSBDevRequestTO,
        ) -> IOReturn,
    >,
    pub ControlRequestAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            req: *mut IOUSBDevRequestTO,
            callback: IOAsyncCallback1,
            refCon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub ReadPipeTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: *mut UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
        ) -> IOReturn,
    >,
    pub WritePipeTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
        ) -> IOReturn,
    >,
    pub ReadPipeAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub WritePipeAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub USBInterfaceGetStringIndex: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, si: *mut UInt8) -> IOReturn,
    >,
    pub USBInterfaceOpenSeize:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
    pub ClearPipeStallBothEnds: ::std::option::Option<
        unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void, pipeRef: UInt8) -> IOReturn,
    >,
    pub SetPipePolicy: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            maxPacketSize: UInt16,
            maxInterval: UInt8,
        ) -> IOReturn,
    >,
    pub GetBandwidthAvailable: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            bandwidth: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetEndpointProperties: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            alternateSetting: UInt8,
            endpointNumber: UInt8,
            direction: UInt8,
            transferType: *mut UInt8,
            maxPacketSize: *mut UInt16,
            interval: *mut UInt8,
        ) -> IOReturn,
    >,
    pub LowLatencyReadIsochPipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            frameStart: UInt64,
            numFrames: UInt32,
            updateFrequency: UInt32,
            frameList: *mut IOUSBLowLatencyIsocFrame,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub LowLatencyWriteIsochPipeAsync: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            buf: *mut ::std::os::raw::c_void,
            frameStart: UInt64,
            numFrames: UInt32,
            updateFrequency: UInt32,
            frameList: *mut IOUSBLowLatencyIsocFrame,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub LowLatencyCreateBuffer: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            buffer: *mut *mut ::std::os::raw::c_void,
            size: IOByteCount,
            bufferType: UInt32,
        ) -> IOReturn,
    >,
    pub LowLatencyDestroyBuffer: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            buffer: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub GetBusMicroFrameNumber: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            microFrame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub GetFrameListTime: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            microsecondsInFrame: *mut UInt32,
        ) -> IOReturn,
    >,
    pub GetIOUSBLibVersion: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            ioUSBLibVersion: *mut NumVersion,
            usbFamilyVersion: *mut NumVersion,
        ) -> IOReturn,
    >,
    pub FindNextAssociatedDescriptor: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            currentDescriptor: *const ::std::os::raw::c_void,
            descriptorType: UInt8,
        ) -> *mut IOUSBDescriptorHeader,
    >,
    pub FindNextAltInterface: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            current: *const ::std::os::raw::c_void,
            request: *mut IOUSBFindInterfaceRequest,
        ) -> *mut IOUSBDescriptorHeader,
    >,
    pub GetBusFrameNumberWithTime: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            frame: *mut UInt64,
            atTime: *mut AbsoluteTime,
        ) -> IOReturn,
    >,
    pub GetPipePropertiesV2: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            direction: *mut UInt8,
            number: *mut UInt8,
            transferType: *mut UInt8,
            maxPacketSize: *mut UInt16,
            interval: *mut UInt8,
            maxBurst: *mut UInt8,
            mult: *mut UInt8,
            bytesPerInterval: *mut UInt16,
        ) -> IOReturn,
    >,
    pub GetPipePropertiesV3: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            properties: *mut IOUSBEndpointProperties,
        ) -> IOReturn,
    >,
    pub GetEndpointPropertiesV3: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            properties: *mut IOUSBEndpointProperties,
        ) -> IOReturn,
    >,
    pub SupportsStreams: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            supportsStreams: *mut UInt32,
        ) -> IOReturn,
    >,
    pub CreateStreams: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
        ) -> IOReturn,
    >,
    pub GetConfiguredStreams: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            configuredStreams: *mut UInt32,
        ) -> IOReturn,
    >,
    pub ReadStreamsPipeTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
            buf: *mut ::std::os::raw::c_void,
            size: *mut UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
        ) -> IOReturn,
    >,
    pub WriteStreamsPipeTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
        ) -> IOReturn,
    >,
    pub ReadStreamsPipeAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub WriteStreamsPipeAsyncTO: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
            buf: *mut ::std::os::raw::c_void,
            size: UInt32,
            noDataTimeout: UInt32,
            completionTimeout: UInt32,
            callback: IOAsyncCallback1,
            refcon: *mut ::std::os::raw::c_void,
        ) -> IOReturn,
    >,
    pub AbortStreamsPipe: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            pipeRef: UInt8,
            streamID: UInt32,
        ) -> IOReturn,
    >,
    pub RegisterForNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            notificationMask: UInt64,
            callback: IOAsyncCallback2,
            refCon: *mut ::std::os::raw::c_void,
            pRegistrationToken: *mut UInt64,
        ) -> IOReturn,
    >,
    pub UnregisterNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            registrationToken: UInt64,
        ) -> IOReturn,
    >,
    pub AcknowledgeNotification: ::std::option::Option<
        unsafe extern "C" fn(
            self_: *mut ::std::os::raw::c_void,
            notificationToken: UInt64,
        ) -> IOReturn,
    >,
    pub RegisterDriver:
        ::std::option::Option<unsafe extern "C" fn(self_: *mut ::std::os::raw::c_void) -> IOReturn>,
}
pub type IOUSBInterfaceInterface700 = IOUSBInterfaceStruct700;

// Tweak: these are just function pointers to thread-safe functions,
// so add send and sync to the C-type. (Calling these from multiple threads
// may cause odd behavior on the USB bus, though, so we'll still want to wrap the
// device in Mutex somewhere up from here.)
unsafe impl Send for IOUSBInterfaceInterface700 {}
unsafe impl Sync for IOUSBInterfaceInterface700 {}
