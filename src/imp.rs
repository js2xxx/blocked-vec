use alloc::{vec, vec::Vec};
use core::{
    alloc::Layout,
    cmp,
    iter::FusedIterator,
    mem::{self, MaybeUninit},
    ops::{Bound, RangeBounds},
    slice,
};

use crate::{IoSlice, IoSliceMut, SeekFrom};

mod block;

use self::block::Block;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct Seeker {
    current_block: usize,
    current_pos_in_block: usize,
    current_pos: usize,
    end_pos: usize,
}

impl Seeker {
    fn start(len: usize) -> Seeker {
        Seeker {
            current_block: 0,
            current_pos_in_block: 0,
            current_pos: 0,
            end_pos: len,
        }
    }

    #[inline]
    fn is_at_end(&self) -> bool {
        self.current_pos >= self.end_pos
    }

    fn read(&mut self, mut buf: &mut [impl IoSliceMut], blocks: &[Block]) -> Result<usize, usize> {
        let mut read_len = 0;
        loop {
            let block = blocks.get(self.current_block).ok_or(read_len)?;
            let len = block.read(self.current_pos_in_block, &mut buf);
            if len > 0 {
                read_len += len;
                self.advance(len, blocks).expect("Inconsistent seeker");
            } else {
                break Ok(read_len);
            }
        }
    }

    fn write(
        &mut self,
        buf: &mut &mut [impl IoSlice],
        blocks: &mut [Block],
    ) -> Result<usize, usize> {
        let mut written_len = 0;
        loop {
            let block = blocks.get_mut(self.current_block).ok_or(written_len)?;
            let len = block.write(self.current_pos_in_block, buf);
            if len > 0 {
                written_len += len;
                self.advance(len, blocks).expect("Inconsistent seeker");
            } else {
                break Ok(written_len);
            }
        }
    }

    fn append_end(&mut self, extra: usize) {
        if self.current_pos >= self.end_pos {
            self.current_pos += extra;
            self.end_pos = self.current_pos;
        } else {
            self.end_pos += extra;
        }
    }

    fn append_new(&mut self, block: &Block) {
        if self.current_pos == self.end_pos {
            self.current_pos += block.len();
            self.current_block += 1;
            self.current_pos_in_block = 0;
        }
        self.end_pos += block.len();
    }

    fn extend_end(&mut self, extra: usize, last: &Block) {
        if (self.end_pos..(self.end_pos + extra)).contains(&self.current_pos) {
            self.current_block -= 1;
            self.current_pos_in_block += last.len() - extra;
        } else if self.current_pos >= self.end_pos + extra {
            self.current_pos_in_block -= extra;
        }
        self.end_pos += extra;
    }

    fn extend_new(&mut self, len: usize) {
        if self.current_pos_in_block >= len {
            self.current_block += 1;
            self.current_pos_in_block -= len;
        }
        self.end_pos += len;
    }

    fn truncate(&mut self, new_pos: usize, blocks: &[Block]) -> Option<(usize, usize)> {
        if self.end_pos <= new_pos {
            return None;
        }
        self.end_pos = new_pos;

        match self.current_pos.cmp(&new_pos) {
            cmp::Ordering::Less => {
                if blocks.len() <= self.current_block {
                    None
                } else {
                    let mut pos = self.current_pos - self.current_pos_in_block;
                    for (bi_delta, block) in blocks[self.current_block..].iter().enumerate() {
                        if new_pos < block.len() + pos {
                            return Some((self.current_block + bi_delta, new_pos - pos));
                        }
                        pos += block.len();
                    }
                    None
                }
            }
            cmp::Ordering::Equal => Some(if self.current_pos_in_block > 0 {
                self.current_block += 1;
                let pos_in_block = mem::replace(&mut self.current_pos_in_block, 0);
                (self.current_block - 1, pos_in_block)
            } else {
                (self.current_block, 0)
            }),
            cmp::Ordering::Greater => {
                let mut pos = self.current_pos_in_block;
                for (bi_delta, block) in blocks[..self.current_block].iter().rev().enumerate() {
                    pos += block.len();
                    if self.current_pos <= new_pos + pos {
                        self.current_block -= bi_delta;
                        self.current_pos_in_block = self.current_pos - new_pos;
                        return Some((self.current_block - 1, pos + new_pos - self.current_pos));
                    }
                }
                unreachable!()
            }
        }
    }

