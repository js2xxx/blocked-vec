use core::{
    mem,
    ops::{Deref, DerefMut},
    slice,
};

pub trait IoSlice<'a>: Sized + Deref<Target = [u8]> {
    fn advance(&mut self, n: usize);

    fn advance_slices(bufs: &mut &mut [Self], n: usize) {
        // Number of buffers to remove.
        let mut remove = 0;
        // Total length of all the to be removed buffers.
        let mut accumulated_len = 0;
        for buf in bufs.iter() {
            if accumulated_len + buf.len() > n {
                break;
            } else {
                accumulated_len += buf.len();
                remove += 1;
            }
        }

        *bufs = &mut mem::take(bufs)[remove..];
        if bufs.is_empty() {
            assert!(
                n == accumulated_len,
                "advancing io slices beyond their length"
            );
        } else {
            bufs[0].advance(n - accumulated_len)
        }
    }
}

pub trait IoSliceMut<'a>: Sized + DerefMut<Target = [u8]> {
    fn advance(&mut self, n: usize);

    fn advance_slices(bufs: &mut &mut [Self], n: usize) {
        // Number of buffers to remove.
        let mut remove = 0;
        // Total length of all the to be removed buffers.
        let mut accumulated_len = 0;
        for buf in bufs.iter() {
            if accumulated_len + buf.len() > n {
                break;
            } else {
                accumulated_len += buf.len();
                remove += 1;
            }
        }

        *bufs = &mut mem::take(bufs)[remove..];
        if bufs.is_empty() {
            assert!(
                n == accumulated_len,
                "advancing io slices beyond their length"
            );
        } else {
            bufs[0].advance(n - accumulated_len)
        }
    }
}

#[derive(Copy, PartialEq, Eq, Clone, Debug)]
pub enum SeekFrom {
    Start(usize),
    Current(isize),
    End(isize),
}

impl<'a> IoSlice<'a> for &'a [u8] {
    fn advance(&mut self, n: usize) {
        *self = &self[n..];
    }
}

impl<'a> IoSliceMut<'a> for &'a mut [u8] {
    fn advance(&mut self, n: usize) {
        *self = unsafe {
            let (ptr, len) = (*self as *mut [u8]).to_raw_parts();
            let ptr = ptr.cast::<u8>().add(n);
            slice::from_raw_parts_mut(ptr, len - n)
        };
    }
}

#[cfg(feature = "std")]
impl<'a> IoSlice<'a> for std::io::IoSlice<'a> {
    fn advance(&mut self, n: usize) {
        self.advance(n)
    }

    fn advance_slices(bufs: &mut &mut [Self], n: usize) {
        std::io::IoSlice::advance_slices(bufs, n)
    }
}

#[cfg(feature = "std")]
impl<'a> IoSliceMut<'a> for std::io::IoSliceMut<'a> {
    fn advance(&mut self, n: usize) {
        self.advance(n)
    }

    fn advance_slices(bufs: &mut &mut [Self], n: usize) {
        std::io::IoSliceMut::advance_slices(bufs, n)
    }
}

#[cfg(feature = "std")]
impl From<SeekFrom> for std::io::SeekFrom {
    fn from(this: SeekFrom) -> Self {
        match this {
            SeekFrom::Start(pos) => std::io::SeekFrom::Start(pos as u64),
            SeekFrom::Current(pos) => std::io::SeekFrom::Current(pos as i64),
            SeekFrom::End(pos) => std::io::SeekFrom::End(pos as i64),
        }
    }
}

impl From<std::io::SeekFrom> for SeekFrom {
    fn from(this: std::io::SeekFrom) -> Self {
        match this {
            std::io::SeekFrom::Start(pos) => SeekFrom::Start(pos as usize),
            std::io::SeekFrom::Current(pos) => SeekFrom::Current(pos as isize),
            std::io::SeekFrom::End(pos) => SeekFrom::End(pos as isize),
        }
    }
}
