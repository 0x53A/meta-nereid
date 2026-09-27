//! HWC2 + libhybris backend — merged from nereid-compositor's hwc/mod.rs + hwc/ffi.rs.
//!
//! All symbols are loaded via dlopen/dlsym at runtime so we can cross-compile
//! without having libhybris available on the build host.

#![allow(non_camel_case_types)]

use anyhow::{Context, Result, bail};
use libloading::{Library, Symbol};
use std::ffi::c_void;
use std::cell::UnsafeCell;
use std::os::raw::{c_int, c_uint};
use std::sync::atomic::{AtomicPtr, Ordering};
use tracing::info;

// --- Constants ---

pub const HAL_PIXEL_FORMAT_RGBA_8888: c_int = 1;

pub const HWC2_POWER_MODE_OFF: c_int = 0;
pub const HWC2_POWER_MODE_ON: c_int = 2;

// --- Opaque pointer types ---

pub type HwcDevice = c_void;
pub type HwcDisplay = c_void;
pub type HwcLayer = c_void;
pub type EGLNativeWindowType = *mut c_void;

/// HWC2EventListener — struct of 3 function pointers passed to register_callback.
/// The HAL calls these (especially on_hotplug_received) synchronously during registration.
#[repr(C)]
pub struct HWC2EventListener {
    pub on_vsync_received: unsafe extern "C" fn(
        listener: *mut HWC2EventListener,
        sequence_id: i32,
        display: u64,
        timestamp: i64,
    ),
    pub on_hotplug_received: unsafe extern "C" fn(
        listener: *mut HWC2EventListener,
        sequence_id: i32,
        display: u64,
        connected: bool,
        primary_display: bool,
    ),
    pub on_refresh_received: unsafe extern "C" fn(
        listener: *mut HWC2EventListener,
        sequence_id: i32,
        display: u64,
    ),
}

/// Function pointer table loaded from libhybris at runtime.
pub struct HwcFns {
    _lib_hwc2: Library,
    _lib_hwcnw: Library,

    pub device_new: unsafe extern "C" fn(bool) -> *mut HwcDevice,
    pub device_get_display_by_id: unsafe extern "C" fn(*mut HwcDevice, u64) -> *mut HwcDisplay,
    pub device_register_callback:
        unsafe extern "C" fn(*mut HwcDevice, *mut HWC2EventListener, c_int),
    pub device_on_hotplug: unsafe extern "C" fn(*mut HwcDevice, u64, i32),
    pub display_set_power_mode: unsafe extern "C" fn(*mut HwcDisplay, c_int) -> c_int,
    pub display_create_layer: unsafe extern "C" fn(*mut HwcDisplay) -> *mut HwcLayer,
    pub layer_set_composition_type: unsafe extern "C" fn(*mut HwcLayer, c_int) -> c_int,
    pub display_validate: unsafe extern "C" fn(*mut HwcDisplay, *mut u32, *mut u32) -> c_int,
    pub display_accept_changes: unsafe extern "C" fn(*mut HwcDisplay) -> c_int,
    pub display_present: unsafe extern "C" fn(*mut HwcDisplay, *mut c_int) -> c_int,
    pub display_get_release_fences:
        unsafe extern "C" fn(*mut HwcDisplay, *mut *mut c_void) -> c_int,
    pub out_fences_destroy: unsafe extern "C" fn(*mut c_void),
    pub display_set_client_target:
        unsafe extern "C" fn(*mut HwcDisplay, u32, *mut c_void, i32, i32) -> c_int,
    pub layer_set_display_frame:
        unsafe extern "C" fn(*mut HwcLayer, i32, i32, i32, i32) -> c_int,
    pub layer_set_source_crop:
        unsafe extern "C" fn(*mut HwcLayer, f32, f32, f32, f32) -> c_int,
    pub layer_set_visible_region:
        unsafe extern "C" fn(*mut HwcLayer, i32, i32, i32, i32) -> c_int,
    pub layer_set_blend_mode: unsafe extern "C" fn(*mut HwcLayer, c_int) -> c_int,
    pub layer_set_plane_alpha: unsafe extern "C" fn(*mut HwcLayer, f32) -> c_int,

