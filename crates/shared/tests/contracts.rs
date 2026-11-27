use serde_json::json;
use shared::*;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn fixture() -> Result<(Project, Track), serde_yaml::Error> {
    Ok((
        serde_yaml::from_str(include_str!("../../../examples/demo/project.yaml"))?,
        serde_yaml::from_str(include_str!("../../../examples/demo/tracks/lead.yaml"))?,
    ))
}
#[test]
fn demo_yaml_round_trip() -> TestResult {
    let (p, t) = fixture()?;
    p.validate()?;
    t.validate(&p.tracks[0])?;
    assert_eq!(p, serde_yaml::from_str(&serde_yaml::to_string(&p)?)?);
    assert_eq!(t, serde_yaml::from_str(&serde_yaml::to_string(&t)?)?);
    Ok(())
}
#[test]
fn invalid_project_and_track_rejected() -> TestResult {
    let (mut p, mut t) = fixture()?;
    p.tracks.push("../lead".into());
    assert!(p.validate().is_err());
    p.tracks.pop();
    p.bpm = 0;
    assert!(p.validate().is_err());
    assert!(t.validate("other").is_err());
    t.patterns[0].notes[0].pitch = 128;
    assert!(t.validate("lead").is_err());
    Ok(())
}
#[test]
fn rpc_wire_contract() -> TestResult {
    for method in ["Ping", "LoadProject", "Play", "Stop", "GetState"] {
        let mut wire = json!({"jsonrpc":"2.0","id":42,"method":method});
        if method == "LoadProject" {
            wire["params"] = json!({"path":"demo/project.yaml"});
        }
        let req: Request = serde_json::from_value(wire.clone())?;
        assert_eq!(serde_json::to_value(req)?, wire);
    }
    let state = json!({"title":"First Light","bpm":120,"playing":true});
    for wire in [
        json!({"jsonrpc":"2.0","id":1,"result":"pong"}),
        json!({"jsonrpc":"2.0","id":2,"result":{"state":state}}),
        json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"invalid JSON"}}),
    ] {
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<Response>(wire.clone())?)?,
            wire
        );
    }
    for wire in [
        json!({"jsonrpc":"2.0","method":"StateChanged","params":state}),
        json!({"jsonrpc":"2.0","method":"ValidationError","params":{"message":"invalid YAML"}}),
    ] {
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<Notification>(wire.clone())?)?,
            wire
        );
    }
    Ok(())
}
