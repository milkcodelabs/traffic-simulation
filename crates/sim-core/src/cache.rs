use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use crate::graph::Network;
use crate::CoreError;

pub const CACHE_MAGIC: &[u8; 4] = b"TSIM";
pub const FORMAT_VERSION: u32 = 1;

pub fn store_network(path: &Path, network: &Network) -> Result<(), CoreError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|source| CoreError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
    }
    let payload = bincode::serialize(network).map_err(|error| CoreError::CachePayload(error.to_string()))?;
    let mut bytes = Vec::with_capacity(8 + payload.len());
    bytes.extend_from_slice(CACHE_MAGIC);
    bytes.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&payload);
    let mut file = File::create(path).map_err(|source| CoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    file.write_all(&bytes).map_err(|source| CoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(())
}

pub fn load_network(path: &Path) -> Result<Network, CoreError> {
    let mut file = File::open(path).map_err(|source| CoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|source| CoreError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if bytes.len() < 8 || &bytes[0..4] != CACHE_MAGIC {
        return Err(CoreError::CacheMagic);
    }
    let version_bytes: [u8; 4] = bytes[4..8].try_into().map_err(|_| CoreError::CacheMagic)?;
    let version = u32::from_le_bytes(version_bytes);
    if version != FORMAT_VERSION {
        return Err(CoreError::CacheVersion {
            found: version,
            expected: FORMAT_VERSION,
        });
    }
    let network: Network = bincode::deserialize(&bytes[8..]).map_err(|error| CoreError::CachePayload(error.to_string()))?;
    network.validate()?;
    Ok(network)
}
