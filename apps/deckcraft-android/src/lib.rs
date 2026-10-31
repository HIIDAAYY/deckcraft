//! DeckCraft on Android phones and tablets.
//!
//! The same [`deckcraft_ui_egui::SlideApp`] as the desktop and web apps, run by eframe's
//! NativeActivity runner (wgpu: Vulkan, or OpenGL ES where Vulkan is missing). Android loads this
//! library (`libdeckcraft_android.so`) into `DeckCraftActivity`, a small `NativeActivity` subclass
//! (`android/java/…`) that adds what NDK code can't do alone: the system file pickers, sharing the
//! clipboard, printing, links, full screen for slide shows and the on-screen keyboard's text.
//!
//! Differences from the desktop app, much like the browser's (`apps/deckcraft-web`):
//! - File › Open and Insert › Pictures, Audio and Video use Android's document picker; picked
//!   files arrive through the app's inbox ([`mailbox`]). Opening a presentation from a file
//!   manager or another app's Share/Open with does the same.
//! - A presentation opened from storage that DeckCraft may write back to is edited in a private
//!   working copy ([`workcopies`]): Save writes the copy, and the shell then copies it over the
//!   original, never over one that changed elsewhere meanwhile. Save As and exports go through
//!   Android's save picker; after Save As the presentation takes the new file.
//! - Print uses Android's print dialog; a slide show hides the status and navigation bars.
//! - The on-screen keyboard types into the activity's own text field, which passes text and keys
//!   through the inbox in order ([`keys`]); the app keeps clear of the system bars and keyboard.
//! - Fingers: tap clicks, swipe scrolls, long press then drag moves or selects, two fingers
//!   pinch-zoom, and a two-finger tap opens the context menu ([`touch`]). Back is Escape.
//! - Settings and recovery files live in the app's private storage.
//!
//! Build the APK with `apps/deckcraft-android/build-apk.sh` (see the README beside it).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod keys;
pub mod mailbox;
pub mod touch;
pub mod workcopies;

#[cfg(target_os = "android")]
mod bridge;
#[cfg(target_os = "android")]
mod shell;

/// The entry point android-activity's glue calls on its own thread once the activity starts.
// SAFETY: `android_main` must be exported unmangled: the android-activity glue looks it up by
// this exact name. Nothing else in the process defines it.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    shell::run(app);
}
