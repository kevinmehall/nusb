use std::{ffi::c_void, ptr, time::Duration};
use windows_sys::Win32::{
    Foundation::{GetLastError, FILETIME},
    System::Threading::{
        CloseThreadpoolTimer, CreateThreadpoolTimer, SetThreadpoolTimer,
        WaitForThreadpoolTimerCallbacks, PTP_CALLBACK_INSTANCE, PTP_TIMER,
    },
};

pub struct Timer {
    timer: PTP_TIMER,
}

fn duration_to_filetime(duration: Duration) -> FILETIME {
    let time = i64::try_from(duration.as_micros())
        .unwrap_or(i64::MAX)
        .saturating_mul(-10); // in 100-nanosecond intervals, negative for relative time
    FILETIME {
        dwLowDateTime: (time & 0xFFFFFFFF) as u32,
        dwHighDateTime: (time >> 32) as u32,
    }
}

impl Timer {
    /// Create and arm a timer.
    ///
    /// SAFETY: the caller must ensure that it is safe for the callback to be
    /// called with the given data on another thread, and must remain valid
    /// at any time the timer may trigger.
    pub unsafe fn new(
        callback: unsafe extern "system" fn(PTP_CALLBACK_INSTANCE, *mut c_void, PTP_TIMER),
        callback_data: *mut c_void,
    ) -> Result<Self, ()> {
        let timer =
            unsafe { CreateThreadpoolTimer(Some(callback), callback_data, ptr::null_mut()) };
        if timer == 0 {
            let e = unsafe { GetLastError() };
            log::error!("CreateThreadpoolTimer failed with error {e}");
            return Err(());
        }
        Ok(Timer { timer })
    }

    /// Arm the timer.
    pub fn set(&self, timeout: Duration) {
        let tm = duration_to_filetime(timeout);
        unsafe { SetThreadpoolTimer(self.timer, &tm, 0, 0) };
    }

    /// Cancel the timer and block until any in-flight callback completes.
    pub fn cancel_and_wait(&self) {
        unsafe {
            SetThreadpoolTimer(self.timer, ptr::null_mut(), 0, 0);
            WaitForThreadpoolTimerCallbacks(self.timer, 1);
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe { CloseThreadpoolTimer(self.timer) }
    }
}