    fn advance(&mut self, delta: usize, blocks: &[Block]) -> Option<usize> {
        let new_pos = self.current_pos + delta;
        if new_pos >= self.end_pos {
            self.current_block = blocks.len();
            self.current_pos_in_block = new_pos - self.end_pos;
            self.current_pos = new_pos;
            self.end_pos = new_pos;
            return Some(new_pos);
        }
        let mut pos = self.current_pos_in_block + delta;
        for (bi_delta, block) in blocks[self.current_block..].iter().enumerate() {
            if pos < block.len() {
                self.current_block += bi_delta;
                self.current_pos_in_block = pos;
                self.current_pos = new_pos;
                return Some(new_pos);
            }
            pos -= block.len();
        }
        None
    }

    fn seek_from_start(&mut self, start: usize, blocks: &[Block]) -> Option<usize> {
        let new_pos = start;
        if new_pos >= self.end_pos {
            self.current_block = blocks.len();
            self.current_pos_in_block = new_pos - self.end_pos;
            self.current_pos = new_pos;
            self.end_pos = new_pos;
            return Some(new_pos);
        }
        let mut pos = start;
        for (bi, block) in blocks.iter().enumerate() {
            if pos < block.len() {
                self.current_block = bi;
                self.current_pos_in_block = pos;
                self.current_pos = new_pos;
                return Some(new_pos);
            }
            pos -= block.len();
        }
        None
    }

    fn seek_bound(&mut self, bound: usize, blocks: &[Block]) {
        let mid = self.end_pos / 2;
        if bound < mid {
            self.seek_from_start(bound, blocks);
        } else {
            self.seek_from_end(-(self.end_pos.saturating_sub(bound) as isize), blocks);
        }
    }

    fn seek_from_end(&mut self, end: isize, blocks: &[Block]) -> Option<usize> {
        if end >= 0 {
            self.current_block = blocks.len();
            self.current_pos_in_block = end as usize;
            self.current_pos = self.end_pos + end as usize;
            return Some(self.current_pos);
        }

        let pos_delta = (-end) as usize;
        if self.end_pos < pos_delta {
            return None;
        }
        let new_pos = self.end_pos - pos_delta;
        if new_pos == 0 {
            self.current_block = 0;
            self.current_pos_in_block = 0;
            self.current_pos = 0;
            return Some(0);
        }

        let mut pos = 0;
        for (bi, block) in blocks.iter().enumerate().rev() {
            pos += block.len();
            if pos >= pos_delta {
                self.current_block = bi;
                self.current_pos_in_block = pos - pos_delta;
                self.current_pos = new_pos;
                return Some(new_pos);
            }
        }
        None
    }

    fn seek_from_current(&mut self, current: isize, blocks: &[Block]) -> Option<usize> {
        match current.cmp(&0) {
            cmp::Ordering::Equal => Some(self.current_pos),
            cmp::Ordering::Greater => self.advance(current as usize, blocks),
            cmp::Ordering::Less => {
                let pos_delta = (-current) as usize;
                if self.current_pos < pos_delta {
                    return None;
                }
                let new_pos = self.current_pos - pos_delta;
                if new_pos == 0 {
                    self.current_block = 0;
                    self.current_pos_in_block = 0;
                    self.current_pos = 0;
                    return Some(0);
                }

                if self.current_pos_in_block >= pos_delta {
                    self.current_pos_in_block -= pos_delta;
                    self.current_pos = new_pos;
                    return Some(new_pos);
                }

                let mut pos = self.current_pos_in_block;
                for (bi_delta, block) in blocks[..self.current_block].iter().rev().enumerate() {
                    pos += block.len();
                    if pos >= pos_delta {
                        self.current_block -= bi_delta + 1;
                        self.current_pos_in_block = pos - pos_delta;
                        self.current_pos = new_pos;
                        return Some(new_pos);
                    }
                }

                None
            }
        }
    }
}

