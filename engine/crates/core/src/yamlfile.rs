//! Configuration lives in YAML files: the screen, the API and agents all edit the same file.

use std::path::Path;

use serde::{de::DeserializeOwned, Serialize};

use crate::ApiError;

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Result<T, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_yaml::from_str(&s).map_err(|e| ApiError::bad(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(ApiError::internal(e.to_string())),
    }
}

pub fn save<T: Serialize>(path: &Path, v: &T) -> Result<(), ApiError> {
    let s = serde_yaml::to_string(v).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| ApiError::internal(e.to_string()))
}
