// bobcat-gui: Bobcat's own paste, copy and zoom controls for the embedded HTML5 console
// Version: 1.1.9
// License: CC BY-NC-SA 4.0 (see LICENSE.md)
//
// The console is Dell's web page in a web view, so these work from the outside:
//  - zoom:  the web view's zoom factor
//  - paste: key events dispatched on the page's window (Dell's viewer listens for keydown /
//           keypress / keyup there and maps them to USB keys)
//  - copy:  a WebKit snapshot of the console, cropped to the page's canvas and read with tesseract

use std::sync::mpsc;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::html5::LABEL;

/// JavaScript that types `text` into the viewer, US layout, as real-looking key events.
fn paste_script(text: &str) -> String {
    let json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(async function (text) {{
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const SHIFTED = '~!@#$%^&*()_+{{}}|:"<>?';
  const UNSHIFT = {{ '~': '`', '!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
                   '_': '-', '+': '=', '{{': '[', '}}': ']', '|': '\\', ':': ';', '"': "'", '<': ',', '>': '.', '?': '/' }};
  const PUNCT = {{ ' ': ['Space', 32], '-': ['Minus', 189], '=': ['Equal', 187], '[': ['BracketLeft', 219], ']': ['BracketRight', 221],
                 '\\': ['Backslash', 220], ';': ['Semicolon', 186], "'": ['Quote', 222], ',': ['Comma', 188], '.': ['Period', 190],
                 '/': ['Slash', 191], '`': ['Backquote', 192] }};
  function info(ch) {{
    if (ch === '\n') return {{ key: 'Enter', code: 'Enter', keyCode: 13, shift: false }};
    if (ch === '\t') return {{ key: 'Tab', code: 'Tab', keyCode: 9, shift: false }};
    const shift = SHIFTED.includes(ch) || (ch >= 'A' && ch <= 'Z');
    const base = SHIFTED.includes(ch) ? UNSHIFT[ch] : ch.toLowerCase();
    if (base >= 'a' && base <= 'z') return {{ key: ch, code: 'Key' + base.toUpperCase(), keyCode: base.toUpperCase().charCodeAt(0), shift }};
    if (base >= '0' && base <= '9') return {{ key: ch, code: 'Digit' + base, keyCode: base.charCodeAt(0), shift }};
    if (PUNCT[base]) return {{ key: ch, code: PUNCT[base][0], keyCode: PUNCT[base][1], shift }};
    return null;
  }}
  function fire(type, o) {{
    const e = new KeyboardEvent(type, {{ key: o.key, code: o.code, bubbles: true, cancelable: true, shiftKey: !!o.shift }});
    Object.defineProperty(e, 'keyCode', {{ get: () => o.keyCode }});
    Object.defineProperty(e, 'which', {{ get: () => o.keyCode }});
    window.dispatchEvent(e);
  }}
  const SHIFT = {{ key: 'Shift', code: 'ShiftLeft', keyCode: 16, shift: true }};
  for (const ch of text) {{
    const o = info(ch);
    if (!o) continue;
    if (o.shift) fire('keydown', SHIFT);
    fire('keydown', o);
    fire('keypress', o);
    fire('keyup', o);
    if (o.shift) fire('keyup', SHIFT);
    await sleep(12);
  }}
}})({json});"#
    )
}

#[tauri::command]
pub fn html5_paste(app: AppHandle) -> Result<usize, String> {
    paste_from_clipboard(&app)
}

pub fn paste_from_clipboard(app: &AppHandle) -> Result<usize, String> {
    let text = crate::with_clip(|c| c.get_text())?;
    let n = text.chars().count();
    if n > 10_000 {
        return Err(format!("clipboard too large ({n} characters, max 10000)"));
    }
    let wv = app.get_webview(LABEL).ok_or("the HTML5 console is not open")?;
    wv.eval(paste_script(&text)).map_err(|e| e.to_string())?;
    Ok(n)
}