    pub nw_create: unsafe extern "C" fn(
        c_uint,
        c_uint,
        c_int,
        unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void),
        *mut c_void,
    ) -> EGLNativeWindowType,
    pub nw_destroy: unsafe extern "C" fn(EGLNativeWindowType),
    pub nw_get_fence: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub nw_set_fence: unsafe extern "C" fn(*mut c_void, c_int),
}

impl HwcFns {
    /// Load all hwcomposer symbols from libhybris shared libraries.
    pub fn load() -> Result<Self> {
        unsafe {
            let lib_hwc2 = Library::new("libhwc2.so.1")
                .or_else(|_| Library::new("libhwc2_compat_layer.so"))
                .context("Failed to load libhwc2.so.1")?;
            let lib_hwcnw = Library::new("libhybris-hwcomposerwindow.so.1")
                .or_else(|_| Library::new("libhybris-hwcomposerwindow.so"))
                .context("Failed to load libhybris-hwcomposerwindow.so.1")?;

            macro_rules! load {
                ($lib:expr, $name:literal) => {{
                    let sym: Symbol<_> = $lib
                        .get($name.as_bytes())
                        .with_context(|| format!("Symbol {} not found", $name))?;
                    *sym
                }};
            }

            Ok(Self {
                device_new: load!(lib_hwc2, "hwc2_compat_device_new"),
                device_get_display_by_id: load!(lib_hwc2, "hwc2_compat_device_get_display_by_id"),
                device_register_callback: load!(lib_hwc2, "hwc2_compat_device_register_callback"),
                device_on_hotplug: load!(lib_hwc2, "hwc2_compat_device_on_hotplug"),
                display_set_power_mode: load!(lib_hwc2, "hwc2_compat_display_set_power_mode"),
                display_create_layer: load!(lib_hwc2, "hwc2_compat_display_create_layer"),
                layer_set_composition_type: load!(
                    lib_hwc2,
                    "hwc2_compat_layer_set_composition_type"
                ),
                display_validate: load!(lib_hwc2, "hwc2_compat_display_validate"),
                display_accept_changes: load!(lib_hwc2, "hwc2_compat_display_accept_changes"),
                display_present: load!(lib_hwc2, "hwc2_compat_display_present"),
                display_get_release_fences: load!(
                    lib_hwc2,
                    "hwc2_compat_display_get_release_fences"
                ),
                out_fences_destroy: load!(lib_hwc2, "hwc2_compat_out_fences_destroy"),
                display_set_client_target: load!(
                    lib_hwc2,
                    "hwc2_compat_display_set_client_target"
                ),
                layer_set_display_frame: load!(
                    lib_hwc2,
                    "hwc2_compat_layer_set_display_frame"
                ),
                layer_set_source_crop: load!(lib_hwc2, "hwc2_compat_layer_set_source_crop"),
                layer_set_blend_mode: load!(lib_hwc2, "hwc2_compat_layer_set_blend_mode"),
                layer_set_plane_alpha: load!(lib_hwc2, "hwc2_compat_layer_set_plane_alpha"),
                layer_set_visible_region: load!(
                    lib_hwc2,
                    "hwc2_compat_layer_set_visible_region"
                ),

                nw_create: load!(lib_hwcnw, "HWCNativeWindowCreate"),
                nw_destroy: load!(lib_hwcnw, "HWCNativeWindowDestroy"),
                nw_get_fence: load!(lib_hwcnw, "HWCNativeBufferGetFence"),
                nw_set_fence: load!(lib_hwcnw, "HWCNativeBufferSetFence"),

                _lib_hwc2: lib_hwc2,
                _lib_hwcnw: lib_hwcnw,
            })
        }
    }
}