/// A vector of blocks.
///
/// See the [module documentation](crate) for details.
#[derive(Debug)]
pub struct BlockedVec {
    blocks: Vec<Block>,
    layout: Layout,
    len: usize,
    seeker: Option<Seeker>,
}

impl BlockedVec {
    /// Creates a new [`BlockedVec`].
    ///
    /// # Panics
    ///
    /// Panics if the queried page size cannot be made into a layout.
    #[cfg(feature = "std")]
    pub fn new() -> Self {
        let ps = page_size::get();
        let layout = Layout::from_size_align(ps, ps).expect("Invalid layout");
        Self::new_paged(layout)
    }

    /// Creates a new [`BlockedVec`] with a given page layout.
    pub fn new_paged(page_layout: Layout) -> Self {
        BlockedVec {
            blocks: Vec::new(),
            layout: page_layout,
            len: 0,
            seeker: None,
        }
    }

    /// Creates a new [`BlockedVec`] with an initial length, with the cursor
    /// placed at the front.
    ///
    /// # Panics
    ///
    /// Panics if the queried page size cannot be made into a layout.
    #[cfg(feature = "std")]
    pub fn with_len(len: usize) -> Self {
        let ps = page_size::get();
        let layout = Layout::from_size_align(ps, ps).expect("Invalid layout");
        Self::with_len_paged(len, layout)
    }

    /// Creates a new [`BlockedVec`] with a given page layout and an initial
    /// length, with the cursor placed at the front.
    pub fn with_len_paged(len: usize, page_layout: Layout) -> Self {
        match Block::with_len(page_layout, len) {
            Some(block) => BlockedVec {
                blocks: vec![block],
                layout: page_layout,
                len,
                seeker: Some(Seeker::start(len)),
            },
            None => Self::new_paged(page_layout),
        }
    }

    /// Returns the length of this [`BlockedVec`].
    pub fn len(&self) -> usize {
        self.len
    }

