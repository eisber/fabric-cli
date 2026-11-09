// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[cfg(feature = "certificate-store")]
#[path = "native_certificate.rs"]
pub(crate) mod certificate;

use crate::{
    BrokerError, NativeStage, WebAccount, WebTokenRequest,
    request::{RequestFuture, Shared, WorkerExit, worker_pool},
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::{Duration, Instant},
};
use windows::{
    Security::Authentication::Web::Core::{WebAuthenticationCoreManager, WebTokenRequestResult},
    Win32::{
        Foundation::{GetLastError, HWND, LPARAM, LRESULT, SetLastError, WIN32_ERROR, WPARAM},
        System::{
            Console::GetConsoleWindow,
            LibraryLoader::GetModuleHandleW,
            WinRT::{
                IWebAuthenticationCoreManagerInterop, RO_INIT_SINGLETHREADED, RoInitialize,
                RoUninitialize,
            },
        },
        UI::WindowsAndMessaging::{
            CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GA_ROOTOWNER, GWLP_USERDATA, GetAncestor, GetWindowLongPtrW, IsWindow, IsWindowVisible,
            MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SC_CLOSE, SW_SHOWNORMAL,
            SetWindowLongPtrW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_CLOSE,
            WM_NCDESTROY, WM_QUIT, WM_SYSCOMMAND, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
    },
    core::w,
};
use windows_future::{AsyncStatus, IAsyncAction, IAsyncOperation};

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

/// Awaitable native operation using a worker-owned WinRT apartment.
///
/// Dropping or cancelling requests cooperative cleanup and suppresses late success.
pub struct NativeOperation<T> {
    inner: RequestFuture<T>,
}

impl<T> NativeOperation<T> {
    /// Gets secret-free cancellation and actual teardown state.
    pub fn control(&self) -> crate::OperationControl {
        self.inner.0.control()
    }

    /// Requests cancellation; no alternate identity or backend is attempted.
    pub fn cancel(&self) {
        self.inner.0.cancel();
    }
}

impl<T> Future for NativeOperation<T> {
    type Output = Result<T, BrokerError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx)
    }
}

pub(crate) fn spawn<T: Send + 'static>(
    work: impl FnOnce(&Shared<T>) -> Result<T, BrokerError> + Send + 'static,
) -> NativeOperation<T> {
    spawn_with_timeout(Duration::from_secs(120), work)
}

pub(crate) fn spawn_with_timeout<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce(&Shared<T>) -> Result<T, BrokerError> + Send + 'static,
) -> NativeOperation<T> {
    let shared = Shared::with_timeout(timeout);
    let permit = match worker_pool().reserve() {
        Ok(permit) => permit,
        Err(error) => {
            shared.complete(Err(error));
            shared.no_worker();
            return NativeOperation {
                inner: RequestFuture(shared),
            };
        }
    };
    let worker_permit = permit.clone();
    let worker_shared = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("fabric-entra".into())
        .spawn(move || {
            let exit = WorkerExit::new(Arc::clone(&worker_shared), worker_permit);
            worker_shared.attach_worker(thread::current());
            let result = (|| {
                if worker_shared.is_cancelled() {
                    return Err(BrokerError::Cancelled);
                }
                let _apartment = Apartment::new()?;
                work(&worker_shared)
            })();
            worker_shared.complete(result);
            exit.finish();
        });
    if spawned.is_err() {
        shared.complete(Err(BrokerError::ThreadStart));
        WorkerExit::new(Arc::clone(&shared), permit).finish();
    }
    NativeOperation {
        inner: RequestFuture(shared),
    }
}

impl BrokerRequest {
    /// Gets cancellation and actual worker teardown state, separate from delivery.
    pub fn control(&self) -> crate::OperationControl {
        self.inner.0.control()
    }

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
    let permit = match worker_pool().reserve() {
        Ok(permit) => permit,
        Err(error) => {
            shared.complete(Err(error));
            shared.no_worker();
            return BrokerRequest {
                inner: RequestFuture(shared),
            };
        }
    };
    let worker_permit = permit.clone();
    let worker_shared = Arc::clone(&shared);
    let spawned = thread::Builder::new()
        .name("fabric-wam".into())
        .spawn(move || {
            let exit = WorkerExit::new(Arc::clone(&worker_shared), worker_permit);
            worker_shared.attach_worker(thread::current());
            let result = run_request(request, account, &worker_shared);
            worker_shared.complete(result);
            exit.finish();
        });
    if spawned.is_err() {
        shared.complete(Err(BrokerError::ThreadStart));
        WorkerExit::new(Arc::clone(&shared), permit).finish();
    }
    BrokerRequest {
        inner: RequestFuture(shared),
    }
}

