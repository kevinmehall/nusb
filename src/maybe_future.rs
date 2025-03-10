use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll},
};

/// IO that may be performed synchronously or asynchronously.
///
/// A `MaybeFuture` can be run asynchronously with `.await`, or
/// run synchronously (blocking the current thread) with `.wait()`.
pub trait MaybeFuture: IntoFuture<IntoFuture: Send> + Send {
    /// Block waiting for the action to complete
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output;

    /// Apply a function to the output.
    fn map<T: FnOnce(Self::Output) -> R + Unpin + Send, R>(self, f: T) -> Map<Self, T>
    where
        Self: Sized,
    {
        Map {
            wrapped: self,
            func: f,
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows",
    target_os = "macos"
))]
pub mod blocking {
    use super::MaybeFuture;
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

    impl<F, R> MaybeFuture for Blocking<F>
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

impl<T> MaybeFuture for Ready<T>
where
    T: Send,
{
    fn wait(self) -> Self::Output {
        self.0
    }
}

pub struct Map<F, T> {
    wrapped: F,
    func: T,
}

impl<F: MaybeFuture, T: FnOnce(F::Output) -> R, R> IntoFuture for Map<F, T>
where
    T: Send,
{
    type Output = R;
    type IntoFuture = MapFut<F::IntoFuture, T>;

    fn into_future(self) -> Self::IntoFuture {
        MapFut {
            wrapped: self.wrapped.into_future(),
            func: Some(self.func),
        }
    }
}

impl<F: MaybeFuture, T: FnOnce(F::Output) -> R, R> MaybeFuture for Map<F, T>
where
    T: Send,
{
    fn wait(self) -> Self::Output {
        (self.func)(self.wrapped.wait())
    }
}

pub struct MapFut<F, T> {
    wrapped: F,
    func: Option<T>,
}

impl<F: Future, T: FnOnce(F::Output) -> R, R> Future for MapFut<F, T> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: structural pin projection: `self.wrapped` is always pinned.
        let wrapped = unsafe { self.as_mut().map_unchecked_mut(|s| &mut s.wrapped) };

        Future::poll(wrapped, cx).map(|output| {
            // SAFETY: `self.func` is never pinned.
            let func = unsafe { &mut self.as_mut().get_unchecked_mut().func };

            (func.take().expect("polled after completion"))(output)
        })
    }
}