    /// Checks if this [`BlockedVec`] is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn append_inner(&mut self, buf: &mut &mut [impl IoSlice]) -> usize {
        match &mut self.seeker {
            Some(seeker) => {
                let last = self.blocks.last_mut().expect("Inconsistent seeker");
                let skip = if seeker.is_at_end() {
                    let skip = mem::replace(&mut seeker.current_pos_in_block, 0);
                    seeker.current_pos -= skip;
                    skip
                } else {
                    0
                };
                match last.extend(skip, buf) {
                    Ok(len) => {
                        seeker.append_end(len);
                        self.len += len;
                        len - skip
                    }
                    Err(len) => {
                        seeker.append_end(len);
                        self.len += len;

                        let skip2 = if len > skip { 0 } else { skip - len };

                        if let Some((new, len2)) = Block::from_buf(self.layout, skip2, buf) {
                            seeker.append_new(&new);
                            self.len += new.len();
                            self.blocks.push(new);

                            len + len2 - skip + skip2
                        } else {
                            len - skip
                        }
                    }
                }
            }
            None => match Block::from_buf(self.layout, 0, buf) {
                Some((block, len)) => {
                    let mut seeker = Seeker::start(block.len());
                    seeker.current_block = 1;
                    seeker.current_pos = seeker.end_pos;
                    self.seeker = Some(seeker);
                    self.len = block.len();
                    self.blocks = vec![block];
                    len
                }
                None => 0,
            },
        }
    }

    /// Extend the `BlockedVec`, with the possible additional areas filled with
    /// zero.
    ///
    /// # Panics
    ///
    /// Panics if the seeker or blocks are inconsistent.
    pub fn extend(&mut self, additional: usize) {
        match &mut self.seeker {
            Some(seeker) => {
                let last = self.blocks.last_mut().expect("Inconsistent seeker");
                match last.extend(additional, &mut (&mut [] as &mut [&[u8]])) {
                    Ok(len) => {
                        seeker.extend_end(len, last);
                        self.len += len
                    }
                    Err(len) => {
                        seeker.extend_end(len, last);
                        self.len += len;

                        let len2 = additional - len;
                        let (new, _) =
                            Block::from_buf(self.layout, len2, &mut (&mut [] as &mut [&[u8]]))
                                .expect("Inconsistent blocks");
                        seeker.extend_new(len2);
                        self.len += len2;
                        self.blocks.push(new);
                    }
                }
            }
            None => *self = Self::with_len_paged(additional, self.layout),
        }
    }

    /// Append a bunch of buffers to the `BlockedVec`.
    ///
    /// # Returns
    ///
    /// The actual length of the buffers appended.
    ///
    /// # Panics
    ///
    /// Panics if the seeker is inconsistent.
    pub fn append_vectored(&mut self, mut buf: &mut [impl IoSlice]) -> usize {
        self.seek(SeekFrom::End(0)).expect("Inconsistent seeker");
        self.append_inner(&mut buf)
    }

    /// Append a buffer to the `BlockedVec`.
    ///
    /// # Returns
    ///
    /// The actual length of the buffer appended.
    pub fn append(&mut self, buf: &[u8]) -> usize {
        self.append_vectored(&mut [buf])
    }

    /// Like `read`, except that it reads into a slice of buffers.
    pub fn read_vectored(&mut self, buf: &mut [impl IoSliceMut]) -> usize {
        let seeker = match &mut self.seeker {
            Some(seeker) => seeker,
            None => return 0,
        };
        match seeker.read(buf, &self.blocks) {
            Ok(len) => len,
            Err(len) => len,
        }
    }

    /// Like `read_at`, except that it reads into a slice of buffers.
    pub fn read_at_vectored(&self, pos: usize, buf: &mut [impl IoSliceMut]) -> usize {
        let mut seeker = match self.seeker {
            Some(mut seeker) => match seeker.seek_from_start(pos, &self.blocks) {
                Some(_) => seeker,
                None => return 0,
            },
            None => return 0,
        };
        match seeker.read(buf, &self.blocks) {
            Ok(len) => len,
            Err(len) => len,
        }
    }

    /// Pull some bytes from this [`BlockedVec`] into the specified buffer,
    /// returning how many bytes were read.
    #[inline]
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        self.read_vectored(&mut [buf])
    }

    /// Pull some bytes from this [`BlockedVec`] into the specified buffer at a
    /// specified position, returning how many bytes were read.
    #[inline]
    pub fn read_at(&self, pos: usize, buf: &mut [u8]) -> usize {
        self.read_at_vectored(pos, &mut [buf])
    }

    /// Like [`write`], except that it writes from a slice of buffers.
    ///
    /// [`write`]: BlockedVec::write
    pub fn write_vectored(&mut self, mut buf: &mut [impl IoSlice]) -> usize {
        let seeker = match &mut self.seeker {
            Some(seeker) => seeker,
            None => return 0,
        };
        match seeker.write(&mut buf, &mut self.blocks) {
            Ok(len) => len,
            Err(len) => self.append_inner(&mut buf) + len,
        }
    }

    /// Like [`write_at`], except that it writes from a slice of buffers.
    ///
    /// [`write_at`]: BlockedVec::write_at
    pub fn write_at_vectored(&mut self, pos: usize, mut buf: &mut [impl IoSlice]) -> usize {
        let mut seeker = match self.seeker {
            Some(mut seeker) => match seeker.seek_from_start(pos, &self.blocks) {
                Some(_) => seeker,
                None => return 0,
            },
            None => return 0,
        };
        match seeker.write(&mut buf, &mut self.blocks) {
            Ok(len) => len,
            Err(len) => self.append_inner(&mut buf) + len,
        }
    }

    /// Write a buffer into this writer, returning how many bytes were written.
    #[inline]
    pub fn write(&mut self, buf: &[u8]) -> usize {
        self.write_vectored(&mut [buf])
    }

    /// Write a buffer into this writer at a specified position, returning how
    /// many bytes were written.
    #[inline]
    pub fn write_at(&mut self, pos: usize, buf: &[u8]) -> usize {
        self.write_at_vectored(pos, &mut [buf])
    }

    /// Seek to an offset, in bytes, in this [`BlockedVec`].
    pub fn seek(&mut self, pos: SeekFrom) -> Option<usize> {
        match &mut self.seeker {
            Some(seeker) => match pos {
                SeekFrom::Start(start) => seeker.seek_from_start(start as usize, &self.blocks),
                SeekFrom::End(end) => seeker.seek_from_end(end as isize, &self.blocks),
                SeekFrom::Current(current) => {
                    seeker.seek_from_current(current as isize, &self.blocks)
                }
            },
            _ if pos == SeekFrom::End(0) => Some(0),
            _ => None,
        }
    }

    /// Shortens this `BlockedVec` to the specified length.
    pub fn truncate(&mut self, len: usize) -> bool {
        match &mut self.seeker {
            Some(seeker) => match seeker.truncate(len, &self.blocks) {
                Some((bi, pos_in_block)) => {
                    let bi = match self.blocks[bi].truncate(pos_in_block) {
                        Some(true) => bi,
                        _ => bi + 1,
                    };
                    if bi < self.blocks.len() {
                        self.blocks.truncate(bi);
                    }
                    self.len = len;
                    true
                }
                None => {
                    self.len = len;
                    seeker.end_pos > len
                }
            },
            None => false,
        }
    }

    /// Resizes the `BlockedVec` in-place so that `len` is equal to `new_len`,
    /// with the possible additional area filled with zero.
    pub fn resize(&mut self, new_len: usize) {
        if self.len < new_len {
            let extra = new_len - self.len;
            self.extend(extra)
        } else {
            self.truncate(new_len);
        }
    }

    /// Returns the iterator of blocks (i.e. byte slices).
    #[inline]
    pub fn iter(&self) -> BlockIter<'_> {
        BlockIter {
            blocks: &self.blocks,
        }
    }

    /// Returns the mutable iterator of blocks (i.e. byte slices).
    #[inline]
    pub fn iter_mut(&mut self) -> BlockIterMut<'_> {
        BlockIterMut {
            blocks: self.blocks.iter_mut(),
        }
    }

    /// Returns every byte of this [`BlockedVec`].
    #[inline]
    pub fn bytes(&self) -> impl Iterator<Item = u8> + Clone + core::fmt::Debug + '_ {
        self.iter().flatten().copied()
    }

    /// Returns the iterator of blocks (i.e. byte slices) in a selected range.
    ///
    /// The possible blocks parts that are out of range are not returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::alloc::Layout;
    /// use blocked_vec::BlockedVec;
    ///
    /// let layout = Layout::new::<[u8; 4]>();
    /// // Allocated 4 pages at one time, thus continuous.
    /// let vec = BlockedVec::with_len_paged(16, layout);
    /// let mut range = vec.range(7..14);
    /// assert_eq!(range.next(), Some(&[0u8; 7] as &[u8]));
    /// assert_eq!(range.next(), None);
    ///
    /// // Allocated 4 pages independently, thus discrete.
    /// let mut vec = BlockedVec::with_len_paged(4, layout);
    /// vec.append(&[1; 4]);
    /// vec.append(&[2; 4]);
    /// vec.append(&[3; 4]);
    ///
    /// let mut range = vec.range(7..14);
    /// assert_eq!(range.next(), Some(&[1u8; 1] as &[u8]));
    /// assert_eq!(range.next(), Some(&[2u8; 4] as &[u8]));
    /// assert_eq!(range.next(), Some(&[3u8; 2] as &[u8]));
    /// assert_eq!(range.next(), None);
    /// ```
    pub fn range<R>(&self, range: R) -> RangeIter<'_>
    where
        R: RangeBounds<usize>,
    {
        let mut start = match self.seeker {
            Some(_) => Seeker::start(self.len),
            None => return RangeIter::end(),
        };
        match range.start_bound() {
            Bound::Included(&bound) => start.seek_bound(bound, &self.blocks),
            Bound::Excluded(&bound) => start.seek_bound(bound + 1, &self.blocks),
            Bound::Unbounded => {}
        }

        let mut end = start;
        match range.end_bound() {
            Bound::Included(&bound) => end.seek_bound(bound, &self.blocks),
            Bound::Excluded(&bound) => end.seek_bound(bound - 1, &self.blocks),
            Bound::Unbounded => {
                let _ = end.seek_from_end(-1, &self.blocks);
            }
        }

        RangeIter {
            start_block: start.current_block,
            start_offset: start.current_pos_in_block,
            end_block: end.current_block,
            end_offset: end.current_pos_in_block,
            blocks: &self.blocks[start.current_block..],
        }
    }

    /// Returns the mutable iterator of blocks (i.e. byte slices) in a selected
    /// range.
    ///
    /// The possible blocks parts that are out of range are not returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::alloc::Layout;
    /// use blocked_vec::BlockedVec;
    ///
    /// let layout = Layout::new::<[u8; 4]>();
    /// // Allocated 4 pages at one time, thus continuous.
    /// let mut vec = BlockedVec::with_len_paged(16, layout);
    /// let mut range = vec.range_mut(7..14);
    /// assert_eq!(range.next(), Some(&mut [0u8; 7] as &mut [u8]));
    /// assert_eq!(range.next(), None);
    ///
    /// // Allocated 4 pages independently, thus discrete.
    /// let mut vec = BlockedVec::with_len_paged(4, layout);
    /// vec.append(&[1; 4]);
    /// vec.append(&[2; 4]);
    /// vec.append(&[3; 4]);
    ///
    /// let mut range = vec.range_mut(7..14);
    /// assert_eq!(range.next(), Some(&mut [1u8; 1] as &mut [u8]));
    /// assert_eq!(range.next(), Some(&mut [2u8; 4] as &mut [u8]));
    /// assert_eq!(range.next(), Some(&mut [3u8; 2] as &mut [u8]));
    /// assert_eq!(range.next(), None);
    /// ```
    pub fn range_mut<R>(&mut self, range: R) -> RangeIterMut<'_>
    where
        R: RangeBounds<usize>,
    {
        let mut start = match self.seeker {
            Some(_) => Seeker::start(self.len),
            None => return RangeIterMut::end(),
        };
        match range.start_bound() {
            Bound::Included(&bound) => start.seek_bound(bound, &self.blocks),
            Bound::Excluded(&bound) => start.seek_bound(bound + 1, &self.blocks),
            Bound::Unbounded => {}
        }

        let mut end = start;
        match range.end_bound() {
            Bound::Included(&bound) => end.seek_bound(bound, &self.blocks),
            Bound::Excluded(&bound) => end.seek_bound(bound - 1, &self.blocks),
            Bound::Unbounded => {
                let _ = end.seek_from_end(-1, &self.blocks);
            }
        }
        RangeIterMut {
            start_block: start.current_block,
            start_offset: start.current_pos_in_block,
            end_block: end.current_block,
            end_offset: end.current_pos_in_block,
            blocks: self.blocks[start.current_block..].iter_mut(),
        }
    }
}

