use crate::platform::illumos_ugen::{errno_to_transfer_error, ugen_to_transfer_error};
use crate::transfer::internal::Idle;
use crate::transfer::internal::Pending;
use crate::transfer::{
    internal::notify_completion, Buffer, Completion, ControlIn, ControlOut, TransferError,
    SETUP_PACKET_SIZE,
};
use core::mem::MaybeUninit;
use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::io;
use rustix::io::Errno;
use std::cell::UnsafeCell;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicPtr, Ordering};

// We have two possible cases for transfer errors: the raw read/write
// failed OR the read/write succeded and the stat fd returned an error.
// In the future I would love to differentiate these further...
#[derive(Debug, Clone, Copy)]
pub(crate) enum UsbResult {
    Errno(Errno),
    UgenStat(u32),
    Cancelled,
}

impl UsbResult {
    pub(crate) fn to_transfer_error(self) -> TransferError {
        match self {
            UsbResult::Errno(e) => errno_to_transfer_error(e),
            UsbResult::UgenStat(e) => ugen_to_transfer_error(e),
            UsbResult::Cancelled => TransferError::Cancelled,
        }
    }
}

type AioTransferStatus = Option<Result<usize, UsbResult>>;

/// SAFETY: This is used to statically assert that the `status` field of
/// `TransferData` is Copy, and therefore has no load bearing drop. This
/// is so we don't need to explicitly worry about dropping any old status
/// when setting the status after a transfer has been completed.
#[allow(dead_code)]
mod hidden {
    const fn assert_copy<T: Copy>(t: &T) -> T {
        *t
    }

    pub const _STATUS_COPY: super::AioTransferStatus = const { assert_copy(&None) };
}

/// TransferData is held in various nusb containers like `Idle<P>` and
/// `Pending<P>`, and is a little tricky to work with because it contains
/// data that is shared between "userspace" and the AIO callback machinery.
///
/// SAFETY: You should treat `TransferData` as follows:
///
/// * When directly owned, or via an authentic `&mut` reference: You have
///   complete control! Not shared at all.
/// * When accessed via `Idle<TransferData>`: This only occurs when the
///   Rust code has exclusive access, and therefore again, you are able to
///   do whatever you'd like. Not shared at all.
/// * When accessed via `*mut Pending<TransferData>`, the rules depend on
///   who "you" are:
///     * When you are the callback, e.g. `aio_callback`, you may READ all
///       fields, essentially `&TransferData`. You also have authority to
///       WRITE `status`. This ability persists until `aiocb` is written
///       to `null_mut`, signifying that the callback is complete.
///     * When you are NOT the callback, including any `drop` impls or other
///       polling code, you MUST NOT access `status` until AFTER `aiocb` is
///       `null_mut`.
/// * When registering an AIO transfer, the state flow of the `aiocb` field
///   is expected to go:
///     1. The user code moves the TransferData from Idle to Pending. This
///        is achieved by calling `Idle::<TransferData>::raw_transfer`.
///     2. `raw_transfer` allocates an `aiocb` structure, and sets the `aiocb`
///        field to the allocation pointer.
///     3. `raw_transfer` then attempts to register the AIO transaction.
///         * If this *fails*, the user code should immediately set the aiocb
///           field to null, and de-allocate the allocated aiocb struct. The
///           nusb machinery is notified, and the transfer is marked as ready
///           to be moved back to the Idle state. The flow ends here.
///         * If this continues, the transfer now continues the flow continues.
///     4. At some later point, `aio_callback` will be called with the pointer
///        to the `aiocb` struct. Note that the callback is called regardless of
///        whether the transfer completed (successfully or unsuccessfully), OR
///        if the transfer was cancelled due to `aio_cancel` being called. The
///        callback is then responsible for:
///         * Obtaining the outcome (success or failure, including cancellation)
///         * Setting the `status` field with that outcome
///         * AFTER setting the `status`, marking the `aiocb` field back to null
///         * de-allocating the `aiocb` structure.
pub(crate) struct TransferData {
    /// Status of the transfer. `None` when the transfer has not been completed,
    /// or after the completion has already been taken. `Some` when the transfer
    /// has been completed either by the AIO callback, or on some more immediate
    /// error case.
    ///
    /// This field is an `UnsafeCell` as it may be written through an aliased
    /// shared reference/pointer to `TransferData` when in the `Pending` state.
    ///
    /// SAFETY: See struct documentation for sharing rules. The contents of this
    /// UnsafeCell MUST be Copy/MUST NOT have a Drop impl, unless we update the
    /// code to properly drop the previous contents when setting.
    status: UnsafeCell<AioTransferStatus>,

