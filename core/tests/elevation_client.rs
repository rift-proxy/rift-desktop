//! Rift's side of the elevated helper: the sequence from the Windows prompt to a running
//! sing-box and back, with a pretend prompt and channel.
mod common;

use common::helper::{tun_store, Prompt, World, SECRET};
use rift_core::{
    elevation::{
        client::{HelperRuntime, HelperTimeouts},
        helper::Exit,
    },
    failure::FailureCode,
    model::Mode,
    runtime::Runtime,
};
use std::{sync::atomic::Ordering, time::Duration};

fn quick() -> HelperTimeouts {
    HelperTimeouts {
        consent: Duration::from_millis(300),
        connect: Duration::from_millis(300),
        hello: Duration::from_millis(300),
        start: Duration::from_millis(300),
        stop: Duration::from_millis(300),
    }
}
fn runtime(world: &std::sync::Arc<World>) -> HelperRuntime {
    HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(quick())
}
async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never: {what}");
}

#[tokio::test]
async fn one_prompt_starts_the_helper_and_sing_box_runs_with_the_store_and_the_secret() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 1);
    assert_eq!(world.helper_calls.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        world.helper_calls.secrets.lock().unwrap().as_slice(),
        [SECRET]
    );
    assert_eq!(
        world.helper_calls.stores.lock().unwrap()[0].settings.mode,
        Mode::Tun
    );
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn a_controlled_restart_goes_through_the_same_helper_without_another_prompt() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    let mut changed = tun_store();
    changed.settings.mode = Mode::TunProxy;
    rt.start(&changed, SECRET).await.unwrap();
    rt.start(&changed, SECRET).await.unwrap();
    assert_eq!(world.prompts(), 1, "no second UAC");
    assert_eq!(world.helper_calls.starts.load(Ordering::SeqCst), 3);
    assert_eq!(world.helpers_alive(), 1, "one helper all along");
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn stopping_ends_the_helper_and_the_next_start_asks_again() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    rt.stop().await.unwrap();
    assert!(!rt.alive().unwrap());
    assert!(!rt.has_session());
    assert_eq!(world.last_exit().await, Exit::Stopped);
    assert!(
        !world.helper_calls.alive.load(Ordering::SeqCst),
        "sing-box is gone with it"
    );

    rt.start(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 2, "every new TUN connection is a new UAC");
}

#[tokio::test]
async fn a_refusal_is_its_own_error_and_starts_nothing() {
    let world = World::new(Prompt::Decline);
    let mut rt = runtime(&world);
    let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationDeclined));
    assert_eq!(world.prompts(), 1);
    assert_eq!(world.helper_calls.starts.load(Ordering::SeqCst), 0);
    assert!(!rt.alive().unwrap());
    assert_eq!(world.helpers_alive(), 0);
}

#[tokio::test]
async fn a_helper_that_could_not_be_started_is_a_failure_not_a_refusal() {
    let world = World::new(Prompt::Fail);
    let mut rt = runtime(&world);
    let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationFailed));
}

#[tokio::test]
async fn a_helper_that_never_connects_and_a_prompt_that_never_answers_time_out() {
    for prompt in [Prompt::NeverConnects, Prompt::Hangs] {
        let world = World::new(prompt);
        let mut rt = runtime(&world);
        let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::ElevationFailed),
            "{prompt:?}"
        );
        assert!(!rt.has_session(), "{prompt:?}");
    }
}

#[tokio::test]
async fn a_greeting_with_the_wrong_token_or_version_is_not_a_helper() {
    for prompt in [Prompt::WrongToken, Prompt::WrongVersion] {
        let world = World::new(prompt);
        let mut rt = runtime(&world);
        let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::ElevationChannel),
            "{prompt:?}"
        );
        assert!(!rt.has_session(), "{prompt:?}");
        assert_eq!(
            world.helper_calls.starts.load(Ordering::SeqCst),
            0,
            "{prompt:?}"
        );
    }
}

#[tokio::test]
async fn a_helper_that_says_nothing_after_the_greeting_is_a_channel_error() {
    let world = World::new(Prompt::Silent);
    let mut rt = runtime(&world);
    let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationChannel));
    assert!(!rt.has_session());
}

#[tokio::test]
async fn a_store_the_helper_refuses_is_an_error_and_the_helper_stays_for_the_next_try() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    let mut proxy = tun_store();
    proxy.settings.mode = Mode::Proxy;
    let error = rt.start(&proxy, SECRET).await.unwrap_err();
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert!(rt.has_session());
    assert!(!rt.alive().unwrap());
    rt.start(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 1);
}

#[tokio::test]
async fn a_start_that_fails_in_sing_box_keeps_the_helper_and_names_the_stage() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    *world.helper_calls.fail_next_start.lock().unwrap() = Some(FailureCode::SidecarStartExit);
    let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
    assert!(rt.has_session());
    rt.start(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 1);
}

#[tokio::test]
async fn sing_box_that_dies_inside_the_helper_is_a_sing_box_crash() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    world.helper_calls.alive.store(false, Ordering::SeqCst);
    eventually("the runtime notices", || !rt.alive().unwrap()).await;
    assert_eq!(rt.crash_code(), FailureCode::SidecarCrashed);
    assert_eq!(world.last_exit().await, Exit::SingBoxExited);
}

#[tokio::test]
async fn a_helper_that_dies_is_told_apart_from_sing_box() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    world.crash_last_helper();
    eventually("the runtime notices", || !rt.alive().unwrap()).await;
    assert_eq!(rt.crash_code(), FailureCode::ElevationHelperCrashed);
    // A new connection after a crash is a new prompt.
    rt.start(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 2);
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn when_rift_goes_away_the_helper_ends_and_takes_sing_box_with_it() {
    let world = World::new(Prompt::Accept);
    let mut rt = runtime(&world);
    rt.start(&tun_store(), SECRET).await.unwrap();
    drop(rt);
    assert_eq!(world.last_exit().await, Exit::ChannelClosed);
    assert!(!world.helper_calls.alive.load(Ordering::SeqCst));
}

#[tokio::test]
async fn validation_is_not_the_helpers_business() {
    let world = World::new(Prompt::Decline);
    let mut rt = runtime(&world);
    rt.validate(&tun_store(), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 0, "checking a config asks for no rights");
}

#[tokio::test]
async fn xray_start_failures_preserve_engine_stage_and_safe_fixed_text() {
    for (code, text) in [
        (
            FailureCode::XrayConfigRejected,
            "Xray отклонил конфигурацию",
        ),
        (
            FailureCode::XrayConfigTimeout,
            "Проверка конфигурации Xray превысила время ожидания",
        ),
        (FailureCode::XrayStartExit, "Xray завершился при запуске"),
        (
            FailureCode::XrayReadinessTimeout,
            "Локальный канал Xray не ответил вовремя",
        ),
    ] {
        let world = World::new(Prompt::Accept);
        *world.helper_calls.fail_next_start.lock().unwrap() = Some(code);
        let mut rt = runtime(&world);
        let error = rt.start(&tun_store(), SECRET).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(code));
        assert_eq!(error.to_string(), text);
        assert!(!error.to_string().contains(SECRET));
        assert!(!error.to_string().contains("fake"));
        rt.stop().await.unwrap();
    }
}
