// bobcat-cli: turn a screenshot of a text console into text.
// Version: 1.1.9
// License: CC BY-NC-SA 4.0 (see LICENSE.md)
//
// A text console draws every character as a small bitmap on a fixed grid, so each
// distinct character is one repeated shape. We (1) find the grid, (2) cut the screen
// into cells and reduce each to a "shape", (3) learn which character each shape is by
// running tesseract on the screen once and voting across all occurrences of a shape
// (a shape appears many times, so OCR slips are outvoted), then (4) read the screen
// by looking shapes up, which is fast, exact and stable between refreshes.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

pub type Shape = Vec<u8>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grid {
    pub cw: usize,
    pub ch: usize,
    pub cols: usize,
    pub rows: usize,
}

/// The screen cut into cells. `keys[row * cols + col]` is None for a blank cell.
#[derive(Clone)]
pub struct Cells {
    pub grid: Grid,
    pub keys: Vec<Option<Shape>>,
    /// frame-sized: true where a pixel differs from its cell's background
    pub ink: Vec<bool>,
    pub width: usize,
}

/// Cut `px` (w*h pixels) into cw x ch cells. The background of each cell is its most
/// common colour (so reversed-video text and coloured dialogs read like normal text).
pub fn analyse(px: &[u32], w: usize, h: usize, cw: usize, ch: usize) -> Cells {
    let (cols, rows) = (w / cw, h / ch);
    let mut keys = Vec::with_capacity(cols * rows);
    let mut ink = vec![false; w * h];
    let bytes = (cw * ch + 7) / 8;
    for r in 0..rows {
        for c in 0..cols {
            // most common colour in the cell
            let mut counts: Vec<(u32, u32)> = Vec::new();
            for y in 0..ch {
                let base = (r * ch + y) * w + c * cw;
                for x in 0..cw {
                    let p = px[base + x];
                    match counts.iter_mut().find(|(col, _)| *col == p) {
                        Some((_, n)) => *n += 1,
                        None => counts.push((p, 1)),
                    }
                }
            }
            let bg = counts.iter().max_by_key(|(_, n)| *n).map(|(c, _)| *c).unwrap_or(0);
            let mut key = vec![0u8; bytes];
            let mut any = false;
            for y in 0..ch {
                let base = (r * ch + y) * w + c * cw;
                for x in 0..cw {
                    if px[base + x] != bg {
                        let bit = y * cw + x;
                        key[bit / 8] |= 0x80 >> (bit % 8);
                        ink[base + x] = true;
                        any = true;
                    }
                }
            }
            keys.push(if any { Some(key) } else { None });
        }
    }
    Cells { grid: Grid { cw, ch, cols, rows }, keys, ink, width: w }
}

/// Find the character cell size: the largest candidate whose cells reduce to very few
/// distinct shapes (a smaller divisor of the true size also scores well, so prefer big).
pub fn detect_grid(px: &[u32], w: usize, h: usize) -> Option<(usize, usize)> {
    const CANDIDATES: &[(usize, usize)] = &[
        (16, 32), (14, 28), (12, 24), (10, 20), (9, 18), (9, 16), (8, 16), (8, 14), (8, 13),
        (8, 12), (8, 10), (8, 8), (7, 14), (7, 13), (6, 12), (6, 10), (5, 8),
    ];
    let mut best: Option<(usize, usize, usize)> = None; // (area, cw, ch)
    for &(cw, ch) in CANDIDATES {
        if w < cw * 20 || h < ch * 8 {
            continue;
        }
        let cells = analyse(px, w, h, cw, ch);
        let nonempty = cells.keys.iter().flatten().count();
        if nonempty < 20 {
            continue;
        }
        let distinct = cells.keys.iter().flatten().collect::<std::collections::HashSet<_>>().len();
        if (distinct as f64) / (nonempty as f64) <= 0.08 && distinct <= 400 {
            let area = cw * ch;
            if best.map_or(true, |(a, _, _)| area > a) {
                best = Some((area, cw, ch));
            }
        }
    }
    best.map(|(_, cw, ch)| (cw, ch))
}

/// What the OCR said each shape was: shape -> character -> votes.
pub type Votes = HashMap<Shape, HashMap<char, u32>>;

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