#[cfg(feature = "std")]
impl Default for BlockedVec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std")]
impl std::io::Seek for BlockedVec {
    #[inline]
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.seek(pos.into())
            .map(|pos| pos as u64)
            .ok_or_else(|| std::io::ErrorKind::InvalidInput.into())
    }
}

#[cfg(feature = "std")]
impl std::io::Read for BlockedVec {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(self.read(buf))
    }

    #[inline]
    fn read_vectored(&mut self, bufs: &mut [std::io::IoSliceMut<'_>]) -> std::io::Result<usize> {
        Ok(self.read_vectored(bufs))
    }
}

#[cfg(feature = "std")]
impl std::io::Write for BlockedVec {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(self.write(buf))
    }

    #[inline]
    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        Ok(self.write_vectored(&mut Vec::from(bufs)))
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct BlockIter<'a> {
    blocks: &'a [Block],
}

impl<'a> Iterator for BlockIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let (ret, next) = self.blocks.split_first()?;
        self.blocks = next;
        // SAFETY: bytes are always valid.
        Some(unsafe { MaybeUninit::slice_assume_init_ref(ret.as_slice()) })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.blocks.len(), Some(self.blocks.len()))
    }
}

impl ExactSizeIterator for BlockIter<'_> {}

impl FusedIterator for BlockIter<'_> {}

