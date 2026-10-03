//! Private TypeSafe credentials for the core Jev evaluation service.
use std::fs::File;
use std::os::unix::fs::PermissionsExt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::Settings;
use crate::proxy::{self, KeyTarget};

#[derive(Deserialize, Serialize)]
struct Credential {
    api_key: String,
}

/// Prefer the new native credential; keep existing local proxy keys working without Docker.
pub fn key(settings: &Settings) -> Result<Option<String>> {
    let path = settings.data_dir.join("auth/typesafe.json");
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice::<Credential>(&bytes)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "TypeSafe credentials could not be read; replace the key in Setup"
                    )
                })?
                .api_key,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(proxy::legacy_typesafe_key(settings))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn has_key(settings: &Settings) -> bool {
    key(settings).is_ok_and(|k| k.is_some())
}

pub fn save_key(settings: &Settings, key: &str) -> Result<()> {
    KeyTarget::TypeSafe.check(key).map_err(anyhow::Error::msg)?;
    let directory = settings.data_dir.join("auth");
    std::fs::create_dir_all(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let mut temp = tempfile::NamedTempFile::new_in(&directory)?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(
        temp.as_file_mut(),
        &Credential {
            api_key: key.into(),
        },
    )?;
    temp.as_file().sync_all()?;
    temp.persist(directory.join("typesafe.json"))
        .map_err(|e| e.error)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_key_is_private_and_overrides_legacy_storage_without_a_gateway() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings {
            data_dir: temp.path().into(),
            ..Settings::load().unwrap()
        };
        std::fs::create_dir_all(proxy::dir(&settings)).unwrap();
        std::fs::write(
            proxy::dir(&settings).join(".env"),
            "TYPESAFE_API_KEY=legacy-test-key-with-sufficient-length\n",
        )
        .unwrap();
        assert_eq!(
            key(&settings).unwrap().as_deref(),
            Some("legacy-test-key-with-sufficient-length")
        );
        save_key(&settings, "native-test-key-with-sufficient-length").unwrap();
        assert_eq!(
            key(&settings).unwrap().as_deref(),
            Some("native-test-key-with-sufficient-length")
        );
        assert_eq!(
            std::fs::metadata(settings.data_dir.join("auth/typesafe.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(!settings.data_dir.join("llm.json").exists());
    }
}
