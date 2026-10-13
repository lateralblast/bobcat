# Bobcat

Version: 1.3.9

**B**MC **O**ut-of-band **B**rowser, **C**onsole **A**ccess **T**ool: a command line tool and a desktop app for
reaching a Dell iDRAC's consoles (graphical VNC, text over SSH or Serial over LAN) and for setting those services up.
Tested against an iDRAC8; the settings it changes are named in `CHANGELOG.md` and `RESEARCH.md`.

| Program | What it is |
|---|---|
| `bobcat-cli` | Command line: enables and disables iDRAC services, opens text consoles in your terminal, and shows the VNC console **as text**. |
| `bobcat-gui` | Desktop app (Tauri 2): the VNC console as a window, plus SSH and SOL terminals, on-screen keyboard, copy and paste. |

## bobcat-cli

```
bobcat-cli --ip <iDRAC IP> --user <username> [--check [--install]]
           [--console sol|ssh|vnc [--tls|--notls] [--relearn]]
           [--enable vnc|html5|java|sol [--port 5901] [--sslebl Disabled] [--timeout 300]] [--disable vnc]
```

**Credentials** are kept off the command line where possible. Omit `--pass` to be prompted, or set `BOBCAT_PASS`
(iDRAC password) / `BOBCAT_VNC_PASS` (VNC password). Add `--save` to keep the iDRAC login password (`--console sol`/`ssh`, `--enable`/`--disable`) and the VNC password (`--console vnc`) in the OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service); later runs use them without prompting, and `--forget` removes both for that IP. SSH is done in-process, so no credentials appear in any
process arguments (`--pass` and `--vncpass` do, if you use them).

**Configure the iDRAC** (over SSH, using `racadm`):

| Option | Effect |
|---|---|
| `--enable vnc` | Sets the VNC password (prompted, `--vncpass` or `BOBCAT_VNC_PASS`; 1-8 alphanumeric characters), port (`--port`), SSL encryption (`--sslebl`) and idle timeout (`--timeout`), and enables the VNC server. `--disable vnc` turns it off. |
| `--enable html5` / `java` | Enables the virtual console and selects the HTML5 or Java viewer. |
| `--enable sol` | Enables IPMI over LAN, Serial over LAN and serial redirection. BIOS changes are staged for the next reboot; `bobcat-cli` never reboots the server. |

**Consoles:**