    /// Information about the transfer, including the type of transfer and
    /// relevant buffer. Set to Some on creation, and set to None when
    /// the completion is taken.
    transfer: Option<AioTransferParts>,

    /// A pointer to an aiocb structure, set to null_mut when no outstanding
    /// callback/syscall is live. See the struct docs for details.
    aiocb: AtomicPtr<libc::aiocb>,

    // We need these for aio error handling via the raw fd. Both are set to
    // -1 on creation, and set when the transfer is started (moving from the
    // Idle state to Pending).
    raw_fd: i32,
    raw_stat_fd: i32,
}

/// SAFETY: TransferData is sometimes shared in the Pending state. Atomic operations
/// with aiocb are used to mediate shared access and inner mutability. See the struct
/// docs for TransferData for details on correct usage.
unsafe impl Send for TransferData {}

/// SAFETY: TransferData is sometimes shared in the Pending state. Atomic operations
/// with aiocb are used to mediate shared access and inner mutability. See the struct
/// docs for TransferData for details on correct usage.
unsafe impl Sync for TransferData {}

/// Components for an AIO transfer
struct AioTransferParts {
    /// The kind of transfer, e.g. Bulk In/Out
    kind: AioTransferType,
    /// The heap allocated buffer for the transfer
    ///
    /// NOTE: Although there is never aliased mutation of the *Buffer* itself, the
    /// pointee of the Buffer, e.g. the payload of the `Vec` that was used to create
    /// it WILL have aliased access. As `Buffer` implements `Deref`, we place the
    /// Buffer into an `UnsafeCell` to prevent accidental access while the operating
    /// system is potentially writing into this buffer.
    ///
    /// I'm not sure if this is *exactly* the right way to model this, but I also think
    /// it is sufficiently conservative. We MAY treat the buffer itself (e.g. ptr and len
    /// fields) as appropriately shared, however *dereferencing* the `buffer.ptr` field
    /// should ONLY be done when exclusive access to this field would be appropriate. At
    /// the moment, this is only done in the owned + Idle states. When moving to the Pending
    /// state, we only take the `ptr` field and place it in the `aiocb` struct, which grants
    /// the operating system to access it (and the ptr field should NOT be dereferenced
    /// for read/write access) UNTIL the callback is called, signifying the end of the OS'
    /// access to the payload.
    ///
    /// Also note: the contents of an `UnsafeCell` are dropped "normally", meaning we do
    /// not need any manual handling of this in the `Drop` impl of `AioTransferParts` (or
    /// of the outer `TransferData`).
    buffer: UnsafeCell<Buffer>,
}

/// Kinds of AIO transfers
enum AioTransferType {
    /// Bulk In Transfer
    BulkIn,
    /// Bulk Out Transfer
    BulkOut,
}

/// Transfer data used for blocking syscalls, currently only Control In/Out transfers.
/// Since this isn't done with AIO, it has no tricky usage requirements like the
/// main TransferData type. We simplify this significantly by just taking a `Vec`
pub(crate) struct BlockingTransferData {
    /// Control Packet plus data for ControlOut
    pub(super) out_buffer: Vec<u8>,
    /// size for reading in, 0 for ControlOut
    data_in_len: usize,
}

impl BlockingTransferData {
    pub(super) fn new_control_out(data: ControlOut) -> BlockingTransferData {
        let mut out_buffer =
            Vec::with_capacity(SETUP_PACKET_SIZE.checked_add(data.data.len()).unwrap());
        out_buffer.extend_from_slice(&data.setup_packet());
        out_buffer.extend_from_slice(data.data);
        BlockingTransferData {
            out_buffer,
            data_in_len: 0,
        }
    }

    pub(super) fn new_control_in(data: ControlIn) -> BlockingTransferData {
        let mut out_buffer = Vec::with_capacity(SETUP_PACKET_SIZE);
        out_buffer.extend_from_slice(&data.setup_packet());
        BlockingTransferData {
            out_buffer,
            data_in_len: data.length as usize,
        }
    }