pub(crate) fn native_error(stage: NativeStage, error: windows::core::Error) -> BrokerError {
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

const SIGN_IN_CLASS: windows::core::PCWSTR = w!("FabricCliBrokerSignInParent");

unsafe extern "system" fn sign_in_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE || message == WM_SYSCOMMAND && wparam.0 & 0xfff0 == SC_CLOSE as usize {
        // SAFETY: Only our private class uses this procedure. This HWND owns an
        // Arc reference to its signal until WM_NCDESTROY detaches it.
        let close = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const AtomicBool;
        if !close.is_null() {
            // SAFETY: The window-owned Arc keeps this signal alive for dispatch.
            unsafe { &*close }.store(true, Ordering::Release);
        }
        return LRESULT(0);
    }
    if message == WM_NCDESTROY {
        // SAFETY: WM_NCDESTROY releases exactly the reference transferred to this
        // private HWND. Detach first so a repeated message cannot release it twice.
        let close = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const AtomicBool;
        // SAFETY: Detach our class's user data on its owning STA during destruction.
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
        if !close.is_null() {
            // SAFETY: This reference came from one Arc::into_raw at attachment.
            // If DestroyWindow fails, this callback does not run and the HWND
            // retains the reference rather than leaving a dangling pointer.
            drop(unsafe { Arc::from_raw(close) });
        }
    }
    // SAFETY: Unhandled messages preserve the documented default window behavior.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn sign_in_class() -> Result<(), BrokerError> {
    static CLASS: OnceLock<Result<(), BrokerError>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        // SAFETY: Query this process's module without loading another library.
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(sign_in_window_proc),
            hInstance: module.into(),
            lpszClassName: SIGN_IN_CLASS,
            ..WNDCLASSW::default()
        };
        // SAFETY: The private class and callback live for the process lifetime.
        if unsafe { RegisterClassW(&class) } == 0 {
            Err(native_error(
                NativeStage::ParentWindow,
                windows::core::Error::from_thread(),
            ))
        } else {
            Ok(())
        }
    })
}

pub(crate) struct ParentWindow(
    Option<HWND>,
    Option<crate::OperationControl>,
    bool,
    Option<Arc<AtomicBool>>,
);

impl ParentWindow {
    pub(crate) fn new() -> Result<Self, BrokerError> {
        sign_in_class()?;
        // SAFETY: Use the same current module that registered the private class.
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
        // SAFETY: Our private class/callback and strings live through this call.
        // The worker owns the returned HWND and the callback never closes it.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                SIGN_IN_CLASS,
                w!("Fabric CLI sign-in"),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(module.into()),
                None,
            )
        }
        .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
        let close = Arc::new(AtomicBool::new(false));
        let mut parent = Self(Some(window), None, true, Some(close));
        let pointer = Arc::into_raw(Arc::clone(parent.3.as_ref().expect("owned close signal")));
        // SAFETY: The HWND receives one signal reference, released on WM_NCDESTROY.
        // Reset last error because a successful setter can return the previous 0.
        unsafe {
            SetLastError(WIN32_ERROR(0));
            let previous = SetWindowLongPtrW(window, GWLP_USERDATA, pointer as isize);
            let error = GetLastError();
            if previous == 0 && error != WIN32_ERROR(0) {
                // SAFETY: The failed setter did not transfer this Arc reference.
                drop(Arc::from_raw(pointer));
                parent.close()?;
                return Err(native_error(
                    NativeStage::ParentWindow,
                    windows::core::Error::from(error),
                ));
            }
        }
        Ok(parent)
    }

    pub(crate) fn for_sign_in() -> Result<Self, BrokerError> {
        // SAFETY: These queries inspect only this process's console and its root
        // owner. No arbitrary foreground window is borrowed or modified.
        let terminal = unsafe {
            let console = GetConsoleWindow();
            let root = GetAncestor(console, GA_ROOTOWNER);
            [root, console].into_iter().find(|window| {
                !window.is_invalid()
                    && IsWindow(Some(*window)).as_bool()
                    && IsWindowVisible(*window).as_bool()
            })
        };
        if let Some(window) = terminal {
            return Ok(Self(Some(window), None, false, None));
        }
        let mut parent = Self::new()?;
        // SAFETY: This STA owns the fallback window. It must be visible before
        // WAM is parented to it; a hidden parent can hide the sign-in dialog.
        unsafe {
            let _ = ShowWindow(parent.handle(), SW_SHOWNORMAL);
        }
        // SAFETY: The newly created window is valid and owned by this thread.
        if !unsafe { IsWindowVisible(parent.handle()) }.as_bool() {
            parent.close()?;
            return Err(BrokerError::InteractiveWindowUnavailable);
        }
        Ok(parent)
    }

    pub(crate) fn track_cleanup(&mut self, control: crate::OperationControl) {
        self.1 = Some(control);
    }

    fn handle(&self) -> HWND {
        self.0.expect("the parent window has not been closed")
    }

    fn close_requested(&self) -> bool {
        self.3
            .as_ref()
            .is_some_and(|signal| signal.load(Ordering::Acquire))
    }

    pub(crate) fn close(&mut self) -> Result<(), BrokerError> {
        if let Some(window) = self.0 {
            // SAFETY: Borrowed terminal windows are never destroyed. Owned HWNDs
            // are closed on their STA, unless already closed by the user.
            if self.2 && unsafe { IsWindow(Some(window)) }.as_bool() {
                // SAFETY: This worker owns the still-live window on this STA.
                unsafe { DestroyWindow(window) }
                    .map_err(|error| native_error(NativeStage::ParentWindow, error))?;
            }
            self.0 = None;
        }
        Ok(())
    }
}