// --- Display info ---

#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
}

// --- Hotplug callback globals ---

static CALLBACK_DEVICE: AtomicPtr<HwcDevice> = AtomicPtr::new(std::ptr::null_mut());
static CALLBACK_ON_HOTPLUG: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "C" fn hotplug_callback(
    _listener: *mut HWC2EventListener,
    _sequence_id: i32,
    display_id: u64,
    connected: bool,
    _primary_display: bool,
) {
    unsafe {
        let device = CALLBACK_DEVICE.load(Ordering::SeqCst);
        let on_hotplug_ptr = CALLBACK_ON_HOTPLUG.load(Ordering::SeqCst);
        if device.is_null() || on_hotplug_ptr.is_null() {
            return;
        }
        let on_hotplug: unsafe extern "C" fn(*mut HwcDevice, u64, i32) =
            std::mem::transmute(on_hotplug_ptr);
        let conn = if connected { 1i32 } else { 0i32 };
        info!(display_id, conn, "Hotplug callback fired");
        on_hotplug(device, display_id, conn);
    }
}

unsafe extern "C" fn vsync_callback(
    _listener: *mut HWC2EventListener,
    _sequence_id: i32,
    _display: u64,
    _timestamp: i64,
) {
}

unsafe extern "C" fn refresh_callback(
    _listener: *mut HWC2EventListener,
    _sequence_id: i32,
    _display: u64,
) {
}

// --- Present callback ---

/// Context passed as cb_data to the present callback.
struct PresentContext {
    display: *mut HwcDisplay,
    display_validate: unsafe extern "C" fn(*mut HwcDisplay, *mut u32, *mut u32) -> c_int,
    display_accept_changes: unsafe extern "C" fn(*mut HwcDisplay) -> c_int,
    display_set_client_target:
        unsafe extern "C" fn(*mut HwcDisplay, u32, *mut c_void, i32, i32) -> c_int,
    display_present: unsafe extern "C" fn(*mut HwcDisplay, *mut c_int) -> c_int,
    display_get_release_fences: unsafe extern "C" fn(*mut HwcDisplay, *mut *mut c_void) -> c_int,
    out_fences_destroy: unsafe extern "C" fn(*mut c_void),
    nw_get_fence: unsafe extern "C" fn(*mut c_void) -> c_int,
    nw_set_fence: unsafe extern "C" fn(*mut c_void, c_int),
    last_present_fence: c_int,
    error: Option<String>,
}

unsafe extern "C" fn present_callback(
    cb_data: *mut c_void,
    _window: *mut c_void,
    buffer: *mut c_void,
) {
    unsafe {
        let ctx = &mut *(*(cb_data as *const UnsafeCell<PresentContext>)).get();
        if ctx.error.is_some() { return; }

        let mut num_types: u32 = 0;
        let mut num_requests: u32 = 0;

        let status = (ctx.display_validate)(ctx.display, &mut num_types, &mut num_requests);
        // HWC2_ERROR_HAS_CHANGES=5 is a successful validation requiring accept.
        if status != 0 && status != 5 {
            ctx.error = Some(format!("validateDisplay: {status}")); return;
        }
        let status = (ctx.display_accept_changes)(ctx.display);
        if status != 0 { ctx.error = Some(format!("acceptChanges: {status}")); return; }

        let acquire_fence = (ctx.nw_get_fence)(buffer);
        // HAL_DATASPACE_UNKNOWN = 0
        let status = (ctx.display_set_client_target)(ctx.display, 0, buffer, acquire_fence, 0);
        if status != 0 { ctx.error = Some(format!("setClientTarget: {status}")); return; }

        let mut present_fence: c_int = -1;
        let status = (ctx.display_present)(ctx.display, &mut present_fence);
        if status != 0 {
            if present_fence >= 0 { libc::close(present_fence); }
            ctx.error = Some(format!("presentDisplay: {status}")); return;
        }

        // Diagnostic caller drains each submitted frame before another swap.
        if ctx.last_present_fence >= 0 { libc::close(ctx.last_present_fence); }
        ctx.last_present_fence = present_fence;

        // Set the present fence on the buffer so EGL knows when it's released
        (ctx.nw_set_fence)(buffer, if present_fence >= 0 { libc::dup(present_fence) } else { -1 });

        // Clean up release fences
        let mut out_fences: *mut c_void = std::ptr::null_mut();
        let status = (ctx.display_get_release_fences)(ctx.display, &mut out_fences);
        if status != 0 { ctx.error = Some(format!("getReleaseFences: {status}")); }
        if !out_fences.is_null() {
            (ctx.out_fences_destroy)(out_fences);
        }
    }
}