/// Run tesseract over the screen (one row per line) at several scales and collect votes.
/// A line only counts if tesseract returned exactly as many characters as there are
/// non-blank cells in that row, which keeps misread or merged lines out of the vote.
pub fn ocr_votes(cells: &Cells, scales: &[usize]) -> Result<Votes, String> {
    let g = cells.grid;
    let rows: Vec<usize> = (0..g.rows)
        .filter(|&r| (0..g.cols).any(|c| cells.keys[r * g.cols + c].is_some()))
        .collect();
    if rows.is_empty() {
        return Ok(Votes::new());
    }
    // black ink on white, rows separated by a white gap so each becomes one OCR line
    let gap = g.ch; // a full row of white between rows keeps tesseract's lines separate
    let (iw, ih) = (g.cols * g.cw, rows.len() * (g.ch + gap));
    let mut img = vec![255u8; iw * ih];
    for (n, &r) in rows.iter().enumerate() {
        for y in 0..g.ch {
            let sy = r * g.ch + y;
            let dy = n * (g.ch + gap) + y;
            for x in 0..iw {
                if cells.ink[sy * cells.width + x] {
                    img[dy * iw + x] = 0;
                }
            }
        }
    }
    let handles: Vec<_> = scales
        .iter()
        .map(|&s| {
            let img = img.clone();
            thread::spawn(move || -> Result<String, String> {
                let big = bilinear(&img, iw, ih, s);
                let (bw, bh) = (iw * s, ih * s);
                let mut pgm = format!("P5\n{bw} {bh}\n255\n").into_bytes();
                pgm.extend_from_slice(&big);
                let mut child = Command::new("tesseract")
                    .args(["stdin", "stdout", "--psm", "6", "-c", "preserve_interword_spaces=1"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| match e.kind() {
                        std::io::ErrorKind::NotFound => {
                            "tesseract not found (sudo apt install tesseract-ocr, or bobcat-cli --check --install)".to_string()
                        }
                        _ => e.to_string(),
                    })?;
                let mut stdin = child.stdin.take().ok_or("no stdin")?;
                let writer = thread::spawn(move || {
                    let _ = stdin.write_all(&pgm);
                });
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                let _ = writer.join();
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            })
        })
        .collect();
    let mut votes = Votes::new();
    let mut first_err = None;
    for h in handles {
        match h.join() {
            Ok(Ok(text)) => {
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                if lines.len() != rows.len() {
                    continue;
                }
                for (&r, line) in rows.iter().zip(lines) {
                    let chars: Vec<char> = line.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
                    let cols: Vec<usize> = (0..g.cols).filter(|&c| cells.keys[r * g.cols + c].is_some()).collect();
                    if chars.len() != cols.len() {
                        continue;
                    }
                    for (c, ch) in cols.into_iter().zip(chars) {
                        let key = cells.keys[r * g.cols + c].clone().unwrap();
                        *votes.entry(key).or_default().entry(ch).or_insert(0) += 1;
                    }
                }
            }
            Ok(Err(e)) => first_err = Some(e),
            Err(_) => {}
        }
    }
    match (votes.is_empty(), first_err) {
        (true, Some(e)) => Err(e),
        _ => Ok(votes),
    }
}

/// Learned shape -> character table.
#[derive(Default)]
pub struct Glyphs {
    votes: Votes,
    dict: HashMap<Shape, char>,
    near: HashMap<Shape, Option<char>>,
    /// shapes a calibration could not name (e.g. a glyph clipped at the screen edge)
    unreadable: std::collections::HashSet<Shape>,
    grid: Option<Grid>,
}

/// How common a character is in console text (rough): used to break OCR ties.
fn char_prior(c: char) -> f64 {
    if c.is_ascii_lowercase() || c.is_ascii_digit() {
        1.0
    } else if ".-/_:".contains(c) {
        0.8
    } else if c.is_ascii_uppercase() {
        0.4
    } else if ",=()[]<>+#$~*\"'".contains(c) {
        0.3
    } else if c == '?' {
        0.05
    } else {
        0.15
    }
}

fn hamming(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

impl Glyphs {
    /// Forget everything if the cell grid changed (a different font or video mode).
    pub fn set_grid(&mut self, grid: Grid) {
        if self.grid != Some(grid) {
            *self = Glyphs { grid: Some(grid), ..Default::default() };
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dict.is_empty()
    }

    pub fn votes(&self) -> &Votes {
        &self.votes
    }

    pub fn unreadable(&self) -> &std::collections::HashSet<Shape> {
        &self.unreadable
    }

    pub fn merge(&mut self, v: Votes) {
        for (shape, chars) in v {
            let e = self.votes.entry(shape).or_default();
            for (c, n) in chars {
                *e.entry(c).or_insert(0) += n;
            }
        }
        // One character <-> one shape. Score each (shape, character) by the share of that
        // shape's OCR votes it got, times how common the character is in console text:
        // OCR reads this font's zero as 6, @, 8 and only then 0, but @ is rare and 0 is
        // common, so the zero shape ends up as 0 while the real @ keeps its own shape.
        let totals: HashMap<&Shape, u32> = self.votes.iter().map(|(k, m)| (k, m.values().sum())).collect();
        let mut pairs: Vec<(f64, u32, &Shape, char)> = self
            .votes
            .iter()
            .flat_map(|(k, m)| {
                let total = totals[k] as f64;
                m.iter().map(move |(c, n)| (*n as f64 / total * char_prior(*c), *n, k, *c))
            })
            .collect();
        pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(b.1.cmp(&a.1)).then(a.3.cmp(&b.3)));
        let mut dict = HashMap::new();
        let mut used = std::collections::HashSet::new();
        for (_, _, shape, c) in pairs {
            if dict.contains_key(shape) || used.contains(&c) {
                continue;
            }
            dict.insert(shape.clone(), c);
            used.insert(c);
        }
        self.dict = dict;
        self.near.clear();
        let dict = &self.dict;
        self.unreadable.retain(|k| !dict.contains_key(k));
    }

    /// Remember these shapes as not nameable so they don't trigger more OCR runs.
    pub fn give_up_on(&mut self, shapes: Vec<Shape>) {
        self.unreadable.extend(shapes);
    }

    /// The character for a shape: exact match, else the closest known shape if it is
    /// nearly identical (a cursor drawn over a letter, a clipped glyph), else None.
    pub fn lookup(&mut self, key: &Shape) -> Option<char> {
        if let Some(c) = self.dict.get(key) {
            return Some(*c);
        }
        if let Some(c) = self.near.get(key) {
            return *c;
        }
        let limit = (key.len() as u32 * 8) / 16; // ~6% of the cell's pixels
        let found = self
            .dict
            .iter()
            .map(|(k, c)| (hamming(key, k), *c))
            .min_by_key(|(d, _)| *d)
            .filter(|(d, _)| *d <= limit)
            .map(|(_, c)| c);
        self.near.insert(key.clone(), found);
        found
    }
}

/// The screen as lines of text, plus how many cells could not be read (shown as '?').
pub fn render(cells: &Cells, glyphs: &mut Glyphs) -> (Vec<String>, usize, Vec<Shape>) {
    let g = cells.grid;
    let mut unknown = 0;
    let mut fresh: Vec<Shape> = Vec::new(); // unknown shapes we have not yet given up on
    let mut lines: Vec<String> = Vec::with_capacity(g.rows);
    for r in 0..g.rows {
        let mut line = String::with_capacity(g.cols);
        for c in 0..g.cols {
            match &cells.keys[r * g.cols + c] {
                None => line.push(' '),
                Some(k) => match glyphs.lookup(k) {
                    Some(ch) => line.push(ch),
                    None => {
                        unknown += 1;
                        if !glyphs.unreadable.contains(k) && !fresh.contains(k) {
                            fresh.push(k.clone());
                        }
                        line.push('?')
                    }
                },
            }
        }
        lines.push(line.trim_end().to_string());
    }
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }
    (lines, unknown, fresh)
}


// ---- keeping the learned table between runs ------------------------------------
// One entry per iDRAC in <cache>/bobcat/glyphs.json: the cell size and, for every
// shape (as hex), the characters it was voted to be. Votes are stored rather than the
// final table so later runs keep adding evidence; loaded counts are capped so fresh
// evidence can still outvote a wrong guess (and `--relearn` throws the entry away).

fn cache_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("bobcat").join("glyphs.json")
}