    pub(super) fn blocking_out_transfer(
        &mut self,
        fd: &OwnedFd,
        stat_fd: &OwnedFd,
    ) -> Result<(), TransferError> {
        let Self {
            out_buffer,
            data_in_len: _,
        } = self;
        handle_errno_result(io::write(fd, out_buffer), stat_fd)
            .map(|_| ())
            .map_err(|e| e.to_transfer_error())
    }

    pub(super) fn blocking_in_transfer(
        &mut self,
        fd: &OwnedFd,
        stat_fd: &OwnedFd,
    ) -> Result<Vec<u8>, TransferError> {
        let Self {
            out_buffer,
            data_in_len,
        } = self;
        handle_errno_result(io::write(fd, out_buffer), stat_fd)
            .map_err(|e| e.to_transfer_error())?;

        let mut v = Vec::with_capacity(*data_in_len);
        let cnt = handle_errno_result(
            io::read(fd, rustix::buffer::spare_capacity(&mut v)),
            stat_fd,
        )
        .map_err(|e| e.to_transfer_error())?;
        Ok(v)
    }
}

impl Idle<TransferData> {
    pub(super) fn status_mut(&mut self) -> &mut Option<Result<usize, UsbResult>> {
        // SAFETY: In Idle, there is no aliasing, and it is acceptable to get a
        // mutable reference to the status field
        self.status.get_mut()
    }

    // TODO: This probably should be `take_completion(self)`, but that would
    // require `Idle::<P>::into_inner() -> P`. Our TransferData is relatively
    // cheap: it's really just the box we keep it in, so there's less incentive
    // to keep it, and this would *probably* let us simplify some of the `status`
    // and `transfer` invariants: they could maybe always be `Some` (and therefore not
    // an Option at all).
    pub fn take_completion(&mut self) -> Completion {
        let (len, status) = match self.status_mut().take().unwrap() {
            Ok(len) => (len, Ok(())),
            Err(err) => (0, Err(err.to_transfer_error())),
        };
        let AioTransferParts { kind, buffer } =
            self.transfer.take().expect("should have transfer here");
        let mut buffer = buffer.into_inner();
        match kind {
            AioTransferType::BulkIn => {
                buffer.len = len as u32;
                Completion {
                    status,
                    actual_len: len,
                    buffer,
                }
            }
            AioTransferType::BulkOut => Completion {
                status,
                actual_len: len,
                buffer,
            },
        }
    }