| Option | What you get |
|---|---|
| `--console ssh` | The server's serial console through the iDRAC's SSH shell (`console com2`). Leave with `~.` at the start of a line, or `Ctrl+\`. |
| `--console sol` | The serial console over IPMI Serial over LAN via `ipmitool`. Leave with `~.`. |
| `--console vnc` | The graphical VNC console **as text**, with keyboard input. |

The server has to send its console to serial for `ssh` and `sol` (see `--enable sol`).

**How `--console vnc` works.** The screen is cut into character cells. Each distinct character shape is named once
by running `tesseract` over the screen and voting across all its occurrences, and every later refresh is read by
exact shape lookup, so it is quick and stable. It needs a text console with a fixed-size bitmap font (the Linux
console, BIOS text screens); graphical screens are reported as "no text console detected". TLS is on by default
(`--notls` for plain VNC); log in with the VNC password and leave with `~.` at the start of a line. The learned
character table is saved per iDRAC in `~/.cache/bobcat/glyphs.json` so later runs start faster (`--relearn`
starts over). `BOBCAT_ONCE=1` prints the screen once and exits (for scripts).

**Trust.** The iDRAC's SSH host key and TLS certificate are pinned the first time they are seen (trust on first
use, saved in `~/.config/bobcat/known_certs.json`, shared with `bobcat-gui`) and the connection is refused if they
change. `--trust-new-hostkey` accepts a changed key or certificate.

**Packages.** `bobcat-cli --check` verifies that `ipmitool`, `tesseract` and the GUI's libraries are installed;
add `--install` to install missing ones with apt (sudo prompts for its own password) or, on macOS, Homebrew.

## bobcat-gui

```
cd gui/src-tauri
BOBCAT_VNC_PASS='<vnc password>' cargo run
```

Requires the Tauri 2 Linux dependencies (WebKitGTK 4.1). No Node/npm needed. Enter the iDRAC IP and password
(prefilled from `BOBCAT_VNC_PASS`) and click Connect.

Choose the **Protocol**: VNC (default, the graphical console), **HTML5** (Dell's own web console), **Java** (Dell's Java console, in a window of its own), **SSH**
(the iDRAC's `console com2`) or **SOL** (IPMI Serial over LAN via `ipmitool`). HTML5, Java, SSH and SOL need the iDRAC username
and password; SSH and SOL show a terminal.
`BOBCAT_PROTOCOL=vnc|html5|java|ssh|sol`, `BOBCAT_USER` and `BOBCAT_PASS` set the starting values. The **Remember** checkbox keeps the password in the OS keychain (the iDRAC login for SSH, SOL, HTML5 and Java, shared with `bobcat-cli`; the VNC password for VNC); unticking it and connecting deletes the entry.

- **HTML5:** logs in to the iDRAC web interface (certificate checked against the pin first) and shows its HTML5
  console inside the main window, under the toolbar (a second web view; a small certificate window flashes up for
  about 3 seconds while the viewer sets itself up). Disconnect, closing the app, Ctrl+C or SIGTERM disconnect the
  viewer and log the session out. The HTML5 viewer must be selected on the iDRAC (`bobcat-cli --enable html5`).
  The console is Dell's own page, so its keyboard, mouse and menus are Dell's. Bobcat's **Copy** (reads the
  screen with OCR; needs `tesseract-ocr`), **Paste** (types your clipboard in, US layout) and **text size**
  (zoom) buttons work on it and sit on the second toolbar row beside Certificate; the on-screen keyboard is not used.
- **Java** (macOS and 64-bit Linux): logs in, downloads Dell's viewer from the iDRAC over the pinned connection and
  runs it with `java`, in a window of its own (it cannot be embedded). The Java viewer must be selected on the iDRAC
  (`bobcat-cli --enable java`). On macOS the viewer's native libraries are Intel-only, so it needs an x86_64 Java 8:
  the first time, bobcat asks and then downloads Azul Zulu 8 into `~/.config/bobcat/java` (checksum checked, nothing
  installed system-wide; Rosetta 2 is needed on Apple Silicon). `BOBCAT_JAVA=<path to java>` uses another runtime
  (on Linux `java` from the PATH is used). Virtual media is off (`BOBCAT_JAVA_VM=1` keeps what the iDRAC asks for).
  The viewer checks the iDRAC's certificate itself and warns when it does not chain to a known root. On macOS bobcat
  sets the viewer's "User's Certificate Store Path" (unless you have set your own) to `~/.config/bobcat/java-trust` and
  puts the iDRAC's certificate there once bobcat has checked it against its pin (trust on first use), so the viewer's
  warning does not appear; a changed certificate is refused by bobcat and never added.
  Close the viewer's window or press Disconnect to end the session; the viewer's output is in
  `~/.config/bobcat/java-viewer/<ip>/viewer.log`. The viewer's own warning that its keyboard library failed to load
  is harmless (typed text still works; some keys and Single Cursor mode do not).
- **VNC:** click the screen to send keyboard and mouse input (Ctrl+Alt+Del button provided). Compressed Zrle
  updates are requested; set `BOBCAT_ENCODING=raw` to force uncompressed Raw.
- **TLS:** if VNC SSL encryption is enabled on the iDRAC (`bobcat-cli --enable vnc --sslebl Auto_Negotiate`),
  tick **TLS** before connecting (or set `BOBCAT_TLS=1`). The iDRAC takes 10-15 seconds to complete the handshake.
  Its self-signed certificate is saved the first time you connect and a changed one blocks the connection; the
  **Certificate** button (**Host key** for SSH) shows it and can trust a new one.
- **Keyboard:** the on-screen keyboard has US/UK/DE/FR layouts, can be dragged by its title bar, and works in every
  protocol (click Shift/Ctrl/Alt/Meta first for combinations).
- **Paste** (button or Ctrl+Shift+V): types your clipboard into the console (US layout) or pastes into the terminal.
- **Copy text:** in VNC, drag over the text on screen to read it with OCR (needs `tesseract-ocr`; best on plain text
  consoles); in SSH/SOL it copies the selection, or the whole visible screen.
- **Text size:** the drop-down zooms the console; **Fit window** scales it to the window.
- Closing the window (or Ctrl+C) hangs up SSH and SOL sessions cleanly so the iDRAC releases them.

## Files and variables

| | |
|---|---|
| `~/.config/bobcat/known_certs.json` | Pinned TLS certificates and SSH host keys (both programs). |
| `~/.cache/bobcat/glyphs.json` | Learned character table for `--console vnc`. |
| `BOBCAT_*` | `PASS`, `VNC_PASS`, `USER`, `PROTOCOL`, `TLS`, `AUTOCONNECT`, `ENCODING`, `ONCE`, `DUMP`, `DEBUG` (`1`, or `2` for bytes). |

This project used to be called dracon. The old `~/.config/dracon-gui` and `~/.cache/dracon-cli` files are still read (and
copied to the new locations); the old `DRACON_*` environment variables are no longer used, only `BOBCAT_*`.

## License

Licensed under CC BY-NC-SA 4.0. See [LICENSE.md](LICENSE.md).

## Help Support Development

If you find this software useful and would like to support its development, please consider buying me a coffee:

https://ko-fi.com/richardatlateralblast

## Third-party

`gui/ui/vendor/` contains xterm.js 5.5.0 and its fit addon (MIT licence, see `gui/ui/vendor/xterm-LICENSE`).
