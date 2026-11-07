// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private native boundary. No raw pointer or unsafe API leaves this module.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    mem::size_of,
    path::Path,
    sync::{Arc, Mutex},
};

use netcorehost::{
    hostfxr::{DelegateLoader, Hostfxr, ManagedFunctionWithDefaultSignature},
    pdcstring::PdCString,
};

use crate::{
    BindingError, CancellationToken, HostOptions, MAX_RESPONSE_BYTES, validate_response_json,
};

static RUNTIME: Mutex<Option<Arc<Runtime>>> = Mutex::new(None);

/// Must match NativeCall in managed/NativeEntryPoint.cs, using platform alignment.
#[repr(C)]
struct NativeCall {
    abi_version: u32,
    request: *const u8,
    request_len: i32,
    response: *mut u8,
    response_capacity: i32,
    response_len: i32,
}

pub(crate) struct Runtime {
    options: HostOptions,
    entry: ManagedFunctionWithDefaultSignature,
    // ManagedFunction alone does not keep hostfxr's dynamic library loaded.
    _loader: DelegateLoader,
    _host: Hostfxr,
    gate: Mutex<()>,
}

impl Runtime {
    pub(crate) fn load(options: HostOptions) -> Result<Arc<Self>, BindingError> {
        if !options.hostfxr_path().is_absolute() || !options.component_directory().is_absolute() {
            return Err(BindingError::InvalidConfiguration);
        }
        let host_path = options
            .hostfxr_path()
            .canonicalize()
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        let component = options
            .component_directory()
            .canonicalize()
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        let options = HostOptions::new(host_path, component);
        let mut current = RUNTIME.lock().map_err(|_| BindingError::HostUnavailable)?;
        if let Some(runtime) = current.as_ref() {
            return if runtime.options == options {
                Ok(Arc::clone(runtime))
            } else {
                Err(BindingError::RuntimeAlreadyConfigured)
            };
        }

        let assembly = options
            .component_directory()
            .join("Fabric.SemanticModel.Managed.dll");
        let config = options
            .component_directory()
            .join("Fabric.SemanticModel.Managed.runtimeconfig.json");
        let deps = options
            .component_directory()
            .join("Fabric.SemanticModel.Managed.deps.json");
        if !assembly.is_file() || !config.is_file() || !deps.is_file() {
            return Err(BindingError::RuntimeUnavailable);
        }
        let host = Hostfxr::load_from_path(options.hostfxr_path())
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        let context = host
            .initialize_for_runtime_config(platform_string(&config)?)
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        let loader = context
            .get_delegate_loader()
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        let entry = loader
            .load_assembly_and_get_function_with_default_signature(
                &platform_string(&assembly)?,
                &PdCString::from_os_str(
                    "Fabric.SemanticModel.Interop.NativeEntryPoint, Fabric.SemanticModel.Managed",
                )
                .map_err(|_| BindingError::InvalidConfiguration)?,
                &PdCString::from_os_str("Invoke")
                    .map_err(|_| BindingError::InvalidConfiguration)?,
            )
            .map_err(|_| BindingError::RuntimeUnavailable)?;
        // Closing hostfxr's initialization context does not unload CoreCLR.
        // RUNTIME retains the host and loader for the entire process lifetime.
        drop(context);
        let runtime = Arc::new(Self {
            options,
            entry,
            _loader: loader,
            _host: host,
            gate: Mutex::new(()),
        });
        *current = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    pub(crate) fn analyze(
        &self,
        request: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, BindingError> {
        cancellation.check()?;
        let _guard = self
            .gate
            .lock()
            .map_err(|_| BindingError::HostUnavailable)?;
        cancellation.check()?;
        let mut response = Vec::new();
        response
            .try_reserve_exact(MAX_RESPONSE_BYTES)
            .map_err(|_| BindingError::HostUnavailable)?;
        response.resize(MAX_RESPONSE_BYTES, 0);
        let mut call = NativeCall {
            abi_version: 1,
            request: request.as_ptr(),
            request_len: i32::try_from(request.len()).map_err(|_| BindingError::RequestTooLarge)?,
            response: response.as_mut_ptr(),
            response_capacity: i32::try_from(response.len())
                .map_err(|_| BindingError::ResponseTooLarge)?,
            response_len: 0,
        };
        // SAFETY: Invoke has hostfxr's exact ComponentEntryPoint ABI. NativeCall
        // matches the managed sequential struct; both buffers stay allocated and
        // immovable during this synchronous call. The trusted managed component
        // checks all lengths, writes at most response_capacity initialized bytes,
        // retains no pointers, and catches managed exceptions before returning.
        let status = unsafe {
            (self.entry)(
                std::ptr::from_mut(&mut call).cast::<c_void>(),
                i32::try_from(size_of::<NativeCall>())
                    .map_err(|_| BindingError::InvalidResponse)?,
            )
        };
        cancellation.check()?;
        match status {
            0 => {}
            1 => return Err(BindingError::InvalidRequest),
            2 => return Err(BindingError::UnsupportedSchema),
            3 => return Err(BindingError::UnsupportedFormat),
            4 => return Err(BindingError::MalformedModel),
            5 => return Err(BindingError::ManagedFailure),
            6 => return Err(BindingError::ResponseTooLarge),
            _ => return Err(BindingError::InvalidResponse),
        }
        let written =
            usize::try_from(call.response_len).map_err(|_| BindingError::InvalidResponse)?;
        if written == 0 || written > response.len() {
            return Err(BindingError::InvalidResponse);
        }
        response.truncate(written);
        validate_response_json(&response)?;
        cancellation.check()?;
        Ok(response)
    }
}

fn platform_string(path: &Path) -> Result<PdCString, BindingError> {
    PdCString::from_os_str(path).map_err(|_| BindingError::InvalidConfiguration)
}
