use std::{
    future::{Future, IntoFuture},
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

/// IO that may be performed synchronously or asynchronously.
///
/// A `MaybeFuture` can be run asynchronously with `.await`, or
/// run synchronously (blocking the current thread) with `.wait()`.
#[must_use = "must `.wait()` or `.await` to perform the action"]
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

    /// Maps the output's success value to a different value.
    fn map_ok<C, T, U, E>(self, c: C) -> impl MaybeFuture<Output = Result<U, E>>
    where
        Self: MaybeFuture<Output = Result<T, E>>,
        C: FnOnce(T) -> U + Unpin + NonWasmSend,
        Self: Sized,
    {
        self.map(|res| res.map(c))
    }

    /// Maps the output's error value to a different value.
    fn map_err<C, T, E, F>(self, c: C) -> impl MaybeFuture<Output = Result<T, F>>
    where
        Self: MaybeFuture<Output = Result<T, E>>,
        C: FnOnce(E) -> F + Unpin + NonWasmSend,
        Self: Sized,
    {
        self.map(|res| res.map_err(c))
    }

    /// Continue with another potentially asynchronous operation after success.
    ///
    /// When run with [`MaybeFuture::wait`], both operations execute synchronously.
    /// When awaited, the second operation starts after the first resolves.
    fn and_then<C, T, U, E, N>(self, c: C) -> impl MaybeFuture<Output = Result<U, E>>
    where
        Self: MaybeFuture<Output = Result<T, E>> + Sized,
        C: FnOnce(T) -> N + NonWasmSend,
        N: MaybeFuture<Output = Result<U, E>>,
    {
        AndThen {
            wrapped: self,
            func: c,
            next: PhantomData,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use std::marker::Send as NonWasmSend;

#[cfg(target_arch = "wasm32")]
pub trait NonWasmSend {}
#[cfg(target_arch = "wasm32")]
impl<T> NonWasmSend for T {}

#[cfg(not(target_arch = "wasm32"))]
pub use std::marker::Sync as NonWasmSync;

#[cfg(target_arch = "wasm32")]
pub trait NonWasmSync {}
#[cfg(target_arch = "wasm32")]
impl<T> NonWasmSync for T {}

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

/// Construct a [`MaybeFuture`] that is immediately ready.
pub fn ready<T: NonWasmSend>(value: T) -> impl MaybeFuture<Output = T> {
    Ready(value)
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

/// A [`MaybeFuture`] that chains a second operation after the first succeeds.
struct AndThen<F, C, N> {
    wrapped: F,
    func: C,
    next: PhantomData<fn() -> N>,
}

impl<F, C, N, T, U, E> IntoFuture for AndThen<F, C, N>
where
    F: MaybeFuture<Output = Result<T, E>>,
    C: FnOnce(T) -> N + NonWasmSend,
    N: MaybeFuture<Output = Result<U, E>>,
{
    type Output = Result<U, E>;
    type IntoFuture = AndThenFut<F::IntoFuture, C, N::IntoFuture>;

    fn into_future(self) -> Self::IntoFuture {
        AndThenFut {
            first: Some(Box::pin(self.wrapped.into_future())),
            func: Some(self.func),
            second: None,
        }
    }
}

impl<F, C, N, T, U, E> MaybeFuture for AndThen<F, C, N>
where
    F: MaybeFuture<Output = Result<T, E>>,
    C: FnOnce(T) -> N + NonWasmSend,
    N: MaybeFuture<Output = Result<U, E>>,
{
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(self) -> Self::Output {
        let value = self.wrapped.wait()?;
        (self.func)(value).wait()
    }
}

struct AndThenFut<F, C, N> {
    first: Option<Pin<Box<F>>>,
    func: Option<C>,
    second: Option<Pin<Box<N>>>,
}

impl<F, C, N> Unpin for AndThenFut<F, C, N> {}

impl<F, C, N, Next, T, U, E> Future for AndThenFut<F, C, N>
where
    F: Future<Output = Result<T, E>>,
    C: FnOnce(T) -> Next,
    Next: IntoFuture<Output = Result<U, E>, IntoFuture = N>,
    N: Future<Output = Result<U, E>>,
{
    type Output = Result<U, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            if let Some(second) = self.second.as_mut() {
                return second.as_mut().poll(cx);
            }

            let first = self.first.as_mut().expect("polled after completion");
            match first.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    self.first = None;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(value)) => {
                    self.first = None;
                    let next = self
                        .func
                        .take()
                        .expect("continuation missing after first operation completed");
                    self.second = Some(Box::pin(next(value).into_future()));
                }
            }
        }
    }
}
