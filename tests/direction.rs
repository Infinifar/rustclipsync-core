use rustclipsync_core::{direction::*, state, Error};

#[test]
fn direction_wire_matches_server_contract_and_rejects_unknown_enum_and_invalid_versions() {
    let frame:DirectionFrame=serde_json::from_str(r#"{"type":"SyncDirectionUpdate","sessionId":"00000000-0000-4000-8000-000000000001","version":1,"direction":"RECEIVE_ONLY","paused":false,"connect":true}"#).unwrap();
    let DirectionFrame::SyncDirectionUpdate(update) = frame else {
        panic!("wrong message type")
    };
    assert_eq!(update.direction, SyncDirection::ReceiveOnly);
    update.validate().unwrap();
    for direction in ["PUSH_ONLY", "RECEIVE_ONLY", "BIDIRECTIONAL"] {
        assert!(serde_json::from_str::<SyncDirection>(&format!("\"{direction}\"")).is_ok());
    }
    assert!(serde_json::from_str::<SyncDirection>("\"DISABLED\"").is_err());
    for version in [0, MAX_DIRECTION_VERSION + 1] {
        let v = SyncDirectionUpdate {
            session_id: state::random_uuid().unwrap(),
            version,
            direction: SyncDirection::Bidirectional,
            paused: false,
            connect: true,
        };
        assert!(matches!(v.validate(), Err(Error::InvalidProfile)));
    }
    let v = SyncDirectionRevoke {
        session_id: "invalid".into(),
        version: 1,
    };
    assert!(v.validate().is_err());
}

#[test]
fn server_ack_and_error_examples_deserialize_without_client_generated_expectations() {
    let frame:DirectionFrame=serde_json::from_str(r#"{"type":"SyncDirectionAck","deviceId":"00000000-0000-4000-8000-000000000001","sessionId":"00000000-0000-4000-8000-000000000002","version":7,"serverTime":1793548800,"expiresAt":1796140800,"revoked":false}"#).unwrap();
    let DirectionFrame::SyncDirectionAck(ack) = frame else {
        panic!("wrong ACK type")
    };
    assert_eq!(ack.version, 7);
    assert!(!ack.revoked);
    let error: SyncDirectionError =
        serde_json::from_str(r#"{"code":"PROTOCOL_UNSUPPORTED","minimumProtocolVersion":3}"#)
            .unwrap();
    assert_eq!(error.code, DirectionErrorCode::ProtocolUnsupported);
    assert_eq!(error.minimum_protocol_version, Some(3));
    let error: DirectionFrame =
        serde_json::from_str(r#"{"type":"SyncDirectionError","code":"VERSION_STALE","version":8}"#)
            .unwrap();
    assert!(matches!(
        error,
        DirectionFrame::SyncDirectionError(SyncDirectionError {
            code: DirectionErrorCode::VersionStale,
            version: Some(8),
            ..
        })
    ));
}