    pub(super) fn raw_transfer(mut self, fd: i32, stat_fd: i32) -> Pending<TransferData> {
        //
        // At the start, we are in the idle state, and have exclusive access.
        //
        let (aiocb_ptr, aio_start_func) = {
            let idle: &mut TransferData = &mut self;

            let (buffer_ptr, nbytes, dir) = {
                let xfer = idle.transfer.as_mut().unwrap();
                let AioTransferParts { kind, buffer } = xfer;
                let buffer = buffer.get_mut();
                match kind {
                    AioTransferType::BulkIn => {
                        (buffer.ptr, buffer.requested_len as usize, InternalDir::In)
                    }
                    AioTransferType::BulkOut => (buffer.ptr, buffer.len as usize, InternalDir::Out),
                }
            };

            idle.raw_stat_fd = stat_fd;
            idle.raw_fd = fd;

            // Depending on our direction, get the opcode and relevant "start" function
            type StartFn = unsafe extern "C" fn(*mut libc::aiocb) -> libc::c_int;
            let (opcode, aio_start_func): (libc::c_int, StartFn) = match dir {
                InternalDir::In => (libc::LIO_READ, libc::aio_read),
                InternalDir::Out => (libc::LIO_WRITE, libc::aio_write),
            };

            // Create our aiocb structure.
            // The assumption is we will only use this transfer request once.
            //
            // Store the aiocb ptr prior to calling the read/write function, as the
            // request is enqueued/active immediately.
            let aiocb_ptr = Box::into_raw(Box::new(libc::aiocb {
                aio_fildes: fd,
                aio_buf: buffer_ptr.cast::<libc::c_void>(),
                aio_nbytes: nbytes,

                // We're always at offset zero
                aio_offset: 0,
                // Default is fine
                aio_reqprio: 0,
                aio_sigevent: {
                    // SAFETY: `sigevent` has private padding fields, however the type itself has
                    // no particular runtime invariants, and we are about to initalize all
                    // visible fields before use. As this is a C-oriented structure, zero-initialization
                    // of all fields is appropriate here.
                    let mut sig_e =
                        unsafe { MaybeUninit::<libc::sigevent>::zeroed().assume_init() };

                    sig_e.sigev_notify = libc::SIGEV_THREAD;
                    // This is not used, here for clarity
                    sig_e.sigev_signo = 0;
                    sig_e.ss_sp = aio_callback as *mut libc::c_void;
                    sig_e.sigev_value = libc::sigval {
                        // Note: Fill in later! See the note in the unsafe block below
                        // for why we DON'T just make this:
                        // `sival_ptr: alias as *mut TransferData as *mut libc::c_void`
                        sival_ptr: std::ptr::null_mut(),
                    };
                    sig_e.sigev_notify_attributes = std::ptr::null();
                    sig_e
                },

                aio_lio_opcode: opcode,

                aio_resultp: libc::aio_result_t {
                    aio_return: 0,
                    aio_errno: 0,
                },
                aio_state: 0,
                aio__pad: [0; _],
            }));
            (aiocb_ptr, aio_start_func)
        };

        // END OF IDLE PHASE!
        //
        // BEGIN PENDING PHASE!
        let pending = self.pre_submit();
        let platform_ptr = pending.as_ptr();

        // SAFETY: We have NOT started the aio transfer yet, we are allowed to poke shared things
        // despite being in the pending state.
        let result = unsafe {
            // We can modify the contents of the aiocb as we have not launched it yet
            //
            // We are SPECIFICALLY writing this with `platform_ptr` AFTER we have moved to the
            // Pending state, for provenance reasons. `Pending::as_ptr()` gives us a pointer
            // to `TransferData`, but with the provenance of `TransferInner`. Since `notify_completion`
            // casts this pointer BACK to `TransferInner`, calling it with a pointer with ONLY
            // the `TransferData` as provenance is *technically* incorrect, at least under stacked
            // borrows (it might be allowed in the tree borrow model, I am unsure).
            //
            // This is all just James being overly pedantic, but it doesn't harm anything in
            // practice.
            (&raw mut (*aiocb_ptr).aio_sigevent.sigev_value.sival_ptr)
                .write(platform_ptr as *mut libc::c_void);
            // We can modify the contents of the platform ptr as we have not launched the AIO yet
            (*platform_ptr).aiocb.store(aiocb_ptr, Ordering::Release);
            // Here we go!
            aio_start_func(aiocb_ptr)
        };

        // aio failed, just notify the completion now
        // The status will be updated in the callback for other cases
        if result < 0 {
            // Safety: Although we are pending, the AIO callback failed, which means
            // there is no chance of aliasing. Okay to take exclusive access.
            unsafe {
                // Get error
                let res = Some(Err(UsbResult::Errno(Errno::from_raw_os_error(
                    *libc::___errno(),
                ))));

                let alias = &mut *platform_ptr;
                alias.status.get().write(res);

                // Release the aiocb we just created, as it will no longer be used,
                // and the callback will not perform the regular happy path drop
                alias.aiocb.store(std::ptr::null_mut(), Ordering::Release);
                let _ = Box::from_raw(aiocb_ptr);

                // Notify that the transfer is now complete (by way of error)
                notify_completion::<TransferData>(platform_ptr);
            }
        }

        // Return our transfer which is now in the pending state
        pending
    }
}

impl TransferData {
    pub(super) fn new_bulk_in(buffer: Buffer) -> TransferData {
        TransferData {
            transfer: Some(AioTransferParts {
                kind: AioTransferType::BulkIn,
                buffer: UnsafeCell::new(buffer),
            }),
            status: UnsafeCell::new(None),
            aiocb: AtomicPtr::new(std::ptr::null_mut()),
            raw_stat_fd: -1,
            raw_fd: -1,
        }
    }

    pub(super) fn new_bulk_out(buffer: Buffer) -> TransferData {
        TransferData {
            transfer: Some(AioTransferParts {
                kind: AioTransferType::BulkOut,
                buffer: UnsafeCell::new(buffer),
            }),
            status: UnsafeCell::new(None),
            aiocb: AtomicPtr::new(std::ptr::null_mut()),
            raw_stat_fd: -1,
            raw_fd: -1,
        }
    }
}

