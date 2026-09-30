//! The uploaded file, stored in `import_job_files` in 1 MiB chunks (D6).
//!
//! Writing: the upload inserts each complete chunk with one short statement,
//! so it never holds a connection while it waits for the client (CR7).
//! Reading: [`DbFile`] opens a `Read + Seek` view that fetches chunks on
//! demand and keeps the last [`CACHED_CHUNKS`] of them (T6). An XLSX has its
//! central directory at the end, so it cannot be read front to back. The view
//! runs on a blocking thread and fetches through the runtime handle.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use super::analyse::Source;
use super::parse::ParseError;

/// Bytes per stored chunk; every chunk but the last has exactly this many.
pub const CHUNK: usize = 1024 * 1024;
pub const CACHED_CHUNKS: usize = 8;

/// Inserts chunk `seq` of a job's file.
pub async fn insert_chunk(pool: &PgPool, job: Uuid, seq: i32, data: &[u8]) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO cmdb.import_job_files (job_id, seq, data) VALUES ($1, $2, $3)")
        .bind(job)
        .bind(seq)
        .bind(data)
        .execute(pool)
        .await
        .map(drop)
}

/// A stored file, opened afresh for each pass.
#[derive(Clone)]
pub struct DbFile {
    pub pool: PgPool,
    pub job: Uuid,
    pub len: u64,
    pub runtime: tokio::runtime::Handle,
}

impl Source for DbFile {
    type Reader = DbReader;
    fn open(&self) -> Result<DbReader, ParseError> {
        Ok(DbReader { file: self.clone(), pos: 0, cache: VecDeque::with_capacity(CACHED_CHUNKS) })
    }
}

pub struct DbReader {
    file: DbFile,
    pos: u64,
    /// Most recently used last.
    cache: VecDeque<(u64, Arc<Vec<u8>>)>,
}

impl DbReader {
    fn chunk(&mut self, seq: u64) -> std::io::Result<Arc<Vec<u8>>> {
        if let Some(i) = self.cache.iter().position(|(s, _)| *s == seq) {
            let hit = self.cache.remove(i).expect("position is in range");
            let data = hit.1.clone();
            self.cache.push_back(hit);
            return Ok(data);
        }
        let (pool, job) = (self.file.pool.clone(), self.file.job);
        let seq_i32 = i32::try_from(seq).map_err(|_| std::io::Error::other("chunk number out of range"))?;
        let data: Option<Vec<u8>> = self
            .file
            .runtime
            .block_on(async move {
                sqlx::query_scalar("SELECT data FROM cmdb.import_job_files WHERE job_id = $1 AND seq = $2")
                    .bind(job)
                    .bind(seq_i32)
                    .fetch_optional(&pool)
                    .await
            })
            .map_err(std::io::Error::other)?;
        let data = Arc::new(data.ok_or_else(|| std::io::Error::other("the stored file is incomplete"))?);
        if self.cache.len() == CACHED_CHUNKS {
            self.cache.pop_front();
        }
        self.cache.push_back((seq, data.clone()));
        Ok(data)
    }
}

impl Read for DbReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.file.len || buf.is_empty() {
            return Ok(0);
        }
        let seq = self.pos / CHUNK as u64;
        let offset = (self.pos % CHUNK as u64) as usize;
        let data = self.chunk(seq)?;
        if offset >= data.len() {
            return Err(std::io::Error::other("the stored file is shorter than recorded"));
        }
        let n = (data.len() - offset).min(buf.len()).min((self.file.len - self.pos) as usize);
        buf[..n].copy_from_slice(&data[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for DbReader {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.file.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match target {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before the start of the file")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view reads across chunk boundaries, seeks from the end, and keeps
    /// only the last chunks it used.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_stored_file_reads_like_the_original() {
        let Some(db) = crate::db::scratch::database("import_storage_reads_like_the_original").await else { return };
        let pool = db.pool.clone();
        let job: Uuid = sqlx::query_scalar(
            "INSERT INTO cmdb.import_jobs (created_by_name, status, file_name, file_format, file_size, expires_at)
             VALUES ('t', 'uploading', 'f.csv', 'csv', 0, now() + interval '1 day') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let original: Vec<u8> = (0..(CHUNK * 3 + 123)).map(|i| (i % 251) as u8).collect();
        for (seq, part) in original.chunks(CHUNK).enumerate() {
            insert_chunk(&pool, job, seq as i32, part).await.unwrap();
        }
        let file =
            DbFile { pool: pool.clone(), job, len: original.len() as u64, runtime: tokio::runtime::Handle::current() };
        let expected = original.clone();
        tokio::task::spawn_blocking(move || {
            let mut r = file.open().unwrap();
            let mut all = Vec::new();
            r.read_to_end(&mut all).unwrap();
            assert_eq!(all, expected);
            r.seek(SeekFrom::End(-10)).unwrap();
            let mut tail = [0u8; 10];
            r.read_exact(&mut tail).unwrap();
            assert_eq!(&tail[..], &expected[expected.len() - 10..]);
            r.seek(SeekFrom::Start(CHUNK as u64 - 2)).unwrap();
            let mut across = [0u8; 4];
            r.read_exact(&mut across).unwrap();
            assert_eq!(&across[..], &expected[CHUNK - 2..CHUNK + 2]);
            assert!(r.cache.len() <= CACHED_CHUNKS);
            assert!(r.seek(SeekFrom::Current(-(CHUNK as i64) * 10)).is_err());
        })
        .await
        .unwrap();
        db.drop().await;
    }
}
