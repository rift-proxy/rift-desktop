use rift_core::{
    diagnostics::{Diagnostic, ParameterKind},
    model::Filters,
    subscription::{apply_filters, parse_subscription},
};
use serde_json::json;
const LINK: &str = "vless://11111111-1111-4111-8111-111111111111@hidden-canary.example:443?security=tls&type=xhttp&mode=packet-up&extra=%7B%22noSSEHeader%22%3Atrue%7D";

#[test]
fn successful_keys_are_separate_from_rejected_presence_and_values_never_escape() {
    let parsed =
        parse_subscription(&format!("{LINK}\n{LINK}&fm=secret-canary"), "synthetic").unwrap();
    assert_eq!((parsed.rows.accepted, parsed.rows.rejected()), (1, 1));
    assert_eq!(
        parsed.rows.accepted_parameters[ParameterKind::Mode as usize],
        1
    );
    assert_eq!(
        parsed.rows.accepted_parameters[ParameterKind::Extra as usize],
        1
    );
    assert_eq!(
        parsed.rows.accepted_parameters[ParameterKind::Fm as usize],
        0
    );
    assert_eq!(
        parsed.rows.rejected_parameters[ParameterKind::Fm as usize],
        1
    );
    let error = apply_filters(
        parsed,
        &Filters {
            include: vec!["never-a-matching-name".into()],
            ..Default::default()
        },
    )
    .err()
    .unwrap();
    let report = Diagnostic::from_error(&error).to_json();
    assert_eq!(
        report["subscription"]["accepted_parameters"],
        json!({"mode":1,"extra":1})
    );
    assert_eq!(
        report["subscription"]["ignored_parameters"],
        json!({"fm":0,"mode":0,"extra":0})
    );
    let wire = report.to_string();
    for value in [
        "hidden-canary",
        "secret-canary",
        "packet-up",
        "noSSEHeader",
        "11111111",
    ] {
        assert!(!wire.contains(value));
    }
}

#[test]
fn xray_json_counts_successful_unique_outbounds_not_profile_copies() {
    let outbound = json!({"protocol":"vless","tag":"one","settings":{"vnext":[{"address":"hidden-canary.example","port":443,"users":[{"id":"11111111-1111-4111-8111-111111111111","encryption":"none"}]}]},"streamSettings":{"network":"xhttp","security":"tls","tlsSettings":{"serverName":"hidden-canary.example"},"xhttpSettings":{"mode":"packet-up","extra":{"noSSEHeader":true}}}});
    let parsed = parse_subscription(
        &json!([{"outbounds":[outbound]},{"outbounds":[outbound]}]).to_string(),
        "synthetic",
    )
    .unwrap();
    assert_eq!(parsed.rows.accepted, 1);
    assert_eq!(parsed.rows.accepted_parameters, [0, 1, 1]);
}

#[test]
fn grpc_json_multimode_is_the_mode_parameter_for_both_success_and_refusal() {
    let mut body = json!({"outbounds":[{"protocol":"vless","settings":{"vnext":[{"address":"synthetic.example","port":443,"users":[{"id":"11111111-1111-4111-8111-111111111111","encryption":"none"}]}]},"streamSettings":{"network":"grpc","grpcSettings":{"serviceName":"synthetic","multiMode":false}}}]});
    let parsed = parse_subscription(&body.to_string(), "synthetic").unwrap();
    assert_eq!(
        parsed.rows.accepted_parameters[ParameterKind::Mode as usize],
        1
    );
    body["outbounds"][0]["streamSettings"]["grpcSettings"]["multiMode"] = json!(true);
    let error = parse_subscription(&body.to_string(), "synthetic")
        .err()
        .unwrap();
    assert_eq!(
        error
            .detail()
            .unwrap()
            .subscription
            .unwrap()
            .rows
            .unwrap()
            .rejected_parameters[ParameterKind::Mode as usize],
        1
    );
}
