//! Electron's `dialog.ask` is a system dialog. Linux GPUI currently falls back
//! to an in-window prompt; use the existing GTK runtime there instead.
use gpui::{App, Window};

#[cfg(not(target_os = "linux"))]
pub fn ask(
    window: &mut Window,
    cx: &mut App,
    title: String,
    message: String,
    cancel: String,
    close: String,
) -> anyhow::Result<impl Future<Output = bool> + use<>> {
    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        &title,
        Some(&message),
        &[
            gpui::PromptButton::cancel(cancel),
            gpui::PromptButton::ok(close),
        ],
        cx,
    );
    Ok(async move { answer.await.is_ok_and(|index| index == 1) })
}

#[cfg(target_os = "linux")]
pub fn ask(
    window: &mut Window,
    cx: &mut App,
    title: String,
    message: String,
    cancel: String,
    close: String,
) -> anyhow::Result<impl Future<Output = bool> + use<>> {
    use gtk::prelude::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let parent = match HasWindowHandle::window_handle(window)?.as_raw() {
        RawWindowHandle::Xcb(h) => u64::from(h.window.get()),
        RawWindowHandle::Xlib(h) => h.window,
        _ => {
            anyhow::bail!("A system confirmation dialog is not available for this window backend.")
        }
    };
    gtk::init().map_err(|error| anyhow::anyhow!("Cannot open system confirmation: {error}"))?;
    let display = gtk::gdk::Display::default()
        .and_then(|display| display.downcast::<gdkx11::X11Display>().ok())
        .ok_or_else(|| anyhow::anyhow!("System confirmation requires an X11 display"))?;
    let foreign = gdkx11::X11Window::foreign_new_for_display(&display, parent as _);
    let dialog = gtk::MessageDialog::builder()
        .title(title)
        .text(message)
        .message_type(gtk::MessageType::Warning)
        .modal(true)
        .build();
    dialog.add_button(&cancel, gtk::ResponseType::Cancel);
    dialog.add_button(&close, gtk::ResponseType::Accept);
    dialog.set_default_response(gtk::ResponseType::Accept);
    dialog.realize();
    if let Some(window) = gtk::prelude::WidgetExt::window(&dialog) {
        window.set_transient_for(foreign.upcast_ref());
    }
    let (send, receive) = async_channel::bounded(1);
    dialog.connect_response(move |dialog, answer| {
        let _ = send.try_send(answer == gtk::ResponseType::Accept);
        dialog.hide();
    });
    dialog.show_all();
    dialog.present();
    struct OpenDialog(gtk::MessageDialog);
    impl Drop for OpenDialog {
        fn drop(&mut self) {
            unsafe {
                self.0.destroy();
            }
        }
    }
    let guard = OpenDialog(dialog);
    let executor = cx.background_executor().clone();
    Ok(async move {
        // Only an open dialog pumps GTK. Do not introduce idle frame polling.
        let _guard = guard;
        loop {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            match receive.try_recv() {
                Ok(answer) => return answer,
                Err(async_channel::TryRecvError::Closed) => return false,
                Err(async_channel::TryRecvError::Empty) => {}
            }
            executor.timer(std::time::Duration::from_millis(16)).await;
        }
    })
}

pub use ask as close_terminal;
