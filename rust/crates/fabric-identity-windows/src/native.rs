// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    BrokerError, NativeStage, WebAccount, WebTokenRequest,
    request::{RequestFuture, Shared},
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    thread,
    time::Duration,
};
use windows::{
    Security::Authentication::Web::Core::{WebAuthenticationCoreManager, WebTokenRequestResult},
    Win32::{
        Foundation::HWND,
        System::WinRT::{
            IWebAuthenticationCoreManagerInterop, RO_INIT_SINGLETHREADED, RoInitialize,
            RoUninitialize,
        },
        UI::WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE,
            PeekMessageW, TranslateMessage, WINDOW_EX_STYLE, WM_QUIT, WS_OVERLAPPEDWINDOW,
        },
    },
    core::w,
};
use windows_future::{AsyncStatus, IAsyncOperation};

/// A native response that deliberately does not expose credentials through Debug.
///
/// This is not a validated Fabric session or an Azure `AccessToken`.
pub struct BrokerResponse(WebTokenRequestResult);

impl BrokerResponse {
    /// Returns the native response for explicit provider-specific interpretation.
    ///
    /// It may contain credentials. Do not log or persist it as plaintext.
    pub fn into_native(self) -> WebTokenRequestResult {
        self.0
    }
}

impl fmt::Debug for BrokerResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrokerResponse").finish_non_exhaustive()
    }
}

/// Awaitable desktop request; dropping it requests cancellation, not rollback.
///
/// Native UI and its parent window stay on an owned STA thread until the native
/// operation terminates. This future never blocks the caller's async executor.
pub struct BrokerRequest {
    inner: RequestFuture<BrokerResponse>,
}

impl BrokerRequest {
    /// Requests cancellation without delivering any later success.
    ///
    /// Native cancellation is cooperative. Normal completion waits for cleanup.
    /// If completion notification fails, an explicit unconfirmed error is returned
    /// while the owning UI thread retains pending native resources.
    pub fn cancel(&self) {
        self.inner.0.cancel();
    }
}

impl Future for BrokerRequest {
    type Output = Result<BrokerResponse, BrokerError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

/// Starts an explicitly authorized interactive request on a dedicated STA thread.
///
/// The caller supplies the configured request and optional selected native account.
/// This boundary does not select a provider, tenant, account, scope or fallback.
/// The CLI must initiate it only after explicit user action. It may display
/// broker UI; constructing or linking the crate alone never does so.
///
/// # Errors
///
/// Awaiting the result reports sanitized native failures and cancellation.
/// Provider success/error statuses remain in the native response and must be
/// classified separately; completion is not proof of a successful login.
pub fn request_interactive(request: WebTokenRequest, account: Option<WebAccount>) -> BrokerRequest {
    let shared = Shared::new();
    let worker_shared = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("fabric-wam".into())
        .spawn(move || {
            worker_shared.attach_worker(thread::current());
            let result = run_request(request, account, &worker_shared);
            worker_shared.complete(result);
        });
    if spawned.is_err() {
        shared.complete(Err(BrokerError::ThreadStart));
    }
    BrokerRequest {
        inner: RequestFuture(shared),
    }
}

fn native_error(stage: NativeStage, error: windows::core::Error) -> BrokerError {
    BrokerError::Native {
        stage,
        code: error.code().0,
    }
}

struct Apartment;

impl Apartment {
    fn new() -> Result<Self, BrokerError> {
        // SAFETY: This fresh worker thread owns its STA until Apartment is dropped.
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
            .map_err(|error| native_error(NativeStage::Apartment, error))?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: Paired with successful RoInitialize on this same worker thread.
        unsafe { RoUninitialize() };
    }
}

struct ParentWindow(Option<HWND>);

impl ParentWindow {
    fn new() -> Result<Self, BrokerError> {
        // SAFETY: STATIC is a system class, strings live through this call, and
        // no callback/data pointer is installed. The worker owns the returned HWND.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Fabric CLI sign-in"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
        Ok(Self(Some(window)))
    }

    fn handle(&self) -> HWND {
        self.0.expect("the parent window has not been closed")
    }