// --- HwcBackend ---

/// Safe wrapper around the hwcomposer2 + libhybris stack.
pub struct HwcBackend {
    fns: HwcFns,
    display: *mut HwcDisplay,
    #[allow(dead_code)]
    layer: *mut HwcLayer,
    native_window: EGLNativeWindowType,
    _listener: Box<HWC2EventListener>,
    _present_ctx: Box<UnsafeCell<PresentContext>>,
    pub info: DisplayInfo,
}

unsafe impl Send for HwcBackend {}

impl HwcBackend {
    pub fn new() -> Result<Self> {
        info!("Loading libhybris symbols...");
        let fns = HwcFns::load().context("Failed to load libhybris")?;

        unsafe {
            let device = (fns.device_new)(false);
            if device.is_null() {
                bail!("hwc2_compat_device_new failed");
            }
            info!("HWC2 device created");

            // Set up global state for the hotplug callback
            CALLBACK_DEVICE.store(device, Ordering::SeqCst);
            CALLBACK_ON_HOTPLUG.store(
                fns.device_on_hotplug as *mut c_void,
                Ordering::SeqCst,
            );

            // Create listener with callback function pointers
            let mut listener = Box::new(HWC2EventListener {
                on_vsync_received: vsync_callback,
                on_hotplug_received: hotplug_callback,
                on_refresh_received: refresh_callback,
            });

            // register_callback fires on_hotplug_received synchronously for the primary display
            info!("Registering HWC2 callbacks...");
            (fns.device_register_callback)(device, &mut *listener as *mut _, 0);
            info!("Callbacks registered");

            // After register_callback, the display should be in the internal map
            let display = (fns.device_get_display_by_id)(device, 0);
            if display.is_null() {
                bail!("Failed to get primary display (get_display_by_id returned null after register_callback)");
            }
            info!("Got primary display");

            let info = DisplayInfo {
                width: 416,
                height: 416,
            };
            info!(?info, "Display info (hardcoded for hoki)");

            // Power on
            check_hwc((fns.display_set_power_mode)(display, HWC2_POWER_MODE_ON), "initial power ON")?;

            // Create composition layer
            let layer = (fns.display_create_layer)(display);
            if layer.is_null() {
                bail!("Failed to create HWC layer");
            }

            // HWC2_COMPOSITION_CLIENT = 1
            check_hwc((fns.layer_set_composition_type)(layer, 1), "layer_set_composition_type")?;

            // Set layer geometry to cover the full display
            let w = info.width as i32;
            let h = info.height as i32;
            check_hwc((fns.layer_set_display_frame)(layer, 0, 0, w, h), "layer_set_display_frame")?;
            check_hwc((fns.layer_set_source_crop)(layer, 0.0, 0.0, info.width as f32, info.height as f32), "layer_set_source_crop")?;
            // HWC2_BLEND_MODE_NONE = 0
            check_hwc((fns.layer_set_blend_mode)(layer, 0), "layer_set_blend_mode")?;
            check_hwc((fns.layer_set_plane_alpha)(layer, 1.0), "layer_set_plane_alpha")?;
            check_hwc((fns.layer_set_visible_region)(layer, 0, 0, w, h), "layer_set_visible_region")?;

            // Create present context
            let present_ctx = Box::new(UnsafeCell::new(PresentContext {
                display,
                display_validate: fns.display_validate,
                display_accept_changes: fns.display_accept_changes,
                display_set_client_target: fns.display_set_client_target,
                display_present: fns.display_present,
                display_get_release_fences: fns.display_get_release_fences,
                out_fences_destroy: fns.out_fences_destroy,
                nw_get_fence: fns.nw_get_fence,
                nw_set_fence: fns.nw_set_fence,
                last_present_fence: -1,
                error: None,
            }));

            // Create native window with present callback
            let cb_data = &*present_ctx as *const UnsafeCell<PresentContext> as *mut c_void;
            let native_window = (fns.nw_create)(
                info.width,
                info.height,
                HAL_PIXEL_FORMAT_RGBA_8888,
                present_callback,
                cb_data,
            );
            if native_window.is_null() {
                bail!("HWCNativeWindowCreate failed (present callback required)");
            }
            info!("Native window created ({}x{}) with present callback", info.width, info.height);

            Ok(Self {
                fns,
                display,
                layer,
                native_window,
                _listener: listener,
                _present_ctx: present_ctx,
                info,
            })
        }
    }