#[derive(Debug)]
enum InternalDir {
    In,
    Out,
}

const USB_LC_STAT_UNSPECIFIED_ERR: u32 = 0xe;

extern "C" fn aio_callback(arg: libc::sigval) {
    // SAFETY: aio callback will ONLY be called when Pending, and we can
    // always treat TransferData as shared while in the Pending state.
    let alias_ptr = arg.sival_ptr.cast::<TransferData>();
    let alias = unsafe { &*alias_ptr };

    let aiocb_ptr = alias.aiocb.load(Ordering::Acquire);
    assert_ne!(
        aiocb_ptr,
        std::ptr::null_mut(),
        "aiocb freed before AIO callback executed!"
    );

    let status = match unsafe { libc::aio_error(aiocb_ptr) } {
        // The handling here is a mess because if `aio_error` is 0 this should
        // always return something non-zero. This means the unwrap should
        // be fine
        0 => Ok(unsafe { libc::aio_return(aiocb_ptr).try_into().unwrap() }),

        // Once again, the ugen man page says to check this only if the return
        // is -1
        -1 => {
            let mut stat: [u8; 4] = [0; 4];
            match io::read(
                // SAFETY we expect the stat fd to still be alive at this point
                // and we stored it explicitly
                unsafe { BorrowedFd::borrow_raw(alias.raw_stat_fd) },
                &mut stat,
            ) {
                Ok(4) => Err(UsbResult::UgenStat(u32::from_le_bytes(stat))),
                // man page example just returns the unspecified error
                Ok(_) => Err(UsbResult::UgenStat(USB_LC_STAT_UNSPECIFIED_ERR)),
                // Return this errno?
                Err(errno) => Err(UsbResult::Errno(errno)),
            }
        }

        // Other includes ECANCELED, which would occur if the transfer had
        // been cancelled prior to execution
        other => Err(UsbResult::Errno(Errno::from_raw_os_error(other))),
    };

    unsafe {
        // we are done with our callback, and we are responsible for freeing the
        // aiocb we created.
        drop(Box::from_raw(aiocb_ptr));

        // ...set the status, which we (the callback!) are allowed to do in
        // the Pending state.
        alias.status.get().write(Some(status));

        // Mark the ptr as null to signal that the callback is complete
        alias.aiocb.store(std::ptr::null_mut(), Ordering::Release);

        // Notify that this transfer has completed
        notify_completion::<TransferData>(alias_ptr)
    }
}

fn handle_errno_result(
    status: Result<usize, Errno>,
    stat_fd: &OwnedFd,
) -> Result<usize, UsbResult> {
    let Err(errno) = status else {
        return status.map_err(UsbResult::Errno);
    };
    // The exact wording is that if the return value is -1 we should check the
    // stat fd
    if errno.raw_os_error() == -1 {
        let mut stat: [u8; 4] = [0; 4];
        match io::read(stat_fd, &mut stat) {
            Ok(4) => Err(UsbResult::UgenStat(u32::from_le_bytes(stat))),
            // man page example just returns the unspecified error
            Ok(_) => Err(UsbResult::UgenStat(USB_LC_STAT_UNSPECIFIED_ERR)),
            Err(errno) => Err(UsbResult::Errno(errno)),
        }
    } else {
        // Some other error dealing with the reading/writing. Treat this
        // a a standard errno
        status.map_err(UsbResult::Errno)
    }
}