/// The old name's file (dracon-cli), still read when the new one does not exist yet.
fn legacy_cache_path() -> std::path::PathBuf {
    let mut p = cache_path();
    p.pop();
    p.pop();
    p.join("dracon-cli").join("glyphs.json")
}

fn saved_text() -> Option<String> {
    std::fs::read_to_string(cache_path()).or_else(|_| std::fs::read_to_string(legacy_cache_path())).ok()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// The saved cell size and votes for this iDRAC, if any.
pub fn load_saved(host: &str) -> Option<((usize, usize), Votes, Vec<Shape>)> {
    let text = saved_text()?;
    let root: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = root.get(host)?;
    let cw = entry.get("cw")?.as_u64()? as usize;
    let ch = entry.get("ch")?.as_u64()? as usize;
    let mut votes = Votes::new();
    for (hex, chars) in entry.get("glyphs")?.as_object()? {
        let shape = from_hex(hex)?;
        if shape.len() != (cw * ch + 7) / 8 {
            return None; // inconsistent: ignore the whole entry
        }
        for (c, n) in chars.as_object()? {
            let c = c.chars().next()?;
            let n = (n.as_u64()? as u32).min(8);
            votes.entry(shape.clone()).or_default().insert(c, n);
        }
    }
    let unreadable = entry
        .get("unreadable")
        .and_then(|u| u.as_array())
        .map(|a| a.iter().filter_map(|h| h.as_str().and_then(from_hex)).collect())
        .unwrap_or_default();
    Some(((cw, ch), votes, unreadable))
}

/// Save this iDRAC's votes (other iDRACs' entries are kept).
pub fn save(host: &str, cw: usize, ch: usize, votes: &Votes, unreadable: &std::collections::HashSet<Shape>) -> Result<(), String> {
    let path = cache_path();
    let mut root: serde_json::Map<String, serde_json::Value> = saved_text()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let mut glyphs = serde_json::Map::new();
    for (shape, chars) in votes {
        let mut m = serde_json::Map::new();
        for (c, n) in chars {
            m.insert(c.to_string(), serde_json::json!(n));
        }
        glyphs.insert(to_hex(shape), serde_json::Value::Object(m));
    }
    let unreadable: Vec<String> = unreadable.iter().map(|k| to_hex(k)).collect();
    root.insert(host.to_string(), serde_json::json!({ "cw": cw, "ch": ch, "glyphs": glyphs, "unreadable": unreadable }));
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&serde_json::Value::Object(root)).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}
