//! Content-addressed blob store under `<data>/artifacts/`.
//!
//! A blob lives at `artifacts/sha256/<first two hex>/<sha256>`. Writes go
//! to `artifacts/tmp/` first, are hashed as they stream, and are renamed
//! into place only when complete, so a reader never sees a partial blob and
//! two writers of the same bytes end with one file. Owner: M6.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// A stored blob: its SHA-256 (lowercase hex) and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRef {
    pub sha256: String,
    pub size: u64,
}

/// Why a write was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobError {
    TooLarge { max: u64 },
    Mismatch { expected: String, found: String },
}

impl std::fmt::Display for BlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlobError::TooLarge { max } => write!(f, "blob is larger than {max} bytes"),
            BlobError::Mismatch { expected, found } => {
                write!(f, "sha256 mismatch: expected {expected}, got {found}")
            }
        }
    }
}

impl std::error::Error for BlobError {}

/// True for 64 lowercase hex digits.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug)]
pub struct Blobs {
    root: PathBuf,
}

impl Blobs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store of a data directory: `<data>/artifacts`.
    pub fn in_data_dir(data: &Path) -> Self {
        Self::new(data.join("artifacts"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the blob `sha256` lives. Errors for anything but a SHA-256.
    pub fn path(&self, sha256: &str) -> Result<PathBuf> {
        if !is_sha256(sha256) {
            bail!("not a sha256: {sha256:?}");
        }
        Ok(self.root.join("sha256").join(&sha256[..2]).join(sha256))
    }

    pub async fn exists(&self, sha256: &str) -> bool {
        match self.path(sha256) {
            Ok(p) => tokio::fs::metadata(p).await.is_ok(),
            Err(_) => false,
        }
    }

    /// Starts a streamed write of at most `max` bytes.
    pub async fn writer(&self, max: u64) -> Result<BlobWriter> {
        let tmp_dir = self.root.join("tmp");
        tokio::fs::create_dir_all(&tmp_dir).await?;
        let tmp = tmp_dir.join(format!("{}.part", uuid::Uuid::now_v7()));
        let file = tokio::fs::File::create(&tmp)
            .await
            .with_context(|| format!("create {}", tmp.display()))?;
        Ok(BlobWriter {
            blobs: self.clone(),
            file: Some(file),
            tmp,
            hasher: Sha256::new(),
            size: 0,
            max,
        })
    }

    /// Stores `bytes` and returns their reference.
    pub async fn put(&self, bytes: &[u8]) -> Result<BlobRef> {
        let mut w = self.writer(bytes.len() as u64).await?;
        w.write(bytes).await?;
        w.finish(None).await
    }

    /// Reads a whole blob into memory.
    pub async fn read(&self, sha256: &str) -> Result<Vec<u8>> {
        let p = self.path(sha256)?;
        tokio::fs::read(&p)
            .await
            .with_context(|| format!("read blob {sha256}"))
    }

    /// Opens a blob for streaming.
    pub async fn open(&self, sha256: &str) -> Result<tokio::fs::File> {
        let p = self.path(sha256)?;
        tokio::fs::File::open(&p)
            .await
            .with_context(|| format!("open blob {sha256}"))
    }

    /// Removes a blob. False when it was not there.
    pub async fn remove(&self, sha256: &str) -> Result<bool> {
        let p = self.path(sha256)?;
        match tokio::fs::remove_file(&p).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Removes partial writes older than `older_than` (left by a crash).
    pub async fn clean_tmp(&self, older_than: std::time::Duration) -> Result<usize> {
        let dir = self.root.join("tmp");
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.into()),
        };
        let mut n = 0;
        while let Some(ent) = rd.next_entry().await? {
            let old = ent
                .metadata()
                .await
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= older_than);
            if old && tokio::fs::remove_file(ent.path()).await.is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

/// A blob being written. Dropping it without `finish` leaves a temp file
/// that [`Blobs::clean_tmp`] removes; call [`BlobWriter::abort`] instead.
pub struct BlobWriter {
    blobs: Blobs,
    file: Option<tokio::fs::File>,
    tmp: PathBuf,
    hasher: Sha256,
    size: u64,
    max: u64,
}

impl BlobWriter {
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Appends a chunk. Errors with [`BlobError::TooLarge`] past the cap.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        if self.size + chunk.len() as u64 > self.max {
            return Err(BlobError::TooLarge { max: self.max }.into());
        }
        let f = self.file.as_mut().context("blob writer closed")?;
        f.write_all(chunk).await?;
        self.hasher.update(chunk);
        self.size += chunk.len() as u64;
        Ok(())
    }

    /// Completes the write. With `expect`, the content must hash to it
    /// (lowercase hex) or the write is discarded with [`BlobError::Mismatch`].
    pub async fn finish(mut self, expect: Option<&str>) -> Result<BlobRef> {
        let mut f = self.file.take().context("blob writer closed")?;
        f.flush().await?;
        f.sync_all().await?;
        drop(f);
        let sha = hex::encode(std::mem::take(&mut self.hasher).finalize());
        if let Some(want) = expect.filter(|w| !w.is_empty())
            && !want.eq_ignore_ascii_case(&sha)
        {
            let _ = tokio::fs::remove_file(&self.tmp).await;
            return Err(BlobError::Mismatch {
                expected: want.to_ascii_lowercase(),
                found: sha,
            }
            .into());
        }
        let dest = self.blobs.path(&sha)?;
        if let Some(dir) = dest.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        if tokio::fs::metadata(&dest).await.is_ok() {
            // Same bytes already stored.
            let _ = tokio::fs::remove_file(&self.tmp).await;
        } else {
            tokio::fs::rename(&self.tmp, &dest).await?;
        }
        Ok(BlobRef {
            sha256: sha,
            size: self.size,
        })
    }

    /// Discards the write.
    pub async fn abort(mut self) {
        self.file.take();
        let _ = tokio::fs::remove_file(&self.tmp).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_read_dedupe_remove() {
        let dir = tempfile::tempdir().unwrap();
        let b = Blobs::in_data_dir(dir.path());
        let r = b.put(b"hello").await.unwrap();
        assert_eq!(
            r.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(r.size, 5);
        assert!(b.exists(&r.sha256).await);
        assert_eq!(b.read(&r.sha256).await.unwrap(), b"hello");
        assert_eq!(b.put(b"hello").await.unwrap(), r);
        assert!(b.remove(&r.sha256).await.unwrap());
        assert!(!b.remove(&r.sha256).await.unwrap());
        assert!(!b.exists(&r.sha256).await);
    }

    #[tokio::test]
    async fn streamed_write_caps_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let b = Blobs::in_data_dir(dir.path());
        let mut w = b.writer(8).await.unwrap();
        w.write(b"1234").await.unwrap();
        let e = w.write(b"56789").await.unwrap_err();
        assert_eq!(
            e.downcast_ref::<BlobError>(),
            Some(&BlobError::TooLarge { max: 8 })
        );
        w.abort().await;

        let mut w = b.writer(100).await.unwrap();
        w.write(b"abc").await.unwrap();
        let e = w.finish(Some(&"0".repeat(64))).await.unwrap_err();
        assert!(matches!(
            e.downcast_ref::<BlobError>(),
            Some(BlobError::Mismatch { .. })
        ));
        let mut w = b.writer(100).await.unwrap();
        w.write(b"abc").await.unwrap();
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(w.finish(Some(sha)).await.unwrap().sha256, sha);
        // No partial files are left behind.
        let mut rd = tokio::fs::read_dir(b.root().join("tmp")).await.unwrap();
        assert!(rd.next_entry().await.unwrap().is_none());
        assert_eq!(b.clean_tmp(std::time::Duration::ZERO).await.unwrap(), 0);
    }

    #[test]
    fn paths_refuse_non_hashes() {
        let b = Blobs::new("/x");
        assert!(b.path("../../etc/passwd").is_err());
        assert!(b.path(&"A".repeat(64)).is_err());
        let p = b.path(&"ab".repeat(32)).unwrap();
        assert!(p.ends_with(format!("sha256/ab/{}", "ab".repeat(32))));
    }
}
