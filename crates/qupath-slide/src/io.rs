//! Positioned reads, so that tiles can be fetched from many threads at once.
//!
//! A slide is far larger than RAM, so tiles are read from the file on demand
//! rather than materialised (§6). `Seek` plus `Read` would make the file handle
//! shared mutable state and force a lock around every tile fetch; a positioned
//! read takes `&File` and carries its own offset, so the tile loop needs no
//! synchronisation at all.

use std::fs::File;
use std::path::Path;

use crate::error::{Result, SlideError};

/// Fills `buffer` from `offset`, or fails naming the file.
pub fn read_exact_at(file: &File, path: &Path, buffer: &mut [u8], offset: u64) -> Result<()> {
    positioned(file, buffer, offset).map_err(|source| SlideError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn positioned(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(windows)]
fn positioned(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    let mut written = 0;
    while written < buffer.len() {
        let read = std::os::windows::fs::FileExt::seek_read(
            file,
            &mut buffer[written..],
            offset + written as u64,
        )?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        written += read;
    }
    Ok(())
}
