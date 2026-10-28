// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#![deny(unnameable_test_items)]

use super::{TokioDeployFiles, serialized_work};
use fabric_cli_transfer::TransferFiles;
use std::{
    future::{Future, poll_fn},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::Poll,
    time::Duration,
};

#[tokio::test]
async fn dropped_deploy_work_keeps_its_permit_until_the_blocking_operation_exits() {
    let runtime = tokio::runtime::Handle::current();
    let mut files = TokioDeployFiles::defer_config(
        PathBuf::from("caller-selected-config.yml"),
        "caller-selected-config.yml".into(),
        Default::default(),
        runtime.clone(),
    );
    let state = files.state.clone();
    let observed_stop = Arc::new(AtomicBool::new(false));
    let observed = observed_stop.clone();
    let (started, waiting) = tokio::sync::oneshot::channel();
    let (release, held) = mpsc::channel();
    let mut first = serialized_work(state.clone(), runtime, move |_, stop| {
        started.send(()).unwrap();
        held.recv_timeout(Duration::from_secs(2)).unwrap();
        observed.store(stop.load(Ordering::Acquire), Ordering::Release);
        Ok(())
    });
    poll_fn(|cx| {
        assert!(Future::poll(first.as_mut(), cx).is_pending());
        Poll::Ready(())
    })
    .await;
    waiting.await.unwrap();
    drop(first);
    assert!(state.try_lock().is_err());
    let mut next = files.resolve_directory_async("not-the-config-parent");
    poll_fn(|cx| {
        assert!(Future::poll(next.as_mut(), cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(state.try_lock().is_err());
    release.send(()).unwrap();
    assert_eq!(
        next.await.unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(observed_stop.load(Ordering::Acquire));
}
