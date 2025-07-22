use std::{
    future::{Future, IntoFuture},
    pin::Pin,
    task::{Context, Poll},
};

/// IO that may be performed synchronously or asynchronously.
///
/// A `MaybeFuture` can be run asynchronously with `.await`, or
/// run synchronously (blocking the current thread) with `.wait()`.
pub trait MaybeFuture: IntoFuture<IntoFuture: NonWasmSend> + NonWasmSend {
    /// Block waiting for the action to complete
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output;

    /// Apply a function to the output.
    fn map<T: FnOnce(Self::Output) -> R + Unpin + NonWasmSend, R>(self, f: T) -> Map<Self, T>
    where
        Self: Sized,
    {
        Map {
            wrapped: self,
            func: f,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use std::marker::Send as NonWasmSend;

#[cfg(target_arch = "wasm32")]
pub trait NonWasmSend {}
#[cfg(target_arch = "wasm32")]
impl<T> NonWasmSend for T {}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows",
    target_os = "macos"
))]
pub mod blocking {
    use super::MaybeFuture;
    use std::{
        future::{Future, IntoFuture},
        pin::Pin,
        task::{Context, Poll},
    };

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

        type IntoFuture = BlockingTask<R>;

        fn into_future(self) -> Self::IntoFuture {
            BlockingTask::spawn(self.f)
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

    #[cfg(feature = "smol")]
    pub struct BlockingTask<R>(blocking::Task<R, ()>);

    // If both features are enabled, use `smol` because it does not
    // require the runtime to be explicitly started
    #[cfg(all(feature = "tokio", not(feature = "smol")))]
    pub struct BlockingTask<R>(tokio::task::JoinHandle<R>);

    #[cfg(not(any(feature = "smol", feature = "tokio")))]
    pub struct BlockingTask<R>(Option<R>);

    impl<R: Send + 'static> BlockingTask<R> {
        #[cfg(feature = "smol")]
        fn spawn(f: impl FnOnce() -> R + Send + 'static) -> Self {
            Self(blocking::unblock(f))
        }

        #[cfg(all(feature = "tokio", not(feature = "smol")))]
        fn spawn(f: impl FnOnce() -> R + Send + 'static) -> Self {
            Self(tokio::task::spawn_blocking(f))
        }

        #[cfg(not(any(feature = "smol", feature = "tokio")))]
        fn spawn(_f: impl FnOnce() -> R + Send + 'static) -> Self {
            panic!("Awaiting blocking syscall without an async runtime: enable the `smol` or `tokio` feature of nusb.");
        }
    }

    impl<R> Unpin for BlockingTask<R> {}

    impl<R> Future for BlockingTask<R> {
        type Output = R;

        #[cfg(feature = "smol")]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            Pin::new(&mut self.0).poll(cx)
        }

        #[cfg(all(feature = "tokio", not(feature = "smol")))]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match Pin::new(&mut self.0).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(r)) => Poll::Ready(r),
                Poll::Ready(Err(e)) if e.is_cancelled() => Poll::Pending, // Can happen during runtime shutdown
                Poll::Ready(Err(e)) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Poll::Ready(Err(e)) => panic!("Error from tokio blocking task: {e}"),
            }
        }

        #[cfg(not(any(feature = "smol", feature = "tokio")))]
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            unreachable!()
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

impl<T: NonWasmSend> MaybeFuture for Ready<T> {
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        self.0
    }
}

pub struct Map<F, T> {
    wrapped: F,
    func: T,
}

impl<F: MaybeFuture, T: FnOnce(F::Output) -> R, R> IntoFuture for Map<F, T> {
    type Output = R;
    type IntoFuture = MapFut<F::IntoFuture, T>;

    fn into_future(self) -> Self::IntoFuture {
        MapFut {
            wrapped: self.wrapped.into_future(),
            func: Some(self.func),
        }
    }
}

impl<F: MaybeFuture, T: FnOnce(F::Output) -> R + NonWasmSend, R> MaybeFuture for Map<F, T> {
    #[cfg(not(target_arch = "wasm32"))]
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