impl Pending<TransferData> {
    pub(super) fn cancel(&self) {
        // SAFETY: In the Pending state, TransferData is always treated as
        // aliased, therefore it is valid to take a shared reference.
        let alias: *const TransferData = self.as_ptr().cast_const();
        let alias: &TransferData = unsafe { &*alias };

        // If the ptr is clear, we assume that things have already completed naturally
        // via callback, or was cancelled some other way. Don't continue cleaning up.
        let aiocb_ptr = alias.aiocb.load(Ordering::Acquire);
        if aiocb_ptr.is_null() {
            return;
        }

        // Ask the OS to cancel our pending AIO request
        //
        // Note: This is *asynchronous*, the callback will STILL be called regardless
        // of calling aio_cancel/what this returns, it'll just notice a failure upon
        // calling `aio_read` or `aio_error` when it DOES run.
        let res = unsafe { libc::aio_cancel(alias.raw_fd, aiocb_ptr) };

        // RETURN VALUES
        //     The aio_cancel() function returns the value AIO_CANCELED to the calling
        //     process if the requested operation(s) were canceled. The value
        //     AIO_NOTCANCELED is returned if at least one of the requested
        //     operation(s) cannot be canceled because it is in progress. In this
        //     case, the state of the other operations, if any, referenced in the call
        //     to aio_cancel() is not indicated by the return value of aio_cancel().
        //     The application may determine the state of affairs for these operations
        //     by using aio_error(3C). The value AIO_ALLDONE is returned if all of the
        //     operations have already completed. Otherwise, the function returns −1
        //     and sets errno to indicate the error.
        match res {
            libc::AIO_CANCELED => {
                // The request has been cancelled, when the callback is eventually run, it
                // will do cleanup for us.
                log::trace!("Successfully cancelled transfer");
            }
            libc::AIO_ALLDONE => {
                // Okay, this is not great. Our atomic pointer WASN'T Null, but ALSO the
                // callback wasn't marked as cancelled. Do one quick check JUST IN CASE
                // there was a race between `aiocb.load` and `aio_cancel`. If the pointer
                // is NOW null, we just observed a little race, and we can avoid any further
                // remediation
                let aiocb_ptr2 = alias.aiocb.load(Ordering::Acquire);
                if aiocb_ptr2.is_null() {
                    // Whew. Just a little race. The callback already handled cleanup.
                    return;
                }

                // Now we are in the Cool Zone. The transfer is ALLDONE according to the
                // operating system, but the callback DIDN'T clear the aiocb buffer.
                // Something has gone seriously wrong, and let's not continue out of
                // an abundance of caution
                panic!("Indeterminate outcome reached on cancelling a transfer. This is a program error.");
            }
            libc::AIO_NOTCANCELED => {
                // According to the man page: "At least one of the requests specified was not canceled
                // because it was in progress."
                log::debug!("Observed AIO_NOTCANCELED while cancelling a transfer, the transfer will still execute.");
            }
            other => {
                // ERRORS
                //     The aio_cancel() function will fail if:
                //     EBADF
                //         The fildes argument is not a valid file descriptor.
                //     ENOSYS
                //         The aio_cancel() function is not supported.
                //
                // Either of these are indicators that things did NOT go well, and we should perform cleanup.
                // We're not checking errno because we don't *particularly* care why we got here.
                log::error!(
                    "Observed unexpected return code {other} while cancelling fd:{}",
                    alias.raw_fd
                );
            }
        }
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        // If we are dropping, then aiocb MUST NOT be non-null, as that would mean that
        // there is an aiocb with a reference to the TransferData and buffer we are
        // about to drop.
        //
        // When TransferData exists as an owned item, or as part of an `Idle<TransferData>`,
        // it should NEVER have a non-null `aiocb` pointer. This means that if we are dropped
        // in these states, we should never run afoul of this assert. When a `Pending<TransferData>`
        // is moved back to an `Idle<TransferData>, it is because the callback has run, and it
        // was responsible for clearing the `aiocb` pointer. There is no way to return to an
        // owned `TransferData` from the `Idle` or `Pending` states. Even if a `Pending<TransferData>`
        // was created, but the transfer failed to be registered: it is the responsibility of
        // `raw_transfer` to re-clear and de-allocate the `aiocb` field.
        //
        // Note that EVEN IF a `Pending<TransferData>` is dropped after a transfer is registered,
        // the `TransferInner` that contains it is NOT also immediately dropped: it is marked as
        // "Abandoned", and leaked, and the `TransferData` itself will only be restored and dropped
        // once `notify_completion` is called, which notices that the `TransferInner` was
        // abandoned, so drop should now be called.
        //
        // This means: if we EVER actually get to dropping the `TransferData` and the
        // `aiocb` pointer is NOT null: something very bad/unexpected has occurred!
        assert_eq!(
            self.aiocb.load(Ordering::Acquire),
            std::ptr::null_mut(),
            "Dropped a TransferData while an aiocb was still live, this is a bug"
        );
    }
}
