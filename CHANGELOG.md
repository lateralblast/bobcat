# Changelog

All notable changes are tracked here. Versioning: no digit exceeds 9 (0.0.9 -> 0.1.0).

## [1.5.9] - 2026-10-14
### Added
- `bobcat-cli --disconnect sol`: closes the sessions the iDRAC8 lists as type `SOL` by session ID, like `gui`, `vc` and `ssh`. Real `getssninfo` listings from two iDRAC8s show both an ipmitool Serial over LAN session (`--console sol`) and a serial console opened over SSH (`console com2`, `--console ssh`) as `SOL`, so one option covers both. Not run on the iDRAC yet.

## [1.5.8] - 2026-10-14
### Added
- `bobcat-cli --disconnect list|gui|vc|ssh`: `list` prints `racadm getssninfo`; `gui` (web interface), `vc` (alias `console`; Virtual Console) and `ssh` close the sessions of that type one by one with `racadm closessn -i <id>`, showing each (`closing session 327 (Virtual Console, user root)`). The session table is read from `racadm getssninfo` (fixed-width: `SSNID Type User IP Date`; the type may contain a space), the format taken from an iDRAC8's real output. `ssh` leaves the session it runs in (racadm cannot close it).
- `--disconnect media` also closes any session the iDRAC lists as Virtual Media, after `vmdisconnect` and `remoteimage -d`.
### Notes
- The parser was checked against the real `getssninfo` output; closing by type has not been run on the iDRAC yet. Other session types the iDRAC lists (IPMI, Redfish, WSMAN, Remote Racadm...) are only reachable with `sessions` (all) until their exact names are known.

## [1.5.7] - 2026-10-14
### Added
- `bobcat-cli --disconnect media|iso|vm|sessions|all` (comma list allowed): ends virtual media or sessions on the iDRAC over SSH. `media` runs `racadm vmdisconnect` (an image a console mapped) and `racadm remoteimage -d` (a network remote image); `sessions` runs `racadm closessn -a` (every other session); `all` does both, media first. Each command's answer is shown (`[ok]` / `[--]`); a command with nothing to do is not an error. Needs the iDRAC login like `--enable`, so the keychain, `--pass` or the prompt apply.
### Notes
- Argument parsing was checked; the racadm commands themselves have not been run against an iDRAC yet.

