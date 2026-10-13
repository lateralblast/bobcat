# Research

Notes and findings that don't belong in TODO.md or README.md.

## iDRAC virtual console options
- iDRAC VNC server: TCP 5901, off by default, 8-char password. Enable via UI (Configuration > Virtual Console) or `racadm set iDRAC.VNCServer.Enable Enabled`. Untested.
- Rust `vnc` crate 0.4.x implements an RFB client; auth/encoding support against iDRAC unverified.
- iDRAC9 firmware >= 6.00.02.00: console is eHTML5 over 443 only; no public protocol spec or Rust implementation found.

## VNC on iDRAC (verified 0.0.9)
- iDRAC VNC identifies as "OpenVNC", 1024x768; `vnc` 0.4 crate handshake with VNC password auth works; incremental updates arrive (small repeated rect at 192,766 32x2).
- iDRAC seems to allow a single VNC client; a second concurrent connection fails the handshake ("failed to fill whole buffer").
- Headless (Xvfb) screenshots of the Tauri window came out black, so rendering was not confirmed that way.

## iDRAC VNC encryption (probing 0.2.4)
- `racadm set iDRAC.VNCServer.SSLEncryptionBitLength` only accepts numbers: 0 Disabled, 1 Auto Negotiate, 2 128-Bit or higher, 3 168-Bit or higher. Names are rejected (RAC947). racadm over SSH exits 0 on errors; check output for "ERROR". `;` can't chain commands.
- RESOLVED: with encryption on (Auto Negotiate), port 5901 speaks TLS from the first byte (stunnel-style), TLS 1.2, cipher AES256-GCM-SHA384 (static RSA key exchange, so rustls can't be used; `openssl` crate needed), self-signed Dell cert, and the server takes ~10-12s to answer the ClientHello (earlier probes timed out at 8-10s and wrongly saw silence). After the handshake the normal `RFB 003.008` greeting follows. Python needs `ALL:@SECLEVEL=0` to negotiate it.
- SSVNC (SSL mode) connected then exited right after the handshake in the packet capture; cause not investigated.
- iDRAC8 firmware 2.86.86.86, BIOS 2.19.0.

## Encodings (0.4.0)
- iDRAC8 VNC native pixel format: 16bpp, depth 15, RGB555, little endian. Supports Zrle (full 1024x768 as 64x64 tiles in ~0.3s over TLS vs ~2.3s Raw 32bpp).
- `vnc` 0.4 crate decodes only Raw, CopyRect, Zrle, Cursor, DesktopSize (Hextile/RRE return an error). `Client::set_format` hangs when updates arrive as tiles (it waits for one full-screen rect), so switch format first (while still on Raw), then enable Zrle. Using the server's advertised native 16-bit format produced wrong colours (light blue/cyan), so force 32-bit BGRX instead.

## HTML5 console (0.8.0)
- iDRAC8: `iDRAC.VirtualConsole.PluginType` 2 = HTML5 (the names `HTML5` and `Java` are also accepted: Java reads back as 1, HTML5 as 2; 0 is ActiveX). `iDRAC.VirtualConsole.Enable` must be Enabled. Console port 5900, Timeout 1800, MaxSessions 6.

## Serial over LAN (0.9.1)
- iDRAC8 attributes: `iDRAC.IPMILan.Enable`, `iDRAC.IPMISOL.{Enable,BaudRate,MinPrivilege}` (MinPrivilege 4 = Administrator), `iDRAC.Serial.Enable`, `iDRAC.SerialRedirection.Enable`; BIOS `BIOS.SerialCommSettings.{SerialComm,SerialPortAddress,RedirAfterBoot,FailSafeBaud,ConTermType}`. Test server already had SerialComm=OnConRedirCom2, SerialPortAddress=Serial1Com2Serial2Com1, RedirAfterBoot=Enabled, FailSafeBaud=115200, ConTermType=Ansi, IPMISOL baud 115200, iDRAC.Serial baud 57600.
- BIOS attribute changes are staged and need `racadm jobqueue create BIOS.Setup.1-1` plus a reboot to apply.

- `ssh root@idrac console com2` (PTY + exec `console com2`) prints "Connected to Serial Device 2. To end type: ^\" (quit key set by iDRAC.SerialRedirection.QuitKey). It coexisted with an active ipmitool SOL session. SOL via `ipmitool -I lanplus` works with the default cipher suite (17) on iDRAC8 2.86.86.86.

## Serial console redraw scrolling (0.9.9)
- The TrueNAS console menu (prompt_toolkit) asks the cursor row with ESC[6n, then prints (H - row) newlines and moves up that many lines, where H = 66 is the height the remote tty believes (independent of our pty size: tested with 30, 50 and 70 rows). With a reported row of 66 or more it prints none and redraws in place. A terminal shorter than 66 rows therefore scrolls on each redraw; a tall terminal (as in the CLI) does not. Reproduced with a pty harness that answers ESC[6n with a chosen row (14 -> 52 newlines, 40 -> 26, >=66 -> none).
- The remote also double-translates newlines (`\r\r\n`), which is harmless.

## Screen to text (1.0.5)
- iDRAC8 VNC shows the Linux console as 1024x768 with an 8x16 bitmap font (128x48 cells, 3 colours, no anti-aliasing), so every character is one of ~50 repeated shapes.
- Cell size detection: for each candidate size count distinct non-empty cell shapes; the true size gives a tiny ratio (47 shapes / 1941 cells = 0.024). Divisors of the true size (8x8, 4x8) also score well, so pick the LARGEST candidate with ratio <= 0.08.
- Whole-screen OCR with word boxes mis-aligned columns (drwxr-xr-x came out as drrxr-xr-x). What works: stack only the non-empty rows with a full row of white between them (one row = one OCR line), accept a line only if tesseract returned exactly as many characters as there are non-blank cells in that row, vote per shape across lines and several scales, then assign characters to shapes one-to-one by descending votes (a real `8` has its own shape and wins the 8, leaving the 0 shape its 0).
- Image scaling matters: nearest-neighbour upscaling halves the accepted lines and reads 0 as @; bilinear (3x, 4x, 5x) matches Lanczos. A gap of half a row between rows confused 0/@ again; a full row gap fixed it. tesseract accepts a plain binary PGM on stdin.
- A render taken mid-scroll leaves fragments of the old text; after the picture settles (250ms) ask for one full non-incremental update and render that.
- Calibration takes ~5s (3 tesseract runs in parallel); shapes a calibration could not name (a glyph clipped at the screen edge) are given up on so they don't trigger more runs.
- The iDRAC sends no RFB greeting to a plain client when VNC encryption is on, so `--notls` would hang without a read timeout.
- Digit confusion (1.0.7): on the 8x16 Linux console font tesseract reads the `0` shape as 6 (62 votes), @ (27), 8 (25) and 0 (20); the real `6` and `8` shapes claim their own characters first, so the zero shape gets whatever is left, and by raw votes that was `@` on one screen and `0` on another. Scoring each (shape, character) as share-of-the-shape's-votes x a rough character-frequency prior (@ rare, 0 common) picks 0 on both and leaves @ for the prompt's shape.

## iDRAC8 HTML5 console (1.1.0 - 1.1.1)
- The iDRAC web server serves static pages and files (start.html, login.html, js/...) only to clients that send `Accept-Encoding: gzip`; otherwise 404. Data endpoints (`/data/login`, `/session?...`) work without gzip.
- Login: `POST /data/login` with `user=...&password=...` (urlencoded) returns XML with `authResult` (0 ok, 1 wrong password, 5 no free session) and `forwardUrl` = `index.html?ST1=<t1>,ST2=<t2>`, plus the `-http-session-` cookie (secure, httponly). Login takes ~5s; other requests are fast. `/session?...` calls need the header `ST2: <t2>`. `/data/logout` ends the session (303).
- Console URL (from LaunchConsole.jsesp): `https://<ip>/virtualconsolehtml5.html?ipAddr=<ip>&kvmPort=<rp_int_apcp_port, 5900>&vmPriv=<true|false>&lang=en&aimSession=<getSsnVar aimSession>&ST2=<t2>&TokenName=ST1&TokenKey=<t1>`. Requires `gui_kvm_plugin_type` = 2 (HTML5), `vkvm_bool_enabled` = 1 and `accessConsoleRedirection` = 1.
- The viewer page (virtualconsolehtml5.html) reads `window.opener.top.localeObj` (the language JSON `/locale/locale_en.json`, whose inner `localeString` object is what the web UI assigns to `localeObj`) and sets `window.opener.top.htmlViewerWindow = null`, so it only works as a pop-up of the web UI unless an `opener` is injected before its scripts run. With the stand-in opener the page opens two WebSocket connections to port 443 (about 15 KB and 5.5 KB received for a static screen).
- The web certificate on 443 is the same certificate as on the VNC port 5901, so one pin covers both.
- Headless WebKit (Xvfb, no window manager or GPU) paints nothing, so the console cannot be seen in automated tests; connections and logout were checked on the iDRAC side (`racadm getssninfo`).
- The viewer's first step (rpviewer.js `RP_API_ERROR.POPUP_BLOCKED`) is a certificate dialog: `window.open("https://<ip>:<kvmPort>/<page>", canvasId, "width=700,height=500,...")`, then every 250 ms `popup.postMessage("Hello","*")`; when a `message` event arrives whose origin equals `https://<ip>:<kvmPort>` it proceeds to open its WebSockets. If `window.open` returns null it reports POPUP_BLOCKED ("Turn off your pop-up blocker and try again!"). Port 5900 serves HTTPS ("WS server test page") with the same certificate as 443. The window must therefore be allowed and keep its `opener`.
- WebKitGTK blocks `window.open()` that is not user-initiated by default (`javascript-can-open-windows-automatically` is false) and returns null without emitting the `create` signal, so the app's `on_new_window` handler is never called; the viewer opens its certificate window from its load script, so the setting must be enabled for the console window.
- Killing the console window without running the viewer's `disconnectKVM()` (which calls `exitViewer()` and sends the close-connection messages on the primary and video sockets) leaves the iDRAC's "Virtual Console" session (racadm getssninfo) listed for at least a minute after the sockets are gone. The page's Disconnect Viewer button calls `disconnectVM(); disconnectKVM();`.

## Embedding the HTML5 console (1.1.7)
- Tauri 2 can host a second web view in a window (`Window::add_child`, `unstable` feature). On Linux (`build_gtk` into the window's `default_vbox`) the child is packed into the vertical box and its position is ignored; positioning only works under a `gtk::Fixed` parent. So layout is done in GTK: the child expands, the main page is wrapped in a fixed-height `gtk::EventBox` at the top.
- A size request on the main WebKitWebView is only a minimum: its natural height stays "the whole window" (preferred height min 112, natural 800), so with expand=false it still took 800px and left the console 1px. Wrapping it in an EventBox with `set_size_request(-1, h)` works (EventBox preferred height = the request).
- GTK allocates widget sizes even on Xvfb, so layout can be verified headless by logging `allocation()`; painting cannot.

## Controlling the embedded HTML5 console from outside (1.1.9)
- Dell's viewer registers `keydown`, `keypress` and `keyup` listeners on `window` (and `blur`, `visibilitychange`); it maps `event.keyCode` / `event.code` to USB keys. Dispatching `new KeyboardEvent(...)` on `window` with `keyCode`/`which` defined on the event types text into the console (a Shift keydown/keyup around shifted characters, 12ms between characters). Verified on the TrueNAS console.
- The page loads Prototype.js, which changes `Array.prototype`: `Array.prototype.map.call(HTMLCollection, ...)` fails ("this.each is not a function") and `JSON.stringify([...])` is double-encoded (a quoted string). Use `forEach` loops and `join(',')`.
- `webview.set_zoom(f)` zooms the whole page (Dell's buttons too); the canvas then overflows and scrolls.
- `WebViewExt::snapshot(SnapshotRegion::Visible, ...)` returns a cairo surface even on Xvfb, so a headless test can see the console; WebViewExt::run_javascript (deprecated since WebKit 2.40) returns a script's value. Commands that wait for these must not run on the main thread (use `async` commands).
- tesseract on the console canvas: bilinear 4x of the inverted grayscale reads `root@truenas[/]# a` exactly; cairo's own 3x scaling read `[/]#` as `(7)`; Lanczos 3x added a space.