    /// Get the EGLNativeWindowType for eglCreateWindowSurface.
    pub fn native_window_handle(&self) -> EGLNativeWindowType {
        self.native_window
    }

    /// Called only after synchronous eglSwapBuffers returns, with no concurrent swap.
    pub fn drain_frame(&mut self) -> Result<()> {
        let ctx = self._present_ctx.get_mut();
        if let Some(error) = &ctx.error { bail!("HWC callback: {error}"); }
        wait_fence(ctx.last_present_fence, 1500)?;
        if ctx.last_present_fence >= 0 { unsafe { libc::close(ctx.last_present_fence); } }
        ctx.last_present_fence = -1;
        Ok(())
    }

    pub fn set_power_mode(&self, mode: c_int) -> Result<()> {
        let status = unsafe { (self.fns.display_set_power_mode)(self.display, mode) };
        info!(mode, status, "HWC power transition result");
        if status != 0 { bail!("HWC power mode {mode}: status {status}"); }
        Ok(())
    }

}

impl Drop for HwcBackend {
    fn drop(&mut self) {
        unsafe {
            if !self.native_window.is_null() {
                (self.fns.nw_destroy)(self.native_window);
            }
            if !self.display.is_null() {
                (self.fns.display_set_power_mode)(self.display, HWC2_POWER_MODE_OFF);
            }
        }
    }
}

fn check_hwc(status: c_int, operation: &str) -> Result<()> {
    if status != 0 { bail!("{operation}: HWC status {status}"); }
    Ok(())
}

fn wait_fence(fd: c_int, timeout_ms: c_int) -> Result<()> {
    // -1 is the HWC convention for an already-completed frame.
    if fd == -1 { return Ok(()); }
    if fd < -1 { bail!("invalid fence {fd}"); }
    let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let result = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if result < 0 { return Err(std::io::Error::last_os_error()).context("present fence poll"); }
    if result == 0 { bail!("present fence timed out"); }
    if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
        || pfd.revents & libc::POLLIN == 0 {
        bail!("present fence failed: revents={:#x}", pfd.revents);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd, AsRawFd};
    #[test]
    fn drain_requires_completion_and_rejects_bad_fences() {
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(raw >= 0);
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        assert!(wait_fence(fd.as_raw_fd(), 0).unwrap_err().to_string().contains("timed out"));
        let value = 1u64;
        assert_eq!(unsafe { libc::write(fd.as_raw_fd(), (&value as *const u64).cast(), 8) }, 8);
        wait_fence(fd.as_raw_fd(), 0).unwrap();
        assert!(wait_fence(i32::MAX, 0).is_err());
        assert!(wait_fence(-2, 0).is_err());
        wait_fence(-1, 0).unwrap();
    }
}
