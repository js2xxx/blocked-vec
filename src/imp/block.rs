use alloc::alloc::Global;
use core::{
    alloc::{Allocator, Layout},
    fmt,
    mem::{self, MaybeUninit},
    num::NonZeroUsize,
    ptr::NonNull,
    slice,
};

use crate::{IoSlice, IoSliceMut};

pub struct Block {
    ptr: NonNull<[u8]>,
    layout: Layout,
    len: usize,
}

// SAFETY: blocks share the same rights as `Box<[u8]>`.
unsafe impl Send for Block {}
// SAFETY: blocks share the same rights as `Box<[u8]>`.
unsafe impl Sync for Block {}

impl fmt::Debug for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(unsafe { MaybeUninit::slice_assume_init_ref(self.as_slice()) })
            .finish()
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        unsafe { Global.deallocate(self.ptr.as_non_null_ptr(), self.layout) };
    }
}

impl Block {
    pub fn with_len(layout: Layout, len: usize) -> Option<Self> {
        let count = len.div_ceil(layout.align());
        NonZeroUsize::new(count).map(|count| {
            let (layout, _) = layout.repeat(count.get()).expect("Invalid layout");
            Block {
                ptr: Global
                    .allocate_zeroed(layout)
                    .expect("Failed to allocate memory"),
                layout,
                len,
            }
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn from_buf(
        layout: Layout,
        skip: usize,
        buf: &mut &mut [impl IoSlice],
    ) -> Option<(Self, usize)> {
        let len: usize = buf.iter().map(|buf| buf.len()).sum();
        let mut block = Self::with_len(layout, skip + len)?;
        let written_len = block.write(skip, buf);
        Some((block, written_len))
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.layout.size()
    }

    pub fn as_slice(&self) -> &[MaybeUninit<u8>] {
        assert!(self.len <= self.capacity());
        let base = self.ptr.as_ptr() as *const MaybeUninit<u8>;
        unsafe { slice::from_raw_parts(base, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [MaybeUninit<u8>] {
        assert!(self.len <= self.capacity());
        let base = self.ptr.as_ptr() as *mut MaybeUninit<u8>;
        unsafe { slice::from_raw_parts_mut(base, self.len) }
    }

    fn spare_capacity_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        assert!(self.len <= self.capacity());
        unsafe {
            let base = self.ptr.as_ptr() as *mut MaybeUninit<u8>;
            let base = base.add(self.len);
            let len = self.capacity() - self.len;
            slice::from_raw_parts_mut(base, len)
        }
    }

    pub fn read(&self, start: usize, buf: &mut &mut [impl IoSliceMut]) -> usize {
        let mut start = start;
        let mut read_len = 0;
        for buf in buf.iter_mut() {
            if start >= self.len {
                break;
            } else {
                let len = (self.len - start).min(buf.len());
                let src = &self.as_slice()[start..][..len];
                // SAFETY: Transmuting `&mut [u8]` to `&mut [MaybeUninit<u8>]` is safe.
                let dst = unsafe { mem::transmute::<_, &mut [_]>(&mut buf[..len]) };
                dst.copy_from_slice(src);

                start += len;
                read_len += len;
            }
        }
        IoSliceMut::advance_slices(buf, read_len);
        read_len
    }

    pub fn write(&mut self, start: usize, buf: &mut &mut [impl IoSlice]) -> usize {
        let mut start = start;
        let mut written_len = 0;
        for buf in buf.iter_mut() {
            if start >= self.len {
                break;
            } else {
                let len = (self.len - start).min(buf.len());
                let dst = &mut self.as_mut_slice()[start..][..len];
                // SAFETY: Transmuting `&[u8]` to `&[MaybeUninit<u8>]` is safe.
                let src = unsafe { mem::transmute::<_, &[_]>(&buf[..len]) };
                dst.copy_from_slice(src);

                start += len;
                written_len += len;
            }
        }
        IoSlice::advance_slices(buf, written_len);
        written_len
    }

    pub fn extend(&mut self, skip: usize, buf: &mut &mut [impl IoSlice]) -> Result<usize, usize> {
        let capacity = self.capacity();
        let mut written_len = 0;

        let len = (capacity - self.len).min(skip);
        self.spare_capacity_mut()[..len].fill(MaybeUninit::new(0));
        self.len += len;
        written_len += len;

        for buf in buf.iter_mut() {
            let len = (capacity - self.len).min(buf.len());
            let dst = &mut self.spare_capacity_mut()[..len];
            // SAFETY: Transmuting `&[u8]` to `&[MaybeUninit<u8>]` is safe.
            let src = unsafe { mem::transmute::<_, &[_]>(&buf[..len]) };
            dst.copy_from_slice(src);

            self.len += len;
            written_len += len;
        }
        IoSlice::advance_slices(buf, written_len - skip);
        if self.len == capacity && !buf.is_empty() && !buf[0].is_empty() {
            Err(written_len)
        } else {
            Ok(written_len)
        }
    }

    pub fn truncate(&mut self, pos: usize) -> Option<bool> {
        if pos >= self.len {
            return None;
        }
        self.len = pos;
        Some(pos == 0)
    }
}
