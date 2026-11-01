use rustclipsync_core::{adapters::ClipboardError, session::SyncError, Error};
use std::{error::Error as StdError, io};

#[test]
fn standard_error_conversion_preserves_remote_io_chain() {
    fn operation() -> Result<(), Box<dyn StdError + Send + Sync>> {
        Err(SyncError::Remote(Error::Io(io::Error::other(
            "fixture cause",
        ))))?;
        Ok(())
    }
    let error = operation().unwrap_err();
    assert!(error.downcast_ref::<SyncError>().is_some());
    let remote = error.source().unwrap();
    assert!(remote.downcast_ref::<Error>().is_some());
    let cause = remote
        .source()
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();
    assert_eq!(cause.kind(), io::ErrorKind::Other);
    assert_eq!(cause.to_string(), "fixture cause");
    assert!(cause.source().is_none());
}

#[test]
fn clipboard_and_storage_errors_preserve_causes_without_displaying_private_details() {
    let errors = [
        SyncError::Clipboard(ClipboardError::Io(io::Error::other("private-fixture-text"))),
        SyncError::Storage(io::Error::other("private-fixture-path")),
    ];
    for error in errors {
        assert!(!error.to_string().contains("private-fixture"));
        let mut current: &dyn StdError = &error;
        while let Some(source) = current.source() {
            current = source;
        }
        assert!(current.downcast_ref::<io::Error>().is_some());
    }
    assert!(ClipboardError::Timeout.source().is_none());
    assert!(SyncError::Conflict.source().is_none());
    assert!(Error::Status(503).source().is_none());
    assert!(Error::Status(503).to_string().contains("503"));
}

#[test]
fn json_and_http_causes_remain_accessible_without_displaying_input_or_urls() {
    let json = serde_json::from_str::<bool>("private-fixture-json").unwrap_err();
    let error = Error::Json(json);
    assert!(!error.to_string().contains("private-fixture"));
    assert!(error
        .source()
        .unwrap()
        .downcast_ref::<serde_json::Error>()
        .is_some());

    let http = reqwest::blocking::Client::new()
        .get("private-fixture-url")
        .build()
        .unwrap_err();
    let error = Error::Http(http);
    assert!(!error.to_string().contains("private-fixture"));
    assert!(error
        .source()
        .unwrap()
        .downcast_ref::<reqwest::Error>()
        .is_some());
}
