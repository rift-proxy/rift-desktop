//! The part of asking Windows for rights that needs no Windows: the meaning of the answer of the
//! shell, the command line of the helper and what the helper accepts on it.
use rift_core::elevation::{
    client::Launched,
    uac::{
        helper_arguments, launched_from_shell, parse_helper_args, pipe_name_valid, token_valid,
        HelperArgs, HELPER_ARG,
    },
};

const PIPE: &str = "rift-helper-0123456789abcdef0123456789abcdef";
const TOKEN: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn the_shell_answer_tells_started_declined_and_failed() {
    assert_eq!(launched_from_shell(Ok(())), Launched::Started);
    // Win32 ERROR_CANCELLED: the user answered the prompt with "No".
    assert_eq!(launched_from_shell(Err(Some(1223))), Launched::Declined);
    // Every other error, and an error without a number, is a failure, not a refusal.
    for other in [
        Some(0),
        Some(2),
        Some(5),
        Some(740),
        Some(1),
        Some(-1),
        None,
    ] {
        assert_eq!(
            launched_from_shell(Err(other)),
            Launched::Failed,
            "{other:?}"
        );
    }
}

#[test]
fn the_helper_gets_the_mark_the_channel_and_the_token_and_nothing_else() {
    let line = helper_arguments(PIPE, TOKEN).unwrap();
    assert_eq!(line, format!("{HELPER_ARG} {PIPE} {TOKEN}"));
    // What the helper reads back from that line is the same pair.
    let parts: Vec<String> = line.split(' ').skip(1).map(str::to_owned).collect();
    assert_eq!(
        parse_helper_args(&parts),
        Some(HelperArgs {
            pipe: PIPE.into(),
            token: TOKEN.into()
        })
    );
}

#[test]
fn nothing_that_needs_quoting_reaches_the_command_line_of_the_helper() {
    for bad_pipe in [
        "",
        "rift-helper-short",
        r#"rift-helper-0123456789abcdef0123456789abcde""#,
        "rift-helper-0123456789abcdef0123456789abcde ",
        r"\\.\pipe\rift-helper-0123456789abcdef0123456789abcdef",
        "rift-helper-0123456789abcdef0123456789abcdef --config x",
    ] {
        assert_eq!(helper_arguments(bad_pipe, TOKEN), None, "{bad_pipe:?}");
    }
    for bad_token in [
        "",
        "short",
        "FEDCBA9876543210FEDCBA9876543210",
        "x y",
        "a\"b",
    ] {
        assert_eq!(helper_arguments(PIPE, bad_token), None, "{bad_token:?}");
    }
}

#[test]
fn a_channel_name_is_rifts_own_and_local() {
    assert!(pipe_name_valid(PIPE));
    for bad in [
        "",
        "rift-helper-",
        "rift-helper-0123",
        "rift-helper-0123456789ABCDEF0123456789ABCDEF",
        "rift-helper-0123456789abcdef0123456789abcdeg",
        "rift-helper-0123456789abcdef0123456789abcdef0",
        r"..\rift-helper-0123456789abcdef0123456789abcdef",
        r"\\evil\pipe\rift-helper-0123456789abcdef0123456789abcdef",
        r"\\.\pipe\rift-helper-0123456789abcdef0123456789abcdef",
        "rift-helper-0123456789abcdef0123456789abcde/",
        "other-0123456789abcdef0123456789abcdef",
    ] {
        assert!(!pipe_name_valid(bad), "{bad:?}");
    }
}

#[test]
fn the_helper_takes_exactly_a_channel_and_a_token() {
    let args = |list: &[&str]| list.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        parse_helper_args(&args(&[PIPE, TOKEN])),
        Some(HelperArgs {
            pipe: PIPE.into(),
            token: TOKEN.into()
        })
    );
    for bad in [
        args(&[]),
        args(&[PIPE]),
        args(&[PIPE, TOKEN, "extra"]),
        args(&[TOKEN, PIPE]),
        args(&[PIPE, "short"]),
        args(&["--config", r"C:\evil.json"]),
    ] {
        assert_eq!(parse_helper_args(&bad), None, "{bad:?}");
    }
    assert!(token_valid(TOKEN) && !token_valid("") && !token_valid(&TOKEN.to_uppercase()));
}

#[cfg(not(windows))]
#[tokio::test]
async fn off_windows_the_elevator_can_only_fail() {
    use rift_core::elevation::{client::Elevator, uac::ShellElevator};
    let elevator = ShellElevator::new("rift".into());
    assert_eq!(elevator.launch(PIPE, TOKEN).await, Launched::Failed);
}