## [1.5.6] - 2026-10-14
### Fixed
- GUI HTML5 console: disconnecting (Disconnect, closing the window, switching protocol) while an image was still mapped left the iDRAC's Virtual Media session behind, and the next console then said "Virtual Media is currently unavailable. A Virtual Media or Remote File Share session is in use" until the session was cleared on the iDRAC. The close now calls the page's `disconnectVM()` first when virtual media is active (what Dell's Disconnect Viewer button does), waits 1.2 s, then `disconnectKVM()`; the whole close takes about 3.7 s instead of 2.5 s. A session left over from before this fix has to be cleared on the iDRAC once.
### Notes
- Confirmed on hardware (Linux, iDRAC8): after Disconnect with an ISO still mapped, the next console connected virtual media normally.

## [1.5.5] - 2026-10-14
### Fixed
- GUI HTML5 virtual media: the Map Device button stayed grey after choosing an image. On Linux the page's `change` handler never ran after the native file dialog (the input held the file, `fileName0` stayed undefined), so bobcat now checks the Virtual Media file inputs 2.5 times a second and does what Dell's handler does (sets `fileBlob`/`fileName`, enables the button) for a file the page has not seen. Found with the new test hook below; the virtual media session itself did start on the iDRAC.
### Added
- `BOBCAT_TEST_JS_EVERY=<seconds>` repeats the `BOBCAT_TEST_JS` expression (Linux), to watch page state while using the GUI.
### Notes
- Confirmed on hardware (Linux, iDRAC8): an ISO mapped as virtual CD/DVD was mounted and read from the server's OS. Unmap and disconnect cleanup, macOS and a removable disk image (`.img`) have not been tried.

## [1.5.4] - 2026-10-14
### Added
- GUI HTML5 console: virtual media. The console URL now carries `vmPriv=true` when the logged-in user has the virtual media privilege (`accessVirtualMedia` from the iDRAC session), so Dell's viewer shows its own Virtual Media button: Connect Virtual Media, pick an `.iso` (CD/DVD) or `.img` (removable disk, floppy) with the file dialog, Map Device. `BOBCAT_HTML5_VM=0` hides it again; `BOBCAT_DEBUG=1` logs the privilege. How the page does it is in RESEARCH.md.
### Notes
- Not tried yet: whether the embedded web view's file dialog opens (Linux, macOS), and whether the iDRAC then mounts the image. The probe also showed that `vm_bool_enabled` is 1 and `vmedia_bool_enabled` 0 on the iDRAC at 192.168.8.98; bobcat does not look at either.

## [1.5.3] - 2026-10-14
### Added
- GUI: Paste follows the keyboard layout chosen in the new Layout selector (US, UK, DE, FR; shared with the on-screen keyboard and remembered) for VNC and HTML5. For each character the page works out the key (by position, as the US key in the same place) and whether Shift is needed, as the on-screen keyboard would: on DE, `z` is the key at the US Y position and `/` is Shift+7. VNC sends (keysym, shift) pairs, HTML5 sends key events with `code`/`keyCode`/shift. Ctrl+Shift+V uses the same code. The selector is hidden for SSH, SOL and Java.
- Characters that are not on the selected layout (AltGr ones such as `@`, `{`, `\` on DE and FR) are sent as themselves with the US Shift rule, as before.
### Notes
- Worked out and checked with a script (US, DE, FR, UK samples), not yet tried on hardware with a non-US server. Java Paste is unchanged: Linux already follows the X keyboard layout, macOS still types with the US layout. SSH/SOL paste sends characters, so layouts do not matter there.

## [1.5.2] - 2026-10-14
### Changed
- `build.sh --target linux` without `--package` now builds the distribution's own package, chosen from `ID` / `ID_LIKE` in `/etc/os-release`: deb on Debian, Ubuntu and derivatives; rpm on RHEL, Fedora, CentOS, Alma, Rocky, Oracle, Amazon Linux and SUSE; AppImage on anything else. It used to build deb and AppImage. Add `--package appimage` (or `all`) for the AppImage. The `rpmbuild` check now applies to the default build too.
### Fixed/Tested
- The rpm build works on this Ubuntu machine (`--package rpm`: `dist/bobcat-1.5.1-1.x86_64.rpm`); installing it has not been tried.

## [1.5.1] - 2026-10-14
### Added
- `bobcat-cli --check` reports `rpmbuild` on Linux (optional, never fails the check; it is only needed for `build.sh --package rpm`). `--check --install` now also installs optional items that have a package (`rpm` through apt); Rosetta 2 is still installed through `softwareupdate`.

## [1.5.0] - 2026-10-14
### Added
- `build.sh --target linux --package deb|rpm|appimage|all`: choose the Linux package (a comma list such as `deb,rpm` works; without `--package` it still builds deb and AppImage; `all` is deb, rpm and AppImage). `--target rpm` is a shortcut like `--target deb`. An rpm needs `rpmbuild` (`apt install rpm` / `dnf install rpm-build`), which the script checks for first. Release uploads and the `dist/` copy pick up `.rpm` files. Not built yet: `rpmbuild` is not installed on the Linux machine, so only the argument checks were tried.
### Changed
- `--format` (1.4.7) is now `--package`; `--format` still works as an alias.

## [1.4.9] - 2026-10-14
### Added
- `build.sh --delete-release [<version>] [--keep-tag] [--yes] [--dry-run]`: deletes the GitHub release `v<version>` (default: the current version) with its uploaded files, and the tag on GitHub and locally unless `--keep-tag` is given. It shows what it will delete and asks first (`--yes` skips the question); `--dry-run` only prints the commands. It cannot be combined with `--release` or `--target`, and never touches commits or `dist/`. Not run yet (`gh` is not installed on the Linux machine; only the argument checks were tried).

## [1.4.8] - 2026-10-14
### Added
- GUI Java console on Linux: the viewer's window is docked over the console area (new `java_x11.rs`, `x11rb`). bobcat finds the viewer's top-level window by `_NET_WM_PID`, makes it frameless (`_MOTIF_WM_HINTS`), transient for the main window (stays above it, minimizes with it) and keeps it laid over the area between the toolbar and the status bar, following the main window. Reparenting it into the main window was tried first and failed on hardware (mutter and the viewer's toolkit put the window back in a frame). `BOBCAT_JAVA_DOCK=0` disables it.
- Paste for the Java console on Linux: brings the viewer to the front (`_NET_ACTIVE_WINDOW`) and types the clipboard as key presses with the XTest extension (lookup through the X keyboard layout, Shift handled, stops if the viewer stops being the active window). On GNOME/Wayland the first use shows a permission dialog (remote interaction) for XTest.
- Copy for the Java console on Linux: reads the viewer window's pixels with GetImage (the bottom 26 px information bar is left out; `BOBCAT_JAVA_COPY_CROP="top,bottom"` in pixels), then the same grayscale/invert/3x bicubic and tesseract as on macOS. The Copy and Paste buttons now show for Java on Linux.
### Changed
- Linux: the GUI sets `GDK_BACKEND=x11` (XWayland under Wayland) when it is not set and `DISPLAY` exists, because only an X11 window can host the viewer. Set `GDK_BACKEND` yourself to override.
- `prepare_for_ocr` moved from `java_dock.rs` to `java_console.rs` (shared by macOS and Linux).
### Notes
- Docking, Paste and Copy confirmed on hardware (Linux, GNOME on Wayland through XWayland). The first Paste typed nothing and the first attempt (reparenting) did not embed; both were reworked above. HiDPI scaling is not handled (sizes are in X pixels).

## [1.4.7] - 2026-10-14
### Added
- `build.sh --target deb` (and `--target linux --format deb|appimage|all`, default all): build only the Debian package or only the AppImage. `--format` is refused for other targets. The deb was built on Linux (`dist/bobcat_<version>_amd64.deb`); installing it has not been tried.

## [1.4.6] - 2026-10-14
### Added
- README: screenshots of the app (VNC, HTML5 and Java views; `docs/img/`, taken with the documentation address 192.0.2.10 and no password) and of the macOS installer.
- `build.sh --release [--draft] [--yes] [--dry-run]`: tags the commit `v<version>`, pushes the tag and creates the GitHub release (`gh`) with the installers in `dist/` for that version; the release notes are that version's section of this file. It can follow a build (`--target mac --release`) or run alone on what is already in `dist/`. If the release exists, the files are added to it, so each platform can be built and added from its own machine. It never commits: it stops unless the working tree is committed and HEAD is pushed, refuses a tag that already points at another commit, asks before it does anything (`--yes` skips the question) and `--dry-run` only prints the commands. Only `--dry-run` has been run.
### Changed
- GUI: the HTML5 and Java notes (shown before connecting) are centred with a readable width, and the command in them no longer breaks across lines.

## [1.4.5] - 2026-10-14
### Changed
- GUI Java console: the viewer's jars and native libraries are kept in `~/.config/bobcat/java-viewer/<ip>/` between sessions instead of being deleted and downloaded again every time. Each file is checked with its `ETag` (`If-None-Match`): the iDRAC8 answers `304 Not Modified` with no body, so a cached file costs one small request, and a firmware update that changes a file is picked up. The native libraries are only unpacked (and the macOS OpenSSL path patch applied) when they were downloaded or have not been unpacked yet. The launch file (`viewer.jnlp`, small, with a session-specific launch string) is still fetched each time. Delete the folder to force a fresh download. With `BOBCAT_DEBUG=1` a line says what came from the cache.

## [1.4.4] - 2026-10-14
### Removed
- macOS Java console: the Fit window / Actual size menu tried in the first 1.4.4 build is gone. The Dell Java viewer has no way to turn its scaling off: its View menu only has Refresh, Fit (a plain action), Full Screen and Session User List; Tools > Session Options has three tabs (General, plus two whose names Java does not expose to the Accessibility API, so bobcat could not press them) and the "Scaling" page it lists is only "Maintain Aspect Ratio" for windowed and full-screen mode (Res_en.properties). The viewer always scales the server's picture to its window (a 1600-px-wide screen in the 1100-pt docked window is shown at 0.69x), which is also why Copy's OCR text is small. The Accessibility experiments are in RESEARCH.md.

## [1.4.3] - 2026-10-14
### Fixed
- macOS Java Copy read the viewer's bottom information bar ("Current User(s): ...") and misread characters. The capture now leaves out the title bar (28 pt) and the information bar (26 pt; `BOBCAT_JAVA_COPY_CROP="top,bottom"` changes both) and is cleaned up before tesseract: grayscale, light-on-dark flipped to dark-on-light, enlarged 3x with bicubic smoothing, and tesseract keeps the column spacing (`preserve_interword_spaces`). Black-and-white thresholding was tried and dropped: on a real capture it cut the match with the true text from about 78% to 60% or less. With `BOBCAT_DEBUG=1` the raw and cleaned images are saved as `bobcat-java-copy-raw.png` and `-clean.png` in the temp folder.
- macOS Java console docking: the page only reported its toolbar and status bar heights for HTML5, so for Java they were 0 and the docked viewer covered the whole window, buttons included. The page now reports them for Java too, and the dock waits for real heights before it moves the viewer.
- macOS: with the viewer in front, the first click on the main window only activated it and was lost, and the dock then brought the viewer back to the front before a second click, so no toolbar button could be used. The main window now accepts the activating click (`acceptFirstMouse` in `tauri.conf.json`).
### Added
- macOS: **Paste** and **Copy** work on the Java console (`java_dock.rs`, commands `java_paste` and `java_copy_text`). Paste brings the viewer to the front and types the clipboard into it as real key presses (CGEvent, US layout, about 18 ms per character, Shift sent as its own key), the way a person would; it stops if the viewer loses the focus. It needs the Accessibility permission. Copy captures the viewer's console area with `screencapture` (needs the Screen Recording permission, requested the first time; restart bobcat after granting it), reads it with tesseract and puts the text on the clipboard, like HTML5 Copy on Linux. Both buttons show for Java on macOS only. Confirmed on hardware: Dell's viewer accepts the key presses and a pasted line arrived (docking works too). A longer paste with symbols and capitals also arrived intact. Copy (OCR quality, Screen Recording permission) is still to be tried.

## [1.4.2] - 2026-10-14
### Added
- macOS: the Java console's window is docked over the console area of the main window (`java_dock.rs`). A foreign window cannot be made a child of ours, so bobcat uses the Accessibility API (the same permission that silences the viewer's keyboard warning; macOS asks the first time): it reads its own window's frame, moves and resizes the viewer's window to the area below the toolbar and above the status bar every 150 ms (so it follows moving and resizing), minimizes it with the main window, and brings it back to the front about 0.4 s after the main window was clicked (not while a mouse button is held), because the main window would otherwise cover it. The viewer's own title bar stays visible. Without the permission the viewer stays a free window and the status line says how to grant it. `BOBCAT_JAVA_DOCK=0` turns docking off; `BOBCAT_DEBUG=1` logs the frames. Untested on hardware.

## [1.4.1] - 2026-10-14
### Added
- README: a comparison table of Bobcat against the iDRAC HTML5 viewer, the iDRAC Java viewer and a stock VNC client (Paste, Copy text, zoom, certificate pinning, keychain, enabling services, text-mode VNC, session release).
### Changed
- `bobcat-cli --check` on macOS: finds tools in the Homebrew folders (`/opt/homebrew/bin`, `/usr/local/bin`) even when PATH is minimal, reports whether Homebrew is installed, and on Apple Silicon whether Rosetta 2 is (the Intel-only Java viewer needs it). All platforms report whether a Java runtime is available for the GUI's Java console (`BOBCAT_JAVA`, the Java 8 the GUI downloaded into `~/.config/bobcat/java` on macOS, or `java` on the PATH elsewhere). These new checks are optional: they print `[--]` with a hint and do not make `--check` fail. `--check --install` also installs Rosetta 2 (`softwareupdate --install-rosetta --agree-to-license`) when it is missing; Homebrew and Java are not installed for you.

## [1.4.0] - 2026-10-14
### Changed
- GUI: the VNC port field sits directly after Host with a colon between them (`host:port`); it was after User. Both are hidden for the other protocols as before.
- `bobcat-gui` is now just `bobcat` (the GUI crate, its binary, the macOS app `bobcat.app` and the installer `bobcat_<version>_<arch>.dmg`; log lines start with `[bobcat]`). `bobcat-cli` keeps its name. The app identifier `au.com.lateralblast.bobcat` is unchanged. Earlier entries below still use the old name.
### Added
- `build.sh --target mac|linux|windows [--arch arm64|x64|universal] [--no-cli]`: builds the installers into `dist/` (mac: a `.dmg`; linux: `.deb` and `.AppImage`; windows: an NSIS `-setup.exe`) and a release `bobcat-cli`. A target can only be built on its own platform. Only `--target mac` has been run (arm64); linux and windows are untested.
- Cargo feature `vendored` (both crates): builds OpenSSL into the binary. `build.sh` turns it on so installers do not depend on Homebrew's OpenSSL (the first DMG linked `/opt/homebrew/opt/openssl@4`). Normal `cargo build` is unchanged.
- Bundling is on in `tauri.conf.json` (identifier, category, macOS minimum 11.0) and the app has real icons (`icon.icns`, `icon.ico` and PNGs generated from `bobcat.png`).
- The macOS app declares `NSLocalNetworkUsageDescription` (`gui/src-tauri/Info.plist`, merged into the bundle). Without it macOS Sequoia and later refuse LAN connections from a bundled app without asking, which showed as "connect failed: No route to host (os error 65)" from the DMG while `cargo run` (which uses Terminal's permission) worked. The first connection now asks for Local Network access; if it was refused, enable it under System Settings > Privacy & Security > Local Network.
- The macOS bundle is now signed ad hoc as a bundle (`signingIdentity: "-"`), so its identifier is the stable `au.com.lateralblast.bobcat` and its `Info.plist` is sealed into the signature. Before, only the binary carried a signature with a per-build identifier (`bobcat_gui-<hash>`) and the `Info.plist` was not bound to it, so the Local Network usage description was not honoured.
- The macOS app is not signed or notarized (ad-hoc signature only), so Gatekeeper warns on first launch (right-click > Open, or `xattr -dr com.apple.quarantine /Applications/bobcat-gui.app`).

## [1.3.9] - 2026-10-14
### Changed
- GUI Java console (macOS): the iDRAC's certificate is trusted in the viewer by default. Bobcat has already checked it against its pin (trust on first use, a changed certificate is refused), so after the pin check it writes that certificate into the viewer's certificate store (`~/.config/bobcat/java-trust`) under the name the viewer itself uses (`EMR_<SHA-256 of the issuer in OpenSSL one-line form>_<serial>.pem`, found by looking at what "Always trust this certificate" saved), before the viewer starts. The viewer's warning now only appears for a certificate bobcat did not pin, and a changed certificate is never added. Untested in the GUI; an expired certificate (192.168.8.240) is still refused by the viewer's OpenSSL.

## [1.3.8] - 2026-10-14
### Changed
- GUI Java console (macOS): the certificate warning of 1.3.7 was not removed by `SSL_CERT_DIR` (the viewer's OpenSSL ignores it), so that was dropped. The viewer has its own "User's Certificate Store Path" preference (Tools > Session Options > Certificate); once it is set, the warning offers "Always trust this certificate". Bobcat now sets that preference (`USER_CERT_STORE_PATH` in `com.avocent.viewer`, through `defaults` and PlistBuddy) to `~/.config/bobcat/java-trust`, unless the user has chosen a path of their own. Writing the trusted certificate there from the pinned one, so the warning never shows, is the next step (the format the viewer stores is still to be looked at).

## [1.3.7] - 2026-10-14
### Changed
- GUI Java console: the viewer's own certificate warning ("The connection to <ip> is untrusted") is meant to go away for certificates bobcat has pinned. The viewer is started with `SSL_CERT_DIR` pointing at a directory of the system root certificates (macOS; `/etc/ssl/certs` stays in the list elsewhere) plus the iDRAC's pinned certificate chain. An expired certificate is still refused by the viewer's OpenSSL and still shows the dialog; untested against a valid or default self-signed certificate.

## [1.3.6] - 2026-10-14
### Added
- GUI: **Java** protocol, the iDRAC's Java virtual console in a window of its own (macOS and 64-bit Linux; it cannot be embedded). Bobcat logs in, requests the Web Start launch file, downloads the viewer jar and its native libraries over the pinned connection, and starts `java` on them directly (no Java Web Start). The iDRAC must use the Java plugin (`bobcat-cli --enable java`).
- macOS: the viewer's native libraries are Intel-only, so it needs an x86_64 Java 8. The first connect asks, then downloads Azul Zulu 8 into `~/.config/bobcat/java` after checking its SHA-256 against Azul's published value (nothing system-wide; Rosetta 2 needed on Apple Silicon). `BOBCAT_JAVA` selects another runtime. Apple's stub `libssl` kills the viewer's virtual-media library, so the downloaded copy has its hard-coded `/usr/lib/libssl.dylib` path changed to one that fails normally (the library then uses its built-in OpenSSL), and the viewer runs with TLS 1.2 and no extended master secret, which the iDRAC needs. Details in RESEARCH.md.
- Virtual media is off in the Java viewer (`vm=0`); `BOBCAT_JAVA_VM=1` keeps what the iDRAC asks for. Disconnect ends the viewer with SIGTERM; closing the viewer's own window is the clean way (see TODO.md).
### Changed
- `html5.rs`: the login takes the expected plugin (HTML5 or Java); several helpers are shared with the Java console.

## [1.3.5] - 2026-10-14
### Changed
- GUI: removed the "Protocol" label in front of the protocol drop-down (it keeps a tooltip and an accessible name).

## [1.3.4] - 2026-10-14
### Changed
- GUI: the iDRAC address, user and password fields in the toolbar now have labels (Host, User, Pass). The User label hides with its field in the VNC view.

## [1.3.3] - 2026-10-14
### Changed
- GUI on macOS: the small certificate window the HTML5 viewer opens to the console port (it only answers the viewer's ping and closes itself) is now created hidden instead of flashing on screen. Set `BOBCAT_SHOW_POPUP=1` to show it again, for example if the console no longer connects.

## [1.3.2] - 2026-10-14
### Fixed
- GUI on macOS: the embedded HTML5 console was drawn about one title-bar height too high and covered the second toolbar row. The window's content view runs under the title bar, WebKit lays the main page out below it, and a child view's frame is measured from the top of the full view. The console's frame is now set directly on the main thread with the title-bar height (view height minus `contentLayoutRect`) added to its top, and Tauri's proportional auto-resize is turned off for the view (the page's resize events re-apply the layout). This replaces the `set_bounds` change in 1.3.1, which did not address the cause (the frame it set was already what was asked for). With `BOBCAT_DEBUG` the layout maths is logged (`layout:`).

## [1.3.1] - 2026-10-14
### Fixed
- GUI on macOS: the embedded HTML5 console covered the second toolbar row (Copy, Paste, ...) and did not stay below it when the window was resized. Tauri's `set_size` / `set_position` read the view's bounds back first and wry converts that y assuming an unflipped parent view, so each call moved the console. The layout is now applied with a single `set_bounds` call. With `BOBCAT_DEBUG` the computed layout is logged (`layout:`).

## [1.3.0] - 2026-10-14
### Added
- GUI on macOS: the HTML5 console is now embedded in the main window, between the toolbar and the status bar, instead of a separate window. The console view is positioned by frame and re-fitted whenever the window or toolbar changes size (the toolbar height is now reported separately from the status bar). If embedding fails the separate window is still used. The brief certificate window the viewer opens to the console port still appears.
### Changed
- GUI: the wait for the certificate check to be installed before the first navigation is shared by the embedded view and the separate window.

## [1.2.9] - 2026-10-14
### Changed
- GUI: the OS keychain is now read only while **Remember** is ticked. Before, the saved password was looked up at startup and whenever the IP, user or protocol changed, which made macOS show its keychain prompt even with Remember unticked. Ticking Remember now loads the saved password if there is one; a saved password is no longer filled in automatically at startup. Unticking Remember for a loaded entry still deletes it on connect.

## [1.2.8] - 2026-10-14
### Fixed
- GUI HTML5: an answer cut short by the iDRAC (connection ended without a TLS close before all the data arrived) was treated as complete, which showed up as a confusing `bad gzip body: incomplete deflate stream`. A body shorter than its `Content-Length`, or a gzip failure after such an early end, now reports that the iDRAC cut the answer short.
- `gui/src-tauri/tauri.conf.json` carried an old version (1.1.9); it is now kept in sync with the `Cargo.toml` files.

## [1.2.7] - 2026-10-14
### Added
- `bobcat-cli`: `--save` now covers every console. With `--console vnc` the VNC password is saved in the keychain (service `bobcat-vnc`, account: the iDRAC IP, the entry bobcat-gui uses) after the session ends normally, and later runs read it before prompting. Lookup order for the VNC password: `--vncpass` / `BOBCAT_VNC_PASS`, then the keychain, then the prompt.
- `--forget` removes both the login entry (for `--user`) and the VNC entry for the IP.
- `--enable vnc` together with `--console vnc` and no `--vncpass`: the console still asks for the VNC password after enabling, and that one is not saved (pass `--vncpass` to save it).

### Fixed
- GUI on macOS: the HTML5 console opened as a blank white window. WebKit reads which `WKNavigationDelegate` methods exist once, when the delegate is set, so the pinned-certificate check added afterwards was never called and the iDRAC's certificate was rejected by default. The delegate is now set again after the method is added, and the window waits for that before its first navigation. With `BOBCAT_DEBUG` the window also logs the cookie and navigation steps.

## [1.2.6] - 2026-10-14
### Added
- GUI: the **Remember** checkbox now also works in the VNC view. The VNC password goes in the OS keychain under the service `bobcat-vnc` (account: the iDRAC's IP), apart from the iDRAC login entries, and is saved once the iDRAC has accepted it. Filling, and deleting by unticking Remember and connecting, work as for the other protocols. `bobcat-cli` does not use the saved VNC password yet.

## [1.2.5] - 2026-10-14
### Changed
- GUI: **Copy** now comes before **Paste** on the second toolbar row (one toolbar serves the VNC, HTML5, SSH and SOL views).

## [1.2.4] - 2026-10-14
### Changed
- GUI: the **Copy text** button is now labelled **Copy** (it still reads "Cancel copy" while selecting).

## [1.2.3] - 2026-10-14
### Changed
- The GUI no longer starts with a default user name: the user field is empty unless `BOBCAT_USER` is set. Example user names were removed from RESEARCH.md. No credentials are stored anywhere in the code or documents.

## [1.2.2] - 2026-10-13
### Added
- GUI: **Remember** checkbox for the iDRAC password (SSH, SOL, HTML5), using the same OS keychain entries as `bobcat-cli` (service `bobcat`, account `user@ip`).
  - The password is saved only after the login works: after SSH authentication, after the HTML5 login, or when ipmitool reports "SOL Session operational".
  - Changing the IP or user, or switching protocol, fills the password from the keychain and ticks Remember. Unticking Remember and connecting deletes the saved entry. `BOBCAT_PASS` still wins. VNC is unaffected.
### Fixed
- The GUI crate builds on macOS again: the `BOBCAT_TEST_JS` / `BOBCAT_TEST_MAINJS` hooks (Linux-only code) are now gated by `cfg(target_os = "linux")`.

## [1.2.1] - 2026-10-13
### Added
- `bobcat-cli`: iDRAC login passwords can be kept in the OS credential store (macOS Keychain, Windows Credential Manager, Linux Secret Service) through the `keyring` crate, one entry per `user@ip` under the service name `bobcat`.
  - `--save` stores the password after the command succeeded (never one that just failed, and never one that already came from the keychain).
  - Lookup order for the iDRAC password: `--pass` / `BOBCAT_PASS`, then the keychain, then the prompt.
  - `--forget` deletes the saved entry. If a keychain password is rejected, the error says where it came from and how to replace it.
  - Only the iDRAC login (racadm, SOL, SSH console) is covered; the VNC password is unchanged. The GUI uses it too since 1.2.2.

## [1.2.0] - 2026-10-13
### Fixed
- `--check` no longer reports WebKitGTK/GTK as missing on macOS: those probes (and the `ldconfig` lookup) are Linux-only. On macOS the install hint and `--check --install` use Homebrew (`brew install`, no sudo) instead of apt.

## [1.1.9] - 2026-10-13
### Added
- GUI HTML5: Bobcat's own **Paste**, **Copy text** and **text size** controls work on the embedded console, and sit on the second toolbar row beside the Certificate button (the Keyboard button stays hidden in HTML5 mode).
  - **Paste** types the clipboard into the console (US layout) by dispatching key events on the page's window, which is where Dell's viewer listens (keydown/keypress/keyup), 12ms per character. Verified: a pasted command line appeared in the TrueNAS console, Enter included.
  - **Copy text** takes a WebKit snapshot of the console, cuts out the page's canvas, converts it to grayscale (inverted if dark), enlarges it 4x with bilinear filtering and reads it with tesseract; the text goes on the clipboard. Verified: the pasted command and error message were read back (the cursor can still confuse the last line).
  - **Text size** sets the web view's zoom factor (Fit window = 100%); Dell's own buttons scale with it.
- Developer hooks (headless testing): `BOBCAT_TEST_SNAPSHOT=<secs>:<dir>` saves PNGs of the console and main page, `BOBCAT_TEST_PASTE=<secs>:<text>`, `BOBCAT_TEST_COPY=<secs>:x`, `BOBCAT_TEST_ZOOM=<secs>:<factor>` and `BOBCAT_TEST_JS=<secs>:<javascript>`.
### Fixed
- The OCR step now converts and scales the image itself instead of using cairo's scaling, which read `[/]#` as `(7)`.

## [1.1.8] - 2026-10-13
### Fixed
- GUI HTML5 (embedded): the status bar was hidden under the console view. The placeholder text in the main page's console area did not shrink below its content, so it pushed the status bar out of the fixed-height strip; it now collapses (`min-height: 0`), so the strip shows the toolbar and the status bar.

## [1.1.7] - 2026-10-13
### Changed
- GUI HTML5: the console now appears **inside the main window**, in the console area under the toolbar, instead of a window of its own. It is a second web view in the main window (Tauri multiwebview, the `unstable` feature). On Linux a child view is packed into the window's vertical box rather than positioned, and WebKit's natural height is the whole window, so the main page (toolbar and status bar) is put in a fixed-height container at the top (its height is reported by the page) and the console view takes the rest; both are restored when the console closes. If embedding fails the old separate window is used. Closing, Disconnect, app exit, Ctrl+C and SIGTERM still disconnect the viewer first and log out. Checked headless on the iDRAC8: toolbar strip 1100x112, console 1100x688 in an 1100x800 window; connect, disconnect (sessions gone, layout restored) and reconnect all work.
- Debug-only test hooks `BOBCAT_TEST_DISCONNECT=<seconds>` and `BOBCAT_TEST_RECONNECT=1`; `BOBCAT_DEBUG=1` also logs the layout sizes.

## [1.1.6] - 2026-10-13
### Fixed
- GUI HTML5: the blank "certificate" pop-up stayed on top of the console. Dell's page closes it with `window.close()`, which does not close a window the app created, so the app now closes it 3 seconds after it finishes loading (the viewer pings it every 250ms, so the handshake is long done) and also closes any left over when the console closes.

## [1.1.5] - 2026-10-13
### Fixed
- GUI HTML5: the iDRAC kept a "Virtual Console" session for minutes after the window closed. Every way of closing the console (the window's X, Disconnect, closing the app, Ctrl+C, SIGTERM) now first calls the viewer's own `disconnectKVM()` (what Dell's Disconnect Viewer button does), which sends the close messages, waits about 2.5 seconds, then destroys the window and logs the web session out.
### Verified
- On the iDRAC8 the HTML5 console connects: the pop-up certificate window opens, the viewer opens its WebSockets to port 5900, the iDRAC lists a "Virtual Console" session and video data arrives.

## [1.1.4] - 2026-10-13
### Fixed
- GUI HTML5: still "Turn off your pop-up blocker" because WebKit itself blocks `window.open()` calls that are not a response to a click, before the app's new-window handler is ever called (it never ran). The console window now sets WebKit's `javascript-can-open-windows-automatically`, so the viewer's certificate window opens and the handler (iDRAC address only, pinned certificate) decides.

## [1.1.3] - 2026-10-13
### Fixed
- GUI HTML5: the console failed with "Turn off your pop-up blocker and try again!". The viewer opens a small window to `https://<ip>:5900` so the browser can trust the console server's certificate, pings it with `postMessage` and waits for the page to answer; the app denied every pop-up. Pop-ups to the iDRAC's own address are now allowed, linked to the console window (so `window.opener` and `postMessage` work) and held to the same pinned-certificate check; pop-ups to any other address are still denied.

## [1.1.2] - 2026-10-13
### Fixed
- GUI HTML5: every label in the console window read "undefined". The language file is `{"localeString": {...}}` and the web interface uses the inner object as `localeObj`; the GUI injected the whole file.

## [1.1.1] - 2026-10-13
### Fixed
- GUI HTML5: the console now starts. Dell's viewer page is written as a pop-up of the iDRAC web interface and reads its language strings from `window.opener.top.localeObj`; without that its script failed before connecting. The GUI now fetches the iDRAC language file and injects a stand-in `opener` before the page's scripts run (the page then opens its WebSocket connections: verified on the iDRAC8). Static pages and files are fetched with gzip, which the iDRAC requires. The web session is pinged every 2 minutes while the window is open.

## [1.1.0] - 2026-10-13
### Added
- GUI: **HTML5** in the Protocol drop-down opens the iDRAC's HTML5 virtual console in its own window. The GUI logs in to the iDRAC web interface itself (`/data/login`) over a TLS connection whose certificate is checked against the saved pin before the password is sent (the same certificate and pin as the VNC port), reads the session values the console page needs, hands the session cookie to a new web view and loads `virtualconsolehtml5.html`. The web view's own TLS connections are accepted only for the pinned certificate. Closing the window, Disconnect, closing the app, Ctrl+C or SIGTERM log the session out of the iDRAC (verified: the web session disappears). Needs the HTML5 viewer enabled: `bobcat-cli --enable html5`.

## [1.0.9] - 2026-10-13
### Removed
- The old `DRACON_*` environment variables are no longer read; only `BOBCAT_*` work (`BOBCAT_PASS`, `BOBCAT_VNC_PASS`, `BOBCAT_USER`, `BOBCAT_PROTOCOL`, `BOBCAT_TLS`, `BOBCAT_AUTOCONNECT`, `BOBCAT_ENCODING`, `BOBCAT_ONCE`, `BOBCAT_DUMP`, `BOBCAT_DEBUG`). The old pin file and character table paths are still read.

## [1.0.8] - 2026-10-13
### Changed
- The project is now called **Bobcat** (**B**MC **O**ut-of-band **B**rowser, **C**onsole **A**ccess **T**ool). `dracon-cli` is now `bobcat-cli` and `dracon-gui` is `bobcat-gui` (packages, binaries, window title, app identifier), the environment variables are `BOBCAT_*`, the shared pin file is `~/.config/bobcat/known_certs.json` and the learned character table `~/.cache/bobcat/glyphs.json`. Compatibility: `DRACON_*` variables still work, and the old pin file and character table are read (the pins are copied across on first use), so nothing needs to be re-trusted or re-learned. Earlier entries below keep the old name.
- README rewritten and reorganised.

## [1.0.7] - 2026-10-13
### Fixed
- `--console vnc`: `0` was sometimes read as `@` (and the real `@` as `?`). Tesseract reads this font's zero as 6, @, 8 or 0, so shapes are now matched to characters by the share of OCR votes weighted by how common the character is in console text (`@` is rare, `0` is common). Seen on a screen full of `ls -lsa` output.
- Shapes a calibration could not name (e.g. a glyph clipped at the top edge) are saved with the table, so repeat runs no longer spend ~5 seconds on them.

## [1.0.6] - 2026-10-13
### Added
- `--console vnc` keeps the learned character table between runs, per iDRAC, in `~/.cache/dracon-cli/glyphs.json` (cell size plus the votes for every shape). When nothing new is on screen the OCR step is skipped and the first screen appears as soon as the connection is up; new shapes still trigger a recalibration, which adds to the saved votes. Loaded counts are capped so fresh evidence can outvote a wrong guess. `--relearn` discards the saved table for the iDRAC and learns it again.

## [1.0.5] - 2026-10-13
### Added
- `dracon-cli --console vnc`: connects to the iDRAC VNC console, turns the screen into text and lets you type into it. The screen is cut into character cells; each distinct character shape is named once by running `tesseract` over the screen and voting across all its occurrences (so OCR slips are outvoted), then every later refresh is read by exact shape lookup. Input: letters, Ctrl/Alt combinations, Enter, Tab, Backspace, arrows, Home/End, PgUp/PgDn, Ins/Del and F1-F12 are sent as VNC key events (US layout). Leave with `~.` at the start of a line. Works for text consoles with a fixed-size bitmap font (Linux console, BIOS text screens); graphical screens report "no text console detected". Verified on the iDRAC8 against the TrueNAS Linux console.
- `--tls` / `--notls` for `--console vnc` (TLS is the default). TLS uses the same local forwarder as the GUI and pins the certificate on first use in the shared `~/.config/dracon-gui/known_certs.json`; `--trust-new-hostkey` now also accepts a changed certificate. A mismatch (TLS vs plain) gives a hint instead of hanging.
- `DRACON_ONCE=1` prints the screen as text once and exits (for scripts); `DRACON_DUMP=file.ppm` saves a raw frame; `DRACON_DEBUG=1` logs timings.
### Changed
- `--console vnc` does not ask for the iDRAC password (it uses the VNC password); `--check` lists `tesseract` for it. Debug builds are now optimised (`opt-level = 2`) because the screen conversion is pixel work.

## [1.0.4] - 2026-10-13
### Changed
- GUI: the Protocol drop-down entry "SSH console" is now just "SSH".

## [1.0.3] - 2026-10-13
### Changed
- GUI: the connection status moved from the toolbar to a fixed-height status bar (28px) at the bottom of the window. It stays visible and is not affected by scrolling the console; long messages are cut off with an ellipsis and shown in full as a tooltip.

## [1.0.2] - 2026-10-13
### Fixed
- `dracon-cli --console ssh`: every way out now hangs up properly (quit key, close, disconnect) so the iDRAC releases the serial device: `~.`, the server closing, a write error, the terminal closing (SIGHUP) and SIGTERM/SIGINT, which used to just kill the process. The terminal mode is restored before the closing message.
- `dracon-cli --console sol`: if the program is told to stop (SIGTERM/SIGHUP/SIGINT), it runs `ipmitool sol deactivate` and stops `ipmitool`, instead of leaving the SOL session active. A normal `~.` is still handled by `ipmitool` itself, and an "already active" error does not deactivate someone else's session.
- Still not covered: SIGKILL and power loss.

## [1.0.1] - 2026-10-13
### Fixed
- SSH/SOL sessions are now hung up cleanly so the iDRAC does not keep them "in use" (previously they stayed connected until cleared on the iDRAC):
  - GUI SSH console: on Disconnect, window close, Ctrl+C or SIGTERM it sends the iDRAC's quit key (Ctrl+\), closes the channel and disconnects. Connecting again waits for the previous session to finish hanging up.
  - GUI SOL: sends `~.` and waits for `ipmitool` to exit by itself (which deactivates the SOL payload); if it does not, runs `ipmitool sol deactivate` and then stops it. It used to kill `ipmitool` immediately.
  - `dracon-cli --console ssh`: leaving with `~.` or when the server closes now sends the quit key and disconnects properly.
- Not covered: killing the program with SIGKILL, or the machine losing power, cannot run any cleanup.

## [1.0.0] - 2026-10-13
### Fixed
- GUI: scrolling the SSH/SOL terminal no longer scrolls the toolbar. The page itself never scrolls; only the terminal (or, for VNC, the screen area) does.

## [0.9.9] - 2026-10-13
### Fixed
- GUI SSH/SOL terminal: menus that reserve screen space scrolled the screen (the TrueNAS console menu assumes a 66-row screen, then prints newlines down to row 66 after asking the cursor position; the GUI terminal had fewer rows). Cursor position reports (ESC[6n) are now answered with a row far down the screen, so such menus redraw in place; a program that measures the screen (moves to 999;999 first) still gets the true position. Found with the `DRACON_DEBUG=2` logging.

## [0.9.8] - 2026-10-13
### Added
- GUI: `DRACON_DEBUG=2` logs the bytes sent to and from the SSH/SOL terminal, and the terminal size and resizes, to help diagnose terminal display problems. `DRACON_DEBUG=1` logs sizes and byte counts only.

## [0.9.7] - 2026-10-13
### Changed
- GUI: the drop-downs (Protocol, text size) are the same height (30px) as the toolbar buttons.

## [0.9.6] - 2026-10-13
### Added
- SSH host key pinning (trust on first use) in `dracon-gui` and `dracon-cli`: the iDRAC's SSH host key fingerprint is saved the first time and checked before the password is sent; a changed key blocks the connection. The GUI's Certificate button becomes **Host key** for the SSH console (and is hidden for SOL). The CLI accepts a changed key only with `--trust-new-hostkey`. Pins are shared in `~/.config/dracon-gui/known_certs.json` (keys `ssh:<ip>`).
### Fixed
- ISSUES 1 (SSH host key not verified).

## [0.9.5] - 2026-10-13
### Added
- GUI: Paste, Copy text and Keyboard now work in every protocol. In SSH/SOL: Paste sends the local clipboard to the terminal, Copy text copies the terminal selection (or the whole visible screen), and the on-screen keyboard sends terminal bytes (arrows, function keys, Ctrl and Alt combinations included). Text size already applied to the terminal font.

## [0.9.4] - 2026-10-13
### Added
- GUI: Protocol drop-down (VNC default, SSH console, SOL). The text protocols show a terminal (xterm.js 5.5.0, vendored in `gui/ui/vendor`, MIT) and a user field; VNC-only controls are hidden. SSH uses the iDRAC's `console com2`; SOL runs `ipmitool sol activate` in a pseudo-terminal (password via environment). The Text size menu sets the terminal font size. `DRACON_PROTOCOL`, `DRACON_USER` and `DRACON_PASS` set the starting protocol, user and password. Verified: SSH console connects and streams data on iDRAC8; SOL not yet tested in the GUI.

## [0.9.3] - 2026-10-13
### Added
- `--console ssh` opens the serial console through the iDRAC's SSH shell (`console com2`) on your terminal. No ipmitool needed; credentials are handled in-process. Leave with `~.` at the start of a line or `Ctrl+\`. Verified on iDRAC8 (works alongside an existing ipmitool SOL session).

## [0.9.2] - 2026-10-13
### Added
- `--console sol` attaches to the server's serial console over IPMI Serial over LAN by running `ipmitool -I lanplus ... sol activate` (password passed via `IPMI_PASSWORD`, not the command line). Leave with `~.`. `--check` now also checks for `ipmitool`.
### Changed
- The SSH connection is only opened when `--enable`/`--disable` is used.

## [0.9.1] - 2026-10-13
### Added
- `--enable sol` enables IPMI over LAN, Serial over LAN (115200 baud, Administrator) and the iDRAC serial console, and ensures BIOS serial console redirection (COM2, redirect after boot, 115200). Only settings that differ are changed. BIOS changes are staged as a job for the next reboot; the server is never rebooted by dracon-cli. Verified on iDRAC8 (all already set, so nothing changed).

## [0.9.0] - 2026-10-13
### Added
- `--enable java` enables the virtual console and selects the Java viewer (`PluginType 1`). Giving both `--enable html5` and `--enable java` is rejected. Verified on iDRAC8 firmware 2.86.86.86 (then restored to HTML5).
### Changed
- `--disable html5` / `--disable java` now report "--disable is only supported for vnc".

## [0.8.0] - 2026-10-13
### Added
- `--enable html5` enables the iDRAC virtual console and selects the HTML5 viewer (`iDRAC.VirtualConsole.Enable Enabled`, `PluginType 2`). Verified on iDRAC8 firmware 2.86.86.86.

## [0.7.3] - 2026-10-13
### Changed
- Renamed the command line tool `dracon` to `dracon-cli` (package, binary and help text). The GUI is still `dracon-gui`.

## [0.7.2] - 2026-10-13
### Fixed
- GUI: the size drop-down now shows black text on a light background (it was grey and hard to read).

## [0.7.1] - 2026-10-13
### Changed
- GUI: the Text size menu no longer has a text label; it is a plain drop-down showing the current size (tooltip: "Text size").

## [0.7.0] - 2026-10-13
### Added
- GUI on-screen keyboard: layout buttons in its title bar (US default, UK, German QWERTZ, French AZERTY). The choice is remembered. Layouts change the labels and the characters sent; behaviour on the server depends on how it maps them.

## [0.6.2] - 2026-10-13
### Changed
- GUI: scaled text is sharper. The remote screen is kept at native size and drawn to the display with a high-quality resampler (at the screen's device pixel ratio); whole-number zooms (100%, 200%) stay pixel-crisp.

## [0.6.1] - 2026-10-13
### Changed
- GUI: Connect and Disconnect moved up to the connection row; the other buttons and Text size stay on the second row.

## [0.6.0] - 2026-10-13
### Added
- GUI: "Text size" menu (50% to 200%, or Fit window) that scales the remote screen; the window resizes to match (except in Fit window). The choice is remembered.

## [0.5.1] - 2026-10-13
### Changed
- GUI: toolbar split into two rows: connection fields and status on top, action buttons (Connect, Disconnect, Paste, Copy text, Keyboard, Certificate, Ctrl+Alt+Del) underneath.

## [0.5.0] - 2026-10-13
### Added
- GUI: TLS certificate pinning (trust on first use). The iDRAC certificate's SHA-256 fingerprint is saved silently the first time a host is seen (`~/.config/dracon-gui/known_certs.json`); if it later changes the connection is blocked with a warning and not retried.
- GUI: Certificate button shows the subject, issuer, validity and fingerprint, and offers "Trust this certificate" after a change.

## [0.4.1] - 2026-10-13
### Fixed
- GUI: wrong colours (light blue/cyan) with Zrle. The iDRAC's native 16-bit format was not trusted; the GUI again forces 32-bit BGRX (set while Raw is still selected) and only then switches to Zrle.

## [0.4.0] - 2026-10-13
### Added
- GUI: Zrle compressed encoding (Raw as fallback; `DRACON_ENCODING=raw` forces Raw). Over TLS the first full frame arrives in ~0.3s instead of ~2.3s.
### Changed
- GUI: uses the server's native pixel format (iDRAC8: 16-bit RGB555) and converts to RGBA, instead of forcing 32-bit with `Client::set_format`, which hangs with tiled Zrle updates. CopyRect is no longer requested.

## [0.3.3] - 2026-10-13
### Fixed
- GUI: reverted the 0.3.2 `poll(2)`-based TLS relay, which caused repeated disconnects, back to the 0.3.1 timeout-based relay. The forwarder now logs why it closed.

## [0.3.2] - 2026-10-13
### Fixed
- GUI: much faster screen refresh in TLS mode. The TLS forwarder polled with 5ms timeouts per chunk; it now relays with `poll(2)` and TCP_NODELAY.

## [0.3.1] - 2026-10-13
### Fixed
- GUI: connecting retries up to 6 times, 5s apart, when the iDRAC resets or refuses the connection (it needs a few seconds to release the previous VNC session, e.g. after clicking Connect again). Authentication failures are not retried.

## [0.3.0] - 2026-10-13
### Added
- GUI: TLS option (checkbox, or `DRACON_TLS=1`) for iDRAC VNC with SSL encryption enabled. The iDRAC wraps the whole VNC connection in TLS 1.2 (AES256-GCM-SHA384, self-signed cert, ~10-15s handshake); a local forwarder using the `openssl` crate does the TLS leg. Verified on hardware with `--sslebl Auto_Negotiate`.

## [0.2.4] - 2026-10-13
### Fixed
- `--sslebl`: racadm rejects the value names; numeric values (0 Disabled, 1 Auto Negotiate, 2 128-Bit or higher, 3 168-Bit or higher) are now sent. Added `168-Bit_or_higher`.
- racadm errors over SSH (which exit 0) are now detected and reported instead of being ignored.

## [0.2.3] - 2026-10-13
### Changed
- GUI: buttons no longer shrink when the window is resized. On-screen keyboard keys have a fixed size, and toolbar buttons keep their size and wrap onto another line when the window is narrow.

## [0.2.2] - 2026-10-13
### Changed
- GUI: the on-screen keyboard can be moved by dragging its title bar (kept inside the window).

## [0.2.1] - 2026-10-13
### Added
- GUI on-screen keyboard (Keyboard button): US layout with function, arrow and navigation keys. Shift/Ctrl/Alt/Meta are one-shot latches applied to the next key.

## [0.2.0] - 2026-10-13
### Changed
- GUI: default border padding changed from 32px to 16px.

## [0.1.9] - 2026-10-13
### Changed
- GUI: default border padding increased from 24px to 32px.

## [0.1.8] - 2026-10-13
### Changed
- GUI: the remote screen now has a 24px black border on all four sides; the window fits screen + border + toolbar.

## [0.1.7] - 2026-10-13
### Fixed
- GUI: add 24px padding when fitting the window so the bottom line of the remote screen is not cut off; the area around the screen is black.

## [0.1.6] - 2026-10-13
### Changed
- GUI: the window resizes to fit the remote screen (plus toolbar) on connect and on remote resolution change, capped to the monitor size. Fixes the bottom lines being cut off.

## [0.1.5] - 2026-10-13
### Fixed
- GUI: copied text could be lost on Linux ("Clipboard was dropped very quickly after writing"); the clipboard handle is now kept alive for the life of the app.

## [0.1.4] - 2026-10-13
### Added
- `--install` (with `--check`) installs missing packages via `apt-get install -y` (through `sudo` unless root), then re-checks.

## [0.1.3] - 2026-10-13
### Added
- `--check` flag reports whether required external packages (tesseract, WebKitGTK 4.1, GTK 3) are installed, with an apt install hint, and exits non-zero if any are missing. `--ip` and `--user` are not needed with it.

## [0.1.2] - 2026-10-13
### Added
- GUI "Copy text": drag a rectangle over the console and the text in it is recognised with `tesseract` (OCR) and put on the local clipboard. Requires `tesseract-ocr`.
- Server clipboard text (RFB ServerCutText) is copied to the local clipboard when the server sends it.

## [0.1.1] - 2026-10-13
### Added
- GUI paste: Paste button or Ctrl+Shift+V types the local clipboard text into the console as key events (US layout, max 10000 chars).

## [0.1.0] - 2026-10-13
### Added
- GUI keyboard and mouse input (key and pointer events, wheel scroll, Ctrl+Alt+Del button). Click the screen to focus it.

## [0.0.9] - 2026-10-13
### Added
- `gui/`: Tauri 2 app (`dracon-gui`) that connects to the iDRAC VNC server (`vnc` crate, Raw + CopyRect encodings) and draws the framebuffer on a canvas. Display only; no keyboard/mouse yet.
- `DRACON_VNC_PASS` prefills the VNC password; `DRACON_AUTOCONNECT=1` connects on start; `DRACON_DEBUG=1` logs received rectangles.

## [0.0.8] - 2026-10-13
### Added
- `--timeout` flag (default 300s, range 60-10800) sets the VNC idle timeout with `--enable vnc`.

## [0.0.7] - 2026-10-13
### Added
- `--sslebl` flag (Disabled, Auto_Negotiate, 128-Bit_or_higher; default Disabled) sets VNC SSLEncryptionBitLength with `--enable vnc`.

## [0.0.6] - 2026-10-13
### Added
- `--port` flag (default 5901, range 1024-65535) sets the iDRAC VNC server port with `--enable vnc`.

## [0.0.5] - 2026-10-13
### Added
- `--vncpass` flag to set the VNC password on the command line (`DRACON_VNC_PASS` still works via clap env).

### Verified
- `--enable vnc` confirmed working on real iDRAC hardware.

## [0.0.4] - 2026-10-13
### Added
- `--disable vnc` disables the iDRAC VNC server via racadm over SSH.

## [0.0.3] - 2026-10-13
### Added
- `--enable vnc` runs racadm over SSH (in-process `ssh2`, no sshpass) to set the VNC password and enable the VNC server.
- `--pass` may now come from `DRACON_PASS` or an interactive prompt; VNC password from `DRACON_VNC_PASS` or a prompt.

## [0.0.2] - 2026-10-13
### Added
- `dracon` Rust CLI skeleton with `--ip`, `--user` and `--pass` flags (clap).

## [0.0.1] - 2026-10-13
### Added
- Initial project scaffolding: README.md, CHANGELOG.md, TODO.md, ISSUES.md, RESEARCH.md, LICENSE.md, .gitignore.
