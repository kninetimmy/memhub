pub mod v1;
pub mod v2;

pub use v2::{EXPORT_VERSION, Export};

pub(crate) fn decode(raw: &str) -> crate::Result<Export> {
    #[derive(serde::Deserialize)]
    struct Header {
        memhub_export_version: u32,
    }

    let header: Header = serde_json::from_str(raw)?;
    match header.memhub_export_version {
        v1::EXPORT_VERSION => Ok(serde_json::from_str::<v1::Export>(raw)?.into()),
        v2::EXPORT_VERSION => Ok(serde_json::from_str(raw)?),
        version => Err(crate::MemhubError::InvalidInput(format!(
            "unsupported memhub_export_version: {version} (this build supports {} and {})",
            v1::EXPORT_VERSION,
            v2::EXPORT_VERSION,
        ))),
    }
}