    fn close(&mut self) -> Result<(), BrokerError> {
        if let Some(window) = self.0 {
            // SAFETY: This worker owns the HWND and closes it after native completion.
            unsafe { DestroyWindow(window) }
                .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
            self.0 = None;
        }
        Ok(())
    }
}

impl Drop for ParentWindow {
    fn drop(&mut self) {
        // Normal paths call close and return its error; this is panic-path cleanup
        // or a best-effort retry after a reported close failure.
        let _ = self.close();
    }
}

fn run_request(
    request: WebTokenRequest,
    account: Option<WebAccount>,
    shared: &Shared<BrokerResponse>,
) -> Result<BrokerResponse, BrokerError> {
    if shared.is_cancelled() {
        return Err(BrokerError::Cancelled);
    }
    let _apartment = Apartment::new()?;
    let mut parent = ParentWindow::new()?;
    let result = run_operation(request, account, shared, &parent);
    parent.close()?;
    result
}

fn run_operation(
    request: WebTokenRequest,
    account: Option<WebAccount>,
    shared: &Shared<BrokerResponse>,
    parent: &ParentWindow,
) -> Result<BrokerResponse, BrokerError> {
    let interop: IWebAuthenticationCoreManagerInterop = windows::core::factory::<
        WebAuthenticationCoreManager,
        IWebAuthenticationCoreManagerInterop,
    >()
    .map_err(|error| native_error(NativeStage::Factory, error))?;
    if shared.is_cancelled() {
        return Err(BrokerError::Cancelled);
    }
    // SAFETY: The concrete result type supplies the documented IAsyncOperation IID.
    // Request/account are valid typed WinRT objects. The parent HWND and STA are
    // owned here and retained until native completion, even if the future is dropped.
    let operation: IAsyncOperation<WebTokenRequestResult> = unsafe {
        if let Some(account) = &account {
            interop.RequestTokenWithWebAccountForWindowAsync(parent.handle(), &request, account)
        } else {
            interop.RequestTokenForWindowAsync(parent.handle(), &request)
        }
    }
    .map_err(|error| native_error(NativeStage::Request, error))?;

    let completed = Arc::new(Mutex::new(None));
    let callback_completed = Arc::clone(&completed);
    let worker = thread::current();
    let callback_error = operation
        .when(move |result| {
            *callback_completed.lock().unwrap() = Some(result);
            worker.unpark();
        })
        .err()
        .map(|error| native_error(NativeStage::Completion, error));
    let mut cancel_sent = false;
    let mut cleanup_error = callback_error;
    if let Some(BrokerError::Native { code, .. }) = callback_error {
        shared.complete(Err(BrokerError::CompletionUnconfirmed { code }));
    }
    let result = loop {
        if let Some(result) = completed.lock().unwrap().take() {
            break result.map_err(|error| native_error(NativeStage::Completion, error));
        }
        if callback_error.is_some()
            && let Ok(status) = operation.Status()
            && status != AsyncStatus::Started
        {
            break operation
                .GetResults()
                .map_err(|error| native_error(NativeStage::Completion, error));
        }
        if (shared.is_cancelled() || cleanup_error.is_some()) && !cancel_sent {
            cancel_sent = true;
            if let Err(error) = operation.Cancel() {
                cleanup_error = Some(native_error(NativeStage::Cancellation, error));
            }
        }
        let mut message = MSG::default();
        // SAFETY: The MSG out-pointer is valid and HWND=None reads only this
        // worker's queue. The parent and its system window procedure remain alive.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                shared.cancel();
            } else {
                // SAFETY: Dispatch the initialized message on its owning thread.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        // Pump UI messages while waiting, without blocking the caller's executor.
        thread::park_timeout(Duration::from_millis(16));
    };
    drop(operation);
    if let Some(error) = cleanup_error {
        return Err(error);
    }
    if shared.is_cancelled() {
        return Err(BrokerError::Cancelled);
    }
    result.map(BrokerResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};

    #[test]
    fn native_parent_window_is_hidden_and_explicit_close_is_idempotent() {
        thread::spawn(|| {
            let _apartment = Apartment::new().unwrap();
            let mut parent = ParentWindow::new().unwrap();
            let handle = parent.handle();
            // SAFETY: These queries accept an HWND value, including an invalid one;
            // this worker currently owns the live window and never displays it.
            unsafe {
                assert!(IsWindow(Some(handle)).as_bool());
                assert!(!IsWindowVisible(handle).as_bool());
            }
            parent.close().unwrap();
            parent.close().unwrap();
            assert!(parent.0.is_none());
            // SAFETY: IsWindow accepts a handle to an already destroyed window.
            assert!(!unsafe { IsWindow(Some(handle)) }.as_bool());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn native_parent_window_is_released_on_scope_exit() {
        thread::spawn(|| {
            let _apartment = Apartment::new().unwrap();
            let handle = {
                let parent = ParentWindow::new().unwrap();
                parent.handle()
            };
            // SAFETY: IsWindow accepts a handle to an already destroyed window.
            assert!(!unsafe { IsWindow(Some(handle)) }.as_bool());
        })
        .join()
        .unwrap();
    }
}
