use core::{alloc::Layout, cmp, mem};

use alloc::{vec, vec::Vec};

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
    fn with_len(len: usize) -> Seeker {
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
            self.current_pos_in_block = block.len();
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

#[derive(Debug)]
pub struct BlockedVec {
    blocks: Vec<Block>,
    layout: Layout,
    len: usize,
    seeker: Option<Seeker>,
}

impl BlockedVec {
    #[cfg(feature = "std")]
    pub fn new() -> Self {
        let ps = page_size::get();
        let layout = Layout::from_size_align(ps, ps).expect("Invalid layout");
        Self::new_paged(layout)
    }

    pub fn new_paged(page_layout: Layout) -> Self {
        BlockedVec {
            blocks: Vec::new(),
            layout: page_layout,
            len: 0,
            seeker: None,
        }
    }

    #[cfg(feature = "std")]
    pub fn with_len(len: usize) -> Self {
        let ps = page_size::get();
        let layout = Layout::from_size_align(ps, ps).expect("Invalid layout");
        Self::with_len_paged(len, layout)
    }

    pub fn with_len_paged(len: usize, page_layout: Layout) -> Self {
        match Block::with_len(page_layout, len) {
            Some(block) => BlockedVec {
                blocks: vec![block],
                layout: page_layout,
                len: 0,
                seeker: Some(Seeker::with_len(len)),
            },
            None => Self::new_paged(page_layout),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

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
                    let mut seeker = Seeker::with_len(block.len());
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

    pub fn append_vectored(&mut self, mut buf: &mut [impl IoSlice]) -> Option<usize> {
        self.seek(SeekFrom::End(0))?;
        Some(self.append_inner(&mut buf))
    }

    pub fn append(&mut self, buf: &[u8]) -> Option<usize> {
        self.append_vectored(&mut [buf])
    }

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

    pub fn read_at_vectored(&mut self, pos: usize, buf: &mut [impl IoSliceMut]) -> usize {
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

    #[inline]
    pub fn read(&mut self, buf: &mut [u8]) -> usize {
        self.read_vectored(&mut [buf])
    }

    #[inline]
    pub fn read_at(&mut self, pos: usize, buf: &mut [u8]) -> usize {
        self.read_at_vectored(pos, &mut [buf])
    }

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

    #[inline]
    pub fn write(&mut self, buf: &[u8]) -> usize {
        self.write_vectored(&mut [buf])
    }

    #[inline]
    pub fn write_at(&mut self, pos: usize, buf: &[u8]) -> usize {
        self.write_at_vectored(pos, &mut [buf])
    }

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

    pub fn truncate(&mut self, pos: usize) -> bool {
        match &mut self.seeker {
            Some(seeker) => match seeker.truncate(pos, &self.blocks) {
                Some((bi, pos_in_block)) => {
                    let bi = match self.blocks[bi].truncate(pos_in_block) {
                        Some(true) => bi,
                        _ => bi + 1,
                    };
                    if bi < self.blocks.len() {
                        self.blocks.truncate(bi);
                    }
                    self.len = pos;
                    true
                }
                None => {
                    self.len = pos;
                    seeker.end_pos > pos
                }
            },
            None => false,
        }
    }

    pub fn resize(&mut self, len: usize) {
        if self.len < len {
            let extra = len - self.len;
            self.extend(extra)
        } else {
            self.truncate(len);
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, Write};

    use super::*;

    fn test_inner() -> Option<()> {
        let layout = Layout::new::<[u8; 4]>();
        let mut vec = BlockedVec::new_paged(layout);
        vec.append(&[1, 2, 3, 4, 5])?;
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
        vec.append(&[26, 27, 28, 29, 30])?;

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