impl Drop for ParentWindow {
    fn drop(&mut self) {
        // Normal paths call close and return its error; this is panic-path cleanup
        // or a best-effort retry after a reported close failure.
        let failed = self.close().is_err();
        if let Some(control) = &self.1 {
            control.cleanup_uncertain(failed);
        }
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
    let mut parent = ParentWindow::for_sign_in()?;
    parent.track_cleanup(shared.control());
    let result = run_operation(request, account, shared, &parent);
    if let Err(error) = parent.close() {
        shared.uncertain_cleanup();
        return Err(error);
    }
    result
}

fn run_operation(
    request: WebTokenRequest,
    account: Option<WebAccount>,
    shared: &Shared<BrokerResponse>,
    parent: &ParentWindow,
) -> Result<BrokerResponse, BrokerError> {
    if shared.is_cancelled() {
        return Err(BrokerError::Cancelled);
    }
    let operation = start_interactive(request, account, parent)?;
    wait_for_window(operation, shared, parent).map(BrokerResponse)
}

pub(crate) fn start_interactive(
    request: WebTokenRequest,
    account: Option<WebAccount>,
    parent: &ParentWindow,
) -> Result<IAsyncOperation<WebTokenRequestResult>, BrokerError> {
    let interop: IWebAuthenticationCoreManagerInterop = windows::core::factory::<
        WebAuthenticationCoreManager,
        IWebAuthenticationCoreManagerInterop,
    >()
    .map_err(|error| native_error(NativeStage::Factory, error))?;
    // SAFETY: The concrete result type supplies the documented IAsyncOperation IID.
    // Request/account are valid typed WinRT objects. The parent HWND and STA are
    // owned here and retained until native completion, even if the future is dropped.
    unsafe {
        if let Some(account) = &account {
            interop.RequestTokenWithWebAccountForWindowAsync(parent.handle(), &request, account)
        } else {
            interop.RequestTokenForWindowAsync(parent.handle(), &request)
        }
    }
    .map_err(|error| native_error(NativeStage::Request, error))
}

pub(crate) trait Pending<T> {
    fn completed(
        &self,
        callback: impl FnOnce(windows::core::Result<T>) + Send + 'static,
    ) -> windows::core::Result<()>;
    fn status(&self) -> windows::core::Result<AsyncStatus>;
    fn results(&self) -> windows::core::Result<T>;
    fn cancel(&self) -> windows::core::Result<()>;
}

impl<T: windows::core::RuntimeType + 'static> Pending<T> for IAsyncOperation<T> {
    fn completed(
        &self,
        callback: impl FnOnce(windows::core::Result<T>) + Send + 'static,
    ) -> windows::core::Result<()> {
        self.when(callback)
    }
    fn status(&self) -> windows::core::Result<AsyncStatus> {
        self.Status()
    }
    fn results(&self) -> windows::core::Result<T> {
        self.GetResults()
    }
    fn cancel(&self) -> windows::core::Result<()> {
        self.Cancel()
    }
}

