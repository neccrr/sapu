//! macOS "liquid glass": the system blur behind Sapu's window.
//!
//! The blur view (NSVisualEffectView) is inserted into the window's frame
//! view **below** the content view that Sapu draws into, so it can only ever
//! show *behind* the app. (v0.7 put it on top of the content, which covered
//! everything and left the window blank.)
//!
//! Opt-in for now: start Sapu with `SAPU_GLASS=1`.

/// Is glass requested for this run?
pub fn requested() -> bool {
    cfg!(target_os = "macos") && matches!(std::env::var("SAPU_GLASS").as_deref(), Ok("1" | "true" | "on"))
}

#[cfg(target_os = "macos")]
pub fn install(cc: &eframe::CreationContext<'_>) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
        NSVisualEffectView, NSWindowOrderingMode,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let handle = cc.window_handle().map_err(|e| e.to_string())?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return Err("not an AppKit window".into());
    };
    // SAFETY: winit gives us a valid NSView pointer for the lifetime of the window.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let window = view.window().ok_or("view has no window")?;
    let content = window.contentView().ok_or("window has no content view")?;
    // SAFETY: plain AppKit getter on the main thread.
    let frame_view = unsafe { content.superview() }.ok_or("content view has no superview")?;

    let effect = NSVisualEffectView::initWithFrame(mtm.alloc(), frame_view.bounds());
    effect.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::FollowsWindowActiveState);
    effect.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    // Below the content view: behind Sapu, never on top of it.
    frame_view.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, Some(&content));
    Ok(())
}