#[tauri::command]
pub fn html5_zoom(app: AppHandle, factor: f64) -> Result<(), String> {
    zoom(&app, factor)
}

pub fn zoom(app: &AppHandle, factor: f64) -> Result<(), String> {
    let wv = app.get_webview(LABEL).ok_or("the HTML5 console is not open")?;
    wv.set_zoom(factor.clamp(0.25, 4.0)).map_err(|e| e.to_string())
}

/// Run JavaScript in the console page and return the string it evaluates to.
#[cfg(target_os = "linux")]
fn run_js(app: &AppHandle, script: String) -> Result<String, String> {
    run_js_in(app, LABEL, script)
}

/// The same for any web view by label ("main" is the toolbar page).
#[cfg(target_os = "linux")]
pub fn run_js_in(app: &AppHandle, label: &str, script: String) -> Result<String, String> {
    let wv = app.get_webview(label).ok_or_else(|| format!("no web view called {label}"))?;
    let (tx, rx) = mpsc::channel();
    wv.with_webview(move |pw| {
        use javascriptcore::ValueExt;
        use webkit2gtk::WebViewExt;
        #[allow(deprecated)]
        pw.inner().run_javascript(&script, None::<&webkit2gtk::gio::Cancellable>, move |res| {
            let r = res
                .map_err(|e| e.to_string())
                .and_then(|r| r.js_value().map(|v| v.to_str().to_string()).ok_or_else(|| "no result".to_string()));
            let _ = tx.send(r);
        });
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "the console page did not answer".to_string())?
}

pub fn run_js_pub(app: &AppHandle, script: String) -> Result<String, String> {
    run_js(app, script)
}

/// A snapshot of a web view (PNG). With `crop` = (x, y, w, h, scale) in CSS pixels (the page's
/// pixel ratio is applied) the snapshot is cut to that area, inverted if it is mostly dark,
/// enlarged `scale` times and returned as a PGM, which is what tesseract reads best.
#[cfg(target_os = "linux")]
pub fn snapshot_png(app: &AppHandle, label: &str, crop: Option<(f64, f64, f64, f64, f64)>) -> Result<Vec<u8>, String> {
    let wv = app.get_webview(label).ok_or_else(|| format!("no web view called {label}"))?;
    let (tx, rx) = mpsc::channel::<Result<Vec<u8>, String>>();
    wv.with_webview(move |pw| {
        use gtk::prelude::WidgetExt;
        use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt};
        let dpr = WidgetExt::scale_factor(&pw.inner()) as f64;
        pw.inner().snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&webkit2gtk::gio::Cancellable>, move |res| {
            let out = res.map_err(|e| e.to_string()).and_then(|surface| process_snapshot(surface, crop, dpr));
            let _ = tx.send(out);
        });
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(15)).map_err(|_| "the snapshot timed out".to_string())?
}