impl Pending<()> for IAsyncAction {
    fn completed(
        &self,
        callback: impl FnOnce(windows::core::Result<()>) + Send + 'static,
    ) -> windows::core::Result<()> {
        self.when(callback)
    }
    fn status(&self) -> windows::core::Result<AsyncStatus> {
        self.Status()
    }
    fn results(&self) -> windows::core::Result<()> {
        self.GetResults()
    }
    fn cancel(&self) -> windows::core::Result<()> {
        self.Cancel()
    }
}

pub(crate) fn wait<T: Send + 'static, O>(
    operation: impl Pending<T>,
    shared: &Shared<O>,
) -> Result<T, BrokerError> {
    wait_inner(operation, shared, None)
}

pub(crate) fn wait_for_window<T: Send + 'static, O>(
    operation: impl Pending<T>,
    shared: &Shared<O>,
    parent: &ParentWindow,
) -> Result<T, BrokerError> {
    wait_inner(operation, shared, Some(parent))
}

fn wait_inner<T: Send + 'static, O>(
    operation: impl Pending<T>,
    shared: &Shared<O>,
    parent: Option<&ParentWindow>,
) -> Result<T, BrokerError> {
    let completed = Arc::new(Mutex::new(None));
    let poisoned = Arc::new(AtomicBool::new(false));
    let callback_poisoned = Arc::clone(&poisoned);
    let callback_completed = Arc::clone(&completed);
    let worker = thread::current();
    let callback_error = operation
        .completed(move |result| {
            match callback_completed.lock() {
                Ok(mut completed) => *completed = Some(result),
                Err(_) => callback_poisoned.store(true, Ordering::Release),
            }
            worker.unpark();
        })
        .err()
        .map(|error| native_error(NativeStage::Completion, error));
    let mut cancel_sent = false;
    let mut cleanup_error = callback_error;
    let mut cancellation_started = None;
    if let Some(BrokerError::Native { code, .. }) = callback_error {
        shared.complete(Err(BrokerError::CompletionUnconfirmed { code }));
    }
    let result = loop {
        shared.check_deadline();
        if parent.is_some_and(|parent| {
            // SAFETY: IsWindow accepts live or destroyed HWNDs and never mutates
            // the borrowed terminal or owned parent window.
            parent.close_requested() || !unsafe { IsWindow(Some(parent.handle())) }.as_bool()
        }) {
            shared.cancel();
        }
        let callback_result = match completed.lock() {
            Ok(mut completed) => completed.take(),
            Err(_) => {
                poisoned.store(true, Ordering::Release);
                None
            }
        };
        if poisoned.load(Ordering::Acquire) {
            cleanup_error = Some(BrokerError::CoordinationFailed);
            shared.complete(Err(BrokerError::CoordinationFailed));
        }
        if let Some(result) = callback_result {
            break result.map_err(|error| native_error(NativeStage::Completion, error));
        }
        if let Ok(status) = operation.status()
            && status != AsyncStatus::Started
        {
            break operation
                .results()
                .map_err(|error| native_error(NativeStage::Completion, error));
        }
        if (shared.is_cancelled() || cleanup_error.is_some()) && !cancel_sent {
            cancel_sent = true;
            cancellation_started = Some(Instant::now());
            if let Err(error) = operation.cancel() {
                let error = native_error(NativeStage::Cancellation, error);
                cleanup_error = Some(error);
                shared.complete(Err(error));
            }
        }
        if cancellation_started.is_some_and(|start| start.elapsed() >= Duration::from_secs(5)) {
            let (active, limit) = crate::OperationControl::worker_counts();
            shared.complete(Err(BrokerError::CleanupPending { active, limit }));
        }
        let mut message = MSG::default();
        // SAFETY: The MSG out-pointer is valid and HWND=None reads only this
        // worker's queue. The parent and its system window procedure remain alive.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            let closes_parent = parent.is_some_and(|parent| {
                parent.2
                    && message.hwnd == parent.handle()
                    && (message.message == WM_CLOSE
                        || message.message == WM_SYSCOMMAND
                            && message.wParam.0 & 0xfff0 == SC_CLOSE as usize)
            });
            if message.message == WM_QUIT || closes_parent {
                // Keep an owned modal parent alive until native cancellation and
                // teardown finish, instead of dispatching its close immediately.
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
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SendMessageW};

    #[test]
    fn borrowed_parent_is_never_destroyed_by_broker_teardown() {
        thread::spawn(|| {
            let _apartment = Apartment::new().unwrap();
            let mut owner = ParentWindow::new().unwrap();
            let handle = owner.handle();
            let signal = Arc::clone(owner.3.as_ref().unwrap());
            assert_eq!(Arc::strong_count(&signal), 3);
            {
                let mut borrowed = ParentWindow(Some(handle), None, false, None);
                borrowed.close().unwrap();
                borrowed.close().unwrap();
            }
            // SAFETY: The original owner still holds the live HWND on this STA.
            assert!(unsafe { IsWindow(Some(handle)) }.as_bool());
            owner.close().unwrap();
            assert_eq!(Arc::strong_count(&signal), 2);
        })
        .join()
        .unwrap();
    }

    struct CancelledOperation {
        cancelled: Arc<AtomicBool>,
        parent: HWND,
    }

    impl Pending<()> for CancelledOperation {
        fn completed(
            &self,
            _: impl FnOnce(windows::core::Result<()>) + Send + 'static,
        ) -> windows::core::Result<()> {
            Ok(())
        }

        fn status(&self) -> windows::core::Result<AsyncStatus> {
            Ok(if self.cancelled.load(Ordering::Acquire) {
                AsyncStatus::Canceled
            } else {
                AsyncStatus::Started
            })
        }

        fn results(&self) -> windows::core::Result<()> {
            Ok(())
        }

        fn cancel(&self) -> windows::core::Result<()> {
            // SAFETY: The hidden test parent must stay owned and live until the
            // pending operation observes cancellation, just like a visible one.
            assert!(unsafe { IsWindow(Some(self.parent)) }.as_bool());
            self.cancelled.store(true, Ordering::Release);
            Ok(())
        }
    }

    #[test]
    fn parent_close_messages_cancel_before_releasing_the_modal_parent() {
        thread::spawn(|| {
            let _apartment = Apartment::new().unwrap();
            for message in [WM_CLOSE, WM_SYSCOMMAND] {
                let mut parent = ParentWindow::new().unwrap();
                let handle = parent.handle();
                let shared = Shared::<()>::new();
                let cancelled = Arc::new(AtomicBool::new(false));
                let pending = CancelledOperation {
                    cancelled: Arc::clone(&cancelled),
                    parent: handle,
                };
                // SAFETY: This test owns the hidden window and posts only its
                // close request; no provider, sign-in or visible UI is invoked.
                unsafe {
                    PostMessageW(Some(handle), message, WPARAM(SC_CLOSE as usize), LPARAM(0))
                }
                .unwrap();
                assert_eq!(
                    wait_for_window(pending, &shared, &parent),
                    Err(BrokerError::Cancelled)
                );
                assert!(cancelled.load(Ordering::Acquire));
                // SAFETY: The close message was converted to cancellation,
                // retaining the window until the operation reached terminal state.
                assert!(unsafe { IsWindow(Some(handle)) }.as_bool());
                parent.close().unwrap();
                // SAFETY: IsWindow accepts the now-destroyed handle.
                assert!(!unsafe { IsWindow(Some(handle)) }.as_bool());
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    fn sent_close_messages_keep_the_owned_parent_alive_until_native_cancellation_finishes() {
        thread::spawn(|| {
            let _apartment = Apartment::new().unwrap();
            for message in [WM_CLOSE, WM_SYSCOMMAND] {
                let mut parent = ParentWindow::new().unwrap();
                let handle = parent.handle();
                let shared = Shared::<()>::with_timeout(Duration::from_secs(2));
                let cancelled = Arc::new(AtomicBool::new(false));
                let pending = CancelledOperation {
                    cancelled: Arc::clone(&cancelled),
                    parent: handle,
                };
                let handle_value = handle.0 as usize;
                let sender = thread::spawn(move || {
                    let handle = HWND(handle_value as *mut std::ffi::c_void);
                    // SAFETY: The owner retains this hidden test window until
                    // send completes; the message has no borrowed data pointers.
                    unsafe {
                        SendMessageW(
                            handle,
                            message,
                            Some(WPARAM(SC_CLOSE as usize)),
                            Some(LPARAM(0)),
                        )
                    };
                });
                assert_eq!(
                    wait_for_window(pending, &shared, &parent),
                    Err(BrokerError::Cancelled)
                );
                sender.join().unwrap();
                assert!(parent.close_requested());
                assert!(cancelled.load(Ordering::Acquire));
                // SAFETY: Sent messages ran our private procedure, not the
                // default WM_CLOSE handler that would destroy this live parent.
                assert!(unsafe { IsWindow(Some(handle)) }.as_bool());
                parent.close().unwrap();
            }
        })
        .join()
        .unwrap();
    }

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