#[derive(Debug)]
#[repr(transparent)]
pub struct BlockIterMut<'a> {
    blocks: slice::IterMut<'a, Block>,
}

impl<'a> Iterator for BlockIterMut<'a> {
    type Item = &'a mut [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let ret = self.blocks.next()?;
        // SAFETY: bytes are always valid.
        Some(unsafe { MaybeUninit::slice_assume_init_mut(ret.as_mut_slice()) })
    }
}

impl ExactSizeIterator for BlockIterMut<'_> {}

impl FusedIterator for BlockIterMut<'_> {}

#[derive(Debug, Clone, Copy)]
pub struct RangeIter<'a> {
    start_block: usize,
    start_offset: usize,
    end_block: usize,
    end_offset: usize,
    blocks: &'a [Block],
}

impl<'a> RangeIter<'a> {
    #[inline]
    fn end() -> Self {
        RangeIter {
            start_block: 0,
            start_offset: 0,
            end_block: 0,
            end_offset: 0,
            blocks: &[],
        }
    }
}

impl<'a> Iterator for RangeIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.start_block > self.end_block
            || (self.start_block == self.end_block && self.start_offset >= self.end_offset)
        {
            return None;
        }

        let (block, next) = self.blocks.split_first()?;
        let ret = if self.start_block < self.end_block {
            &block.as_slice()[self.start_offset..]
        } else {
            &block.as_slice()[self.start_offset..=self.end_offset]
        };
        self.blocks = next;
        self.start_block += 1;
        self.start_offset = 0;

        // SAFETY: bytes are always valid.
        Some(unsafe { MaybeUninit::slice_assume_init_ref(ret) })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (
            (self.end_block - self.start_block).saturating_sub(1),
            Some(self.end_block - self.start_block),
        )
    }
}