#[cfg(target_os = "linux")]
fn process_snapshot(surface: gtk::cairo::Surface, crop: Option<(f64, f64, f64, f64, f64)>, dpr: f64) -> Result<Vec<u8>, String> {
    use gtk::cairo::{Context, Format, ImageSurface};
    let img = ImageSurface::try_from(surface).map_err(|_| "the snapshot is not an image".to_string())?;
    let mut png = Vec::new();
    let Some((x, y, w, h, scale)) = crop else {
        img.write_to_png(&mut png).map_err(|e| e.to_string())?;
        return Ok(png);
    };
    // cut the area out at its own size (no scaling)
    let (x, y, w, h) = (x * dpr, y * dpr, (w * dpr).round().max(1.0), (h * dpr).round().max(1.0));
    let (cw, ch) = (w as i32, h as i32);
    let mut cut = ImageSurface::create(Format::Rgb24, cw, ch).map_err(|e| e.to_string())?;
    {
        let cr = Context::new(&cut).map_err(|e| e.to_string())?;
        cr.set_source_surface(&img, -x.round(), -y.round()).map_err(|e| e.to_string())?;
        cr.paint().map_err(|e| e.to_string())?;
    }
    cut.flush();
    let stride = cut.stride() as usize;
    let data = cut.data().map_err(|e| e.to_string())?;
    let mut gray = Vec::with_capacity((cw * ch) as usize);
    for row in 0..ch as usize {
        for col in 0..cw as usize {
            let p = row * stride + col * 4; // B, G, R, X
            gray.push(((data[p] as u32 + data[p + 1] as u32 + data[p + 2] as u32) / 3) as u8);
        }
    }
    // light text on a dark screen reads far better as dark text on light
    let mean = gray.iter().map(|&v| v as u64).sum::<u64>() / gray.len().max(1) as u64;
    if mean < 128 {
        for v in gray.iter_mut() {
            *v = 255 - *v;
        }
    }
    // enlarge with bilinear filtering (tesseract does much better on larger, smooth text; nearest
    // neighbour and cairo's own scaling both read worse), and hand it over as a plain PGM
    let s = scale.max(1.0) as usize;
    let big = bilinear(&gray, cw as usize, ch as usize, s);
    let mut pgm = format!("P5\n{} {}\n255\n", cw as usize * s, ch as usize * s).into_bytes();
    pgm.extend_from_slice(&big);
    Ok(pgm)
}

/// Bilinear upscale of an 8-bit grayscale image by an integer factor.
fn bilinear(src: &[u8], w: usize, h: usize, s: usize) -> Vec<u8> {
    let (ow, oh) = (w * s, h * s);
    let mut out = vec![0u8; ow * oh];
    let at = |x: isize, y: isize| src[(y.clamp(0, h as isize - 1) as usize) * w + x.clamp(0, w as isize - 1) as usize] as f32;
    for oy in 0..oh {
        let sy = (oy as f32 + 0.5) / s as f32 - 0.5;
        let y0 = sy.floor();
        let fy = sy - y0;
        for ox in 0..ow {
            let sx = (ox as f32 + 0.5) / s as f32 - 0.5;
            let x0 = sx.floor();
            let fx = sx - x0;
            let (xi, yi) = (x0 as isize, y0 as isize);
            let v = at(xi, yi) * (1.0 - fx) * (1.0 - fy)
                + at(xi + 1, yi) * fx * (1.0 - fy)
                + at(xi, yi + 1) * (1.0 - fx) * fy
                + at(xi + 1, yi + 1) * fx * fy;
            out[oy * ow + ox] = v.round() as u8;
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
pub fn snapshot_png(_app: &AppHandle, _label: &str, _crop: Option<(f64, f64, f64, f64, f64)>) -> Result<Vec<u8>, String> {
    Err("snapshots are only implemented on Linux".into())
}

/// Copy the console screen as text: snapshot the page's canvas, read it with tesseract and
/// put the text on the clipboard.
#[tauri::command]
pub async fn html5_copy_text(app: AppHandle) -> Result<String, String> {
    copy_text(&app)
}

pub fn copy_text(app: &AppHandle) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        // where is the console canvas on the page (the largest canvas), in CSS pixels
        let rect = run_js(
            app,
            "(function(){var best=null,area=0;Array.prototype.forEach.call(document.getElementsByTagName('canvas'),function(c){var r=c.getBoundingClientRect();if(r.width*r.height>area){area=r.width*r.height;best=r;}});if(!best){best={left:0,top:0,width:window.innerWidth,height:window.innerHeight};}return [best.left,best.top,best.width,best.height].join(',');})()".to_string(),
        )?;
        // plain "x,y,w,h": the page's Prototype.js changes how JSON.stringify serialises arrays
        let v: Vec<f64> = rect.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if v.len() != 4 || v[2] < 20.0 || v[3] < 20.0 {
            return Err("the console screen is not showing yet".into());
        }
        let png = snapshot_png(app, LABEL, Some((v[0], v[1], v[2], v[3], 4.0)))?;
        crate::ocr_png_to_clipboard(&png)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        Err("copying text from the HTML5 console is only implemented on Linux".into())
    }
}
