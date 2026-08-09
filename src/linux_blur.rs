//! Native Linux blur-behind integration for Frost.
//!
//! Transparency and blur are separate compositor features on Linux. Tauri's
//! transparent WebKitGTK surface gives us real alpha, but CSS
//! `backdrop-filter` cannot sample pixels outside the webview. KWin exposes a
//! native blur request on both of its display backends, so opt into that when
//! available and leave other compositors with honest tinted translucency.

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};

/// Enable or disable compositor blur for a Tauri window.
///
/// Returns `true` when a native request was sent. A `false` result is not an
/// error: it means the active compositor/backend has no protocol Coffee CLI
/// can use, so the transparent theme tint remains as the graceful fallback.
pub fn set_blur(window: &tauri::WebviewWindow, on: bool) -> bool {
    let Ok(window_handle) = window.window_handle() else {
        return false;
    };
    let Ok(display_handle) = window.display_handle() else {
        return false;
    };

    match (window_handle.as_raw(), display_handle.as_raw()) {
        (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
            set_x11_blur(display.display, window.window, on)
        }
        (RawWindowHandle::Wayland(window), RawDisplayHandle::Wayland(display)) => unsafe {
            set_wayland_blur(display.display.as_ptr(), window.surface.as_ptr(), on)
        },
        _ => false,
    }
}

fn set_x11_blur(
    display: Option<std::ptr::NonNull<std::ffi::c_void>>,
    window: std::ffi::c_ulong,
    on: bool,
) -> bool {
    use std::ffi::CString;
    use x11_dl::xlib;

    let Some(display) = display else {
        return false;
    };
    let Ok(xlib) = xlib::Xlib::open() else {
        return false;
    };
    let Ok(name) = CString::new("_KDE_NET_WM_BLUR_BEHIND_REGION") else {
        return false;
    };

    unsafe {
        let display = display.as_ptr().cast::<xlib::Display>();
        let atom = (xlib.XInternAtom)(display, name.as_ptr(), xlib::False);
        if atom == 0 {
            return false;
        }

        if on {
            // A present, zero-length CARDINAL property means the whole client
            // area. This is KWindowEffects' null-region representation.
            (xlib.XChangeProperty)(
                display,
                window,
                atom,
                xlib::XA_CARDINAL,
                32,
                xlib::PropModeReplace,
                std::ptr::null(),
                0,
            );
        } else {
            (xlib.XDeleteProperty)(display, window, atom);
        }
        (xlib.XFlush)(display);
    }

    true
}

mod kwin_protocol {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("src/protocols/kwin-blur.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("src/protocols/kwin-blur.xml");
}

use kwin_protocol::{
    org_kde_kwin_blur::{self, OrgKdeKwinBlur},
    org_kde_kwin_blur_manager::{self, OrgKdeKwinBlurManager},
};
use std::sync::Mutex;
use wayland_client::{
    protocol::{wl_registry, wl_surface::WlSurface},
    Connection, Dispatch, Proxy, QueueHandle,
};

#[derive(Default)]
struct WaylandRegistryState {
    manager: Option<OrgKdeKwinBlurManager>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandRegistryState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "org_kde_kwin_blur_manager" {
                state.manager = Some(registry.bind::<OrgKdeKwinBlurManager, (), Self>(
                    name,
                    version.min(1),
                    queue,
                    (),
                ));
            }
        }
    }
}

impl Dispatch<OrgKdeKwinBlurManager, ()> for WaylandRegistryState {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinBlurManager,
        _: org_kde_kwin_blur_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<OrgKdeKwinBlur, ()> for WaylandRegistryState {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinBlur,
        _: org_kde_kwin_blur::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct ActiveWaylandBlur {
    connection: Connection,
    manager: OrgKdeKwinBlurManager,
    blur: OrgKdeKwinBlur,
    surface: WlSurface,
}

static WAYLAND_BLUR: Mutex<Option<ActiveWaylandBlur>> = Mutex::new(None);

/// The raw pointers belong to GTK/Tauri and must remain valid for this call.
unsafe fn set_wayland_blur(
    display: *mut std::ffi::c_void,
    surface: *mut std::ffi::c_void,
    on: bool,
) -> bool {
    use wayland_backend::sys::client::{Backend, ObjectId};

    if display.is_null() || surface.is_null() {
        return false;
    }

    let Ok(mut active) = WAYLAND_BLUR.lock() else {
        return false;
    };

    if let Some(old) = active.take() {
        old.manager.unset(&old.surface);
        old.blur.release();
        let _ = old.connection.flush();
    }
    if !on {
        return true;
    }

    let backend = Backend::from_foreign_display(display.cast());
    let connection = Connection::from_backend(backend);
    let Ok(surface_id) = ObjectId::from_ptr(WlSurface::interface(), surface.cast()) else {
        return false;
    };
    let Ok(surface) = WlSurface::from_id(&connection, surface_id) else {
        return false;
    };

    let mut event_queue = connection.new_event_queue::<WaylandRegistryState>();
    let queue = event_queue.handle();
    let mut state = WaylandRegistryState::default();
    connection.display().get_registry(&queue, ());
    if event_queue.roundtrip(&mut state).is_err() {
        return false;
    }
    let Some(manager) = state.manager.take() else {
        return false;
    };

    let blur = manager.create(&surface, &queue, ());
    // A null region requests the whole surface, matching KDE's public
    // KWindowEffects::enableBlurBehind(window, true, QRegion()) contract.
    blur.set_region(None);
    blur.commit();
    // A non-blocking Wayland flush may report WouldBlock while GTK still owns
    // the display socket. Keep the live objects either way: GTK's event loop
    // will flush the queued request, and retaining them lets the later `off`
    // call reliably unset/release the effect.
    let _ = connection.flush();

    *active = Some(ActiveWaylandBlur {
        connection,
        manager,
        blur,
        surface,
    });
    true
}
