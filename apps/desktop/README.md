# MyCord desktop shell

The installed call app is still `native-sender`. This directory is the new window around that same engine. It is not the app to hand to a friend until a real call has been tested here.

## Decision

Voice and the screen stay in the Rust process. The webview draws the room screen and the buttons. It does not capture the microphone, encode the screen, or receive picture frames. LiveKit and the direct picture path carry the media.

The Next.js site stays the browser product. It is not packaged into this window, because that page shares a screen with the browser capture API and talks to the old remote sender launcher.

A second process, and `cargo run` from the API, are not how the desktop app starts capture. The engine is the `native-sender` library linked into this binary. On a production API, the remote launcher is off unless `ENABLE_REMOTE_NATIVE_RUNTIME=1`.

## Run on this Mac

```bash
cd apps/desktop
npm install
npm run tauri dev
```

The current MyCord.app is unchanged. Open that for a call until this shell has been tried on both computers.

## Package

```bash
cd apps/desktop
npm run tauri build
```

That command has not been run. A Mac build that other people can open still needs signing and notarisation, which are not set up in this repo. Windows packaging has not been built. WebView2 is the Tauri runtime on Windows; the picture surface for that window is not implemented, so Windows watching still belongs to the existing MyCord.exe.

## What was run

- `cargo test --lib` in `apps/native-sender`: 28 passed.
- `cargo check --bin native-sender`: passed.
- `cargo check` for this shell: passed.
- Typecheck for this UI and for `apps/api`: passed.
- `npm run tauri dev` on this Mac: the window opened on the room screen.

Not tested: joining a room, microphone, screen sharing, the Metal stage during a call, leave/reconnect, sleep, or any Windows build of this shell.