impl FusedIterator for RangeIter<'_> {}

#[derive(Debug)]
pub struct RangeIterMut<'a> {
    start_block: usize,
    start_offset: usize,
    end_block: usize,
    end_offset: usize,
    blocks: slice::IterMut<'a, Block>,
}

impl<'a> RangeIterMut<'a> {
    #[inline]
    fn end() -> Self {
        RangeIterMut {
            start_block: 0,
            start_offset: 0,
            end_block: 0,
            end_offset: 0,
            blocks: [].iter_mut(),
        }
    }
}

impl<'a> Iterator for RangeIterMut<'a> {
    type Item = &'a mut [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.start_block > self.end_block
            || (self.start_block == self.end_block && self.start_offset >= self.end_offset)
        {
            return None;
        }

        let block = self.blocks.next()?;
        let ret = if self.start_block < self.end_block {
            &mut block.as_mut_slice()[self.start_offset..]
        } else {
            &mut block.as_mut_slice()[self.start_offset..=self.end_offset]
        };
        self.start_block += 1;
        self.start_offset = 0;

        // SAFETY: bytes are always valid.
        Some(unsafe { MaybeUninit::slice_assume_init_mut(ret) })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (
            (self.end_block - self.start_block).saturating_sub(1),
            Some(self.end_block - self.start_block),
        )
    }
}

impl FusedIterator for RangeIterMut<'_> {}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, Write};

    use super::*;

    fn test_inner() -> Option<()> {
        let layout = Layout::new::<[u8; 4]>();
        let mut vec = BlockedVec::new_paged(layout);
        vec.append(&[1, 2, 3, 4, 5]);
        vec.seek(SeekFrom::Start(3))?;
        vec.write_all(&[6, 7, 8, 9, 10]).unwrap();
        vec.seek(SeekFrom::End(-3))?;
        vec.write_all(&[11, 12, 13, 14, 15]).unwrap();
        vec.seek(SeekFrom::Current(-7))?;
        vec.seek(SeekFrom::Current(1))?;
        vec.write_all(&[16, 17, 18, 19, 20]).unwrap();
        vec.seek(SeekFrom::End(3))?;
        vec.write_all(&[21, 22, 23, 24, 25]).unwrap();
        vec.resize(6);
        vec.seek(SeekFrom::Current(-3))?;
        vec.resize(12);
        vec.append(&[26, 27, 28, 29, 30]);
        // std::eprintln!("{vec:?}");

        vec.rewind().unwrap();
        let mut buf = [0; 17];
        vec.read_exact(&mut buf).unwrap();
        assert_eq!(
            buf,
            [1, 2, 3, 6, 16, 17, 0, 0, 0, 0, 0, 0, 26, 27, 28, 29, 30]
        );
        Some(())
    }

    #[test]
    fn test() {
        test_inner().expect("Failed to seek")
    }
}
