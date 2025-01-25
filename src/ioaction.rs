use std::future::IntoFuture;

/// IO that may be performed synchronously or asynchronously.
///
/// An `IOAction` can be run asynchronously with `.await`, or
/// run synchronously (blocking the current thread) with `.wait()`.
pub trait IoAction: IntoFuture {
    /// Block waiting for the action to complete
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output;
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows",
    target_os = "macos"
))]
pub mod blocking {
    use super::IoAction;
    use std::future::IntoFuture;

    /// Wrapper that invokes a FnOnce on a background thread when
    /// called asynchronously, or directly when called synchronously.
    pub struct Blocking<F> {
        f: F,
    }

    impl<F> Blocking<F> {
        pub fn new(f: F) -> Self {
            Self { f }
        }
    }

    impl<F, R> IntoFuture for Blocking<F>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        type Output = R;

        type IntoFuture = blocking::Task<R, ()>;

        fn into_future(self) -> Self::IntoFuture {
            blocking::unblock(self.f)
        }
    }

    impl<F, R> IoAction for Blocking<F>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        fn wait(self) -> R {
            (self.f)()
        }
    }
}

pub(crate) struct Ready<T>(pub(crate) T);

impl<T> IntoFuture for Ready<T> {
    type Output = T;
    type IntoFuture = std::future::Ready<T>;

    fn into_future(self) -> Self::IntoFuture {
        std::future::ready(self.0)
    }
}

impl<T> IoAction for Ready<T> {
    fn wait(self) -> Self::Output {
        self.0
    }
}
