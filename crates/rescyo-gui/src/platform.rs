//! The window material per platform (ADR-0006, docs/design "Materials per platform"): system
//! glass on the frame (sidebar) only, opaque surfaces under the data, and solid everywhere when
//! the system asks for reduced transparency or has no material.
//!
//! macOS 26+: Liquid Glass (`NSGlassEffectView`) behind the transparent sidebar; macOS 11–15:
//! `NSVisualEffectView` with the sidebar material. The window gets a transparent, hidden title
//! bar with a full-size content view, so the traffic lights sit inside the sidebar.
//! Other platforms: solid tokens.

use slint::ComponentHandle;

use crate::{Look, MainWindow};

/// Chooses the backend before any window exists: on macOS the GPU renderer, which can draw a
/// transparent window (the software renderer's surface there is always opaque).
pub fn select_backend() {
    #[cfg(target_os = "macos")]
    {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
        let r = slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("femtovg".into())
            .with_winit_window_attributes_hook(|a| {
                a.with_titlebar_transparent(true)
                    .with_title_hidden(true)
                    .with_fullsize_content_view(true)
            })
            .select();
        if let Err(e) = r {
            eprintln!("rescyo-gui: GPU renderer unavailable ({e}); using solid materials");
        }
    }
}

/// Whether the system prefers a dark appearance (dark is the design default).
pub fn prefers_dark() -> bool {
    match std::env::var("RESCYO_THEME").as_deref() {
        Ok("light") => return false,
        Ok("dark") => return true,
        _ => {}
    }
    #[cfg(target_os = "macos")]
    {
        mac::prefers_dark()
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

/// Sets the look before the window is shown (transparency is decided at creation).
pub fn prepare(ui: &MainWindow) {
    let look = ui.global::<Look>();
    look.set_mac(cfg!(target_os = "macos"));
    #[cfg(target_os = "macos")]
    look.set_glass(mac::glass_wanted());
}

/// Puts the platform material behind the frame once the native window exists. Returns true
/// when there is nothing (more) to do.
pub fn dress(ui: &MainWindow) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::cell::Cell;
        thread_local!(static DONE: Cell<bool> = const { Cell::new(false) });
        if DONE.with(Cell::get) || !ui.global::<Look>().get_glass() {
            return true;
        }
        match mac::dress(ui.window(), 236.0) {
            Some(true) => {
                DONE.with(|d| d.set(true));
                true
            }
            // The window exists but has no place for the material: fall back to solid.
            Some(false) => {
                ui.global::<Look>().set_glass(false);
                true
            }
            None => false,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ui;
        true
    }
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "AppKit calls through objc2; each block states its invariant"
)]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{ClassType, MainThreadOnly, msg_send};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
        NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode, NSWorkspace,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSUserDefaults};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;

    pub fn prefers_dark() -> bool {
        let defaults = NSUserDefaults::standardUserDefaults();
        let style = defaults.stringForKey(&NSString::from_str("AppleInterfaceStyle"));
        style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
    }

    /// Glass unless the person asked the system to reduce transparency.
    pub fn glass_wanted() -> bool {
        std::env::var_os("RESCYO_SOLID").is_none()
            && !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
    }

    /// Inserts the material view behind the content view, under the sidebar strip.
    pub fn dress(window: &slint::Window, sidebar: f64) -> Option<bool> {
        window.with_winit_window(|w| {
            let Ok(handle) = w.window_handle() else {
                return false;
            };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                return false;
            };
            // SAFETY: winit hands out the NSView of a live window on the main thread; the
            // pointer stays valid for the duration of this callback.
            let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
            let Some(win) = view.window() else { return false };
            let Some(frame_view) = (unsafe { view.superview() }) else {
                return false;
            };
            win.setOpaque(false);
            win.setBackgroundColor(Some(&NSColor::clearColor()));
            let height = view.frame().size.height;
            let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(sidebar, height));
            let Some(material) = glass_view(rect).or_else(|| visual_effect_view(rect)) else {
                return false;
            };
            material.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewHeightSizable | NSAutoresizingMaskOptions::ViewMaxXMargin,
            );
            frame_view.addSubview_positioned_relativeTo(&material, NSWindowOrderingMode::Below, Some(view));
            true
        })
    }

    /// macOS 26 Liquid Glass, when the system has it.
    fn glass_view(rect: NSRect) -> Option<Retained<NSView>> {
        let cls: &AnyClass = AnyClass::get(c"NSGlassEffectView")?;
        // SAFETY: NSGlassEffectView is an NSView subclass; alloc/initWithFrame: is its
        // designated initialiser, and the result is retained by `Retained`.
        unsafe {
            let obj: *mut AnyObject = msg_send![cls, alloc];
            let obj: *mut AnyObject = msg_send![obj, initWithFrame: rect];
            let view: Option<Retained<NSView>> = Retained::from_raw(obj.cast::<NSView>());
            view
        }
    }

    /// macOS 11–15: the sidebar material, blending with what is behind the window.
    fn visual_effect_view(rect: NSRect) -> Option<Retained<NSView>> {
        let mtm = objc2::MainThreadMarker::new()?;
        let v = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect);
        v.setMaterial(NSVisualEffectMaterial::Sidebar);
        v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        v.setState(NSVisualEffectState::Active);
        Some(Retained::into_super(v))
    }

    #[allow(dead_code, reason = "keeps ClassType in scope for the generated alloc calls")]
    fn _uses(_: &dyn Fn() -> &'static AnyClass) {
        let _ = NSView::class();
    }
}
