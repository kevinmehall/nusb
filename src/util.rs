use std::mem::MaybeUninit;

/// Copies the elements from `src` to `dest`,
/// returning a mutable reference to the now initialized contents of `dest`.
///
/// Port of the `[MaybeUninit<T>]` method from std, which is not stable yet.
pub fn write_copy_of_slice<'a, T>(dest: &'a mut [MaybeUninit<T>], src: &[T]) -> &'a mut [T]
where
    T: Copy,
{
    // SAFETY: &[T] and &[MaybeUninit<T>] have the same layout
    let uninit_src: &[MaybeUninit<T>] = unsafe { std::mem::transmute(src) };

    dest.copy_from_slice(uninit_src);

    // SAFETY: Valid elements have just been copied into `self` so it is initialized
    unsafe { &mut *(dest as *mut [MaybeUninit<T>] as *mut [T]) }
}
