# DeckCraft for Android

DeckCraft on Android tablets and phones: the same app as on the desktop (the ribbon, themes,
layouts, shapes, pictures, charts, transitions and animations, `.pptx` in and out, the slide
show), running natively. It is not a web page in a wrapper.

> **Community port, early preview.** DeckCraft itself is in early development. Keep copies of
> presentations that matter.

## Install

Open `DeckCraft-<version>-arm64.apk` on the tablet and allow the install when Android asks.
Needs Android 8.0 or newer on a 64-bit ARM device.

## Using it with fingers

| Gesture | Does |
|---|---|
| Tap | Click: select a slide, a shape or a button |
| Double tap a text box | Edit its text (the on-screen keyboard appears) |
| Swipe | Scroll the slide list, the slide or a gallery; a flick keeps coasting |
| Long press, then drag | Move a shape, or select text |
| Two fingers apart / together | Zoom |
| Two-finger tap | Right-click (the context menu) |
| Back button | Escape: closes a menu, a dialog or File view, ends a slide show |

During a slide show, tap or swipe to move on; the status and navigation bars are hidden (swipe in
from the edge to see them for a moment). A hardware keyboard works as on the desktop.

## Files

- **Open** (File › Open, Insert › Pictures, Audio, Video) shows Android's file picker. **Open
  with DeckCraft** from a file manager and another app's **Share** also work.
- **Save** of a `.pptx` or `.deckcraft` opened from the picker writes back to that file: DeckCraft
  edits a private working copy and copies it over the original after each save, but never over an
  original another app changed meanwhile (it says so; Save As keeps your changes as a new file).
- A new presentation, a shared file and **Save As** ask where with Android's save picker; after
  Save As the presentation takes the name and file you chose. **Export** (PDF, pictures, outline)
  also asks where.
- **Print** uses Android's print dialog. **Copy and Paste** share the system clipboard.
- Settings and recovery files stay in the app's private storage: if Android closes the app in the
  background, unsaved presentations come back when it reopens.
- Media plays without sound for now, and Reuse Slides needs the desktop app.

## Building

`build-apk.sh` builds the APK with the Android SDK's own tools (no Gradle). The fonts come from
[craft-fonts](https://github.com/storytold/craft-fonts) (`CRAFT_FONTS_DIR`, cloned into `target/`
when unset), without the large CJK faces Android already has.

```sh
rustup target add aarch64-linux-android
export ANDROID_HOME=~/Android/Sdk          # with build-tools, platforms;android-35 and an NDK
apps/deckcraft-android/build-apk.sh        # -> dist/android/DeckCraft-<version>-arm64.apk
adb install -r dist/android/DeckCraft-*-arm64.apk
adb logcat -s DeckCraft                    # the app's log
```

The first build creates a signing key in `apps/deckcraft-android/keys/` (not committed). Keep it:
Android installs an update only over a build signed with the same key.

## How it fits together

| Piece | Job |
|---|---|
| `src/lib.rs` | `android_main`, the entry point NativeActivity calls |
| `src/shell.rs` | storage folders, `Services` (open, save, print, clipboard), settings, recovery, the keyboard, full screen for shows |
| `src/workcopies.rs` | working copies of presentations opened from storage, and writing them back (tested on the desktop) |
| `src/touch.rs` | finger gestures → clicks, scrolling and drags (tested on the desktop) |
| `src/keys.rs` | on-screen keyboard keys → egui keys and modifiers (tested on the desktop) |
| `src/mailbox.rs` | the inbox the Java side answers through, in order (tested on the desktop) |
| `src/bridge.rs` | JNI calls into the activity (the only `unsafe` in the port) |
| `android/java/…/DeckCraftActivity.java` | `NativeActivity` subclass: pickers, saving, writing back, printing, clipboard, full screen, the keyboard's text field, window insets |
| `winit-android-modifiers.patch` | applied to winit for the Android build only: reports Shift/Ctrl/Alt, which winit 0.30 never does on Android |
| `android/AndroidManifest.xml` | the app, its launcher entry and the file types it opens |
