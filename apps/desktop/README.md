# MyCord Shell

This is the desktop window around the existing Rust call engine. Voice, capture, and the screen stay in that process. The window only sends join, leave, mute, share, and the picture settings.

The app you already use, `MyCord.app` and `MyCord.exe`, is unchanged. This build is the one to try next.

## Try it on this Mac

Quit the current MyCord on the Mac first, so two apps do not join as the same person.

Open `~/Desktop/MyCord Shell.app`.

The room screen is filled from `~/.mycord` when that file exists. Use a different display name from the other computer, and the same room name. On Windows, keep using the existing `MyCord.exe` for this test. The Windows build of this shell does not draw the screen yet.

The first launch asks for the microphone. The first share asks for screen recording. Both prompts belong to MyCord Shell, which is a different app from MyCord, so macOS asks again.

Leave, or quit the app, stops the call. The picture is drawn in a native surface over the stage. It is not sent through the webview.

## Build again

```bash
cd apps/desktop
npm install
npm run tauri build
```

The app is written to:

`apps/native-sender/target/release/bundle/macos/MyCord Shell.app`

That path is used because the build reuses the engine's existing release output. A copy for the Desktop is:

```bash
ditto "apps/native-sender/target/release/bundle/macos/MyCord Shell.app" "$HOME/Desktop/MyCord Shell.app"
codesign --force --sign - "$HOME/Desktop/MyCord Shell.app"
```

This Mac build is Apple Silicon only. It is ad-hoc signed, so it is for this computer. It is not notarised.

## What was run

- `cargo test --lib` in `apps/native-sender`: 28 passed.
- `npm run tauri build` on this Mac: the app bundled and launched to the room screen.
- The first Join aborted inside WebRTC because the shell link dropped `NSString(StdString)`. The desktop link now passes `-ObjC`, the same flag the working app uses, and that category is in the binary.

Not tested after that link fix: a completed call in this window, microphone audio, screen sharing, the picture during a call, leave during a call, reconnect, sleep, or any Windows build of this shell.
