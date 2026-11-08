//! Small helpers for the JSON files Bean keeps on disk.

use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads `path` as JSON. A missing file is `Ok(None)`; a file that isn't
/// valid JSON for `T` is an [`io::ErrorKind::InvalidData`] error.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Writes `value` to `path` as JSON. Writes to a temporary file and renames
/// it into place, so a crash never leaves a half-written file behind.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(value)?)?;
    fs::rename(tmp, path)
}

/// Deletes `path`, treating an already-missing file as success.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bean-storage-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_and_leaves_no_temp_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("data.json");
        write_json(&path, &vec![1, 2, 3]).unwrap();
        assert_eq!(read_json::<Vec<i32>>(&path).unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn missing_file_is_none() {
        let path = temp_dir("missing").join("nope.json");
        assert_eq!(read_json::<Vec<i32>>(&path).unwrap(), None);
    }

    #[test]
    fn corrupt_file_is_invalid_data() {
        let path = temp_dir("corrupt").join("bad.json");
        fs::write(&path, "{ not json").unwrap();
        let error = read_json::<Vec<i32>>(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn removing_a_missing_file_succeeds() {
        let path = temp_dir("remove").join("gone.json");
        remove(&path).unwrap();
        fs::write(&path, "[]").unwrap();
        remove(&path).unwrap();
        assert!(!path.exists());
    }
}
