//! gen-bigfont — dev-time pipeline that converts the official
//! fusion-pixel-font 12px monospaced BDF (release zip, sha256-pinned below)
//! into marquee's embedded bigfont asset. See docs/bigfont-asset-format.md
//! for the asset format and the regeneration procedure.

mod asset;
mod bdf;
mod charset;

use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::process::ExitCode;

use asset::{AssetView, GlyphOut, WIDTH_FULL, WIDTH_HALF, compose, write_asset};
use sha2::{Digest, Sha256};

/// The one font release this tool is pinned to. Regenerating from a newer
/// release means updating these constants, the docs and the recorded
/// sha256 — deliberately not something the tool does silently.
const RELEASE: &str = "2026.09.25";
const ZIP_NAME: &str = "fusion-pixel-font-12px-monospaced-bdf-v2026.09.25.zip";
const ZIP_SHA256: &str = "d75f5262f108757edb0f47ee8e3d2dfdfbecfd94558faf5ffb0c06dc5866fb3b";

/// Checklist budget for the committed asset (docs/big-font-mode.md §3.2).
const SIZE_BUDGET_BYTES: usize = 250_000;

const USAGE: &str = "\
gen-bigfont — generate marquee's embedded bigfont asset from fusion-pixel-font

USAGE:
  gen-bigfont [OPTIONS] <font-zip>        verify the pinned release zip, parse
                                          the variant BDF, write the asset
  gen-bigfont --dump <asset.bin> <text>   print the glyphs of <text> from a
                                          generated asset (dev check)

OPTIONS:
  --variant <zh-hans|ja|zh-hant>  BDF language variant (default: zh-hans)
  -o, --output <path>             default: assets/bigfont-12px-<variant>.bin
  -h, --help                      print this help
";

fn variant_bdf_name(variant: &str) -> Option<&'static str> {
    match variant {
        "zh-hans" => Some("fusion-pixel-12px-monospaced-zh_hans.bdf"),
        "zh-hant" => Some("fusion-pixel-12px-monospaced-zh_hant.bdf"),
        "ja" => Some("fusion-pixel-12px-monospaced-ja.bdf"),
        _ => None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return if args.is_empty() {
            Err("no arguments".to_string())
        } else {
            Ok(())
        };
    }

    if args[0] == "--dump" {
        if args.len() != 3 {
            return Err("--dump needs exactly <asset.bin> <text>".to_string());
        }
        return dump(&args[1], &args[2]);
    }

    let mut variant = "zh-hans".to_string();
    let mut output: Option<String> = None;
    let mut zip_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--variant" => {
                i += 1;
                variant = args.get(i).ok_or("--variant needs a value")?.clone();
            }
            "-o" | "--output" => {
                i += 1;
                output = Some(args.get(i).ok_or("--output needs a value")?.clone());
            }
            other => {
                if zip_path.is_some() {
                    return Err(format!("unexpected extra argument {other:?}"));
                }
                zip_path = Some(other.to_string());
            }
        }
        i += 1;
    }
    let zip_path = zip_path.ok_or("missing <font-zip> argument")?;
    let bdf_name =
        variant_bdf_name(&variant).ok_or_else(|| format!("unknown variant {variant:?}"))?;
    let output = output.unwrap_or_else(|| format!("assets/bigfont-12px-{variant}.bin"));

    generate(&zip_path, &variant, bdf_name, &output)
}

fn generate(zip_path: &str, variant: &str, bdf_name: &str, output: &str) -> Result<(), String> {
    let zip_bytes = std::fs::read(zip_path).map_err(|e| format!("cannot read {zip_path}: {e}"))?;
    let zip_hash = sha256_hex(&zip_bytes);
    if !zip_hash.eq_ignore_ascii_case(ZIP_SHA256) {
        return Err(format!(
            "{zip_path} is not the pinned release {RELEASE}\n  expected sha256 {ZIP_SHA256}\n  got      sha256 {zip_hash}\n\
             To move to a new release, update RELEASE/ZIP_NAME/ZIP_SHA256 in tools/gen-bigfont/src/main.rs and the records in docs/bigfont-asset-format.md."
        ));
    }
    println!("source: {ZIP_NAME} (release {RELEASE})");
    println!("  zip sha256 verified: {zip_hash}");

    let bdf_text = read_bdf_from_zip(&zip_bytes, bdf_name)?;
    let font = bdf::parse(&bdf_text)?;
    if font.pixel_size != 12 {
        return Err(format!(
            "{bdf_name}: PIXEL_SIZE {} — the asset format is built for 12px",
            font.pixel_size
        ));
    }
    if font.ascent + font.descent != i32::from(asset::GLYPH_HEIGHT) {
        return Err(format!(
            "{bdf_name}: FONT_ASCENT {} + FONT_DESCENT {} != {} (canvas height)",
            font.ascent,
            font.descent,
            asset::GLYPH_HEIGHT
        ));
    }
    println!(
        "  parsed {bdf_name}: {} glyphs, ascent {} descent {}",
        font.glyphs.len(),
        font.ascent,
        font.descent
    );

    let mut by_encoding: HashMap<u32, &bdf::BdfGlyph> = HashMap::new();
    for g in &font.glyphs {
        if let Some(prev) = by_encoding.insert(g.encoding, g) {
            return Err(format!(
                "duplicate ENCODING {} ({} and {})",
                g.encoding, prev.name, g.name
            ));
        }
    }

    let wanted: BTreeSet<char> = charset::charset_v1();
    let mut glyphs: Vec<GlyphOut> = Vec::with_capacity(wanted.len());
    let mut missing: Vec<char> = Vec::new();
    let mut skipped_width: Vec<(char, u32)> = Vec::new();
    let mut clipped_glyphs: Vec<(char, u32)> = Vec::new();
    let mut clipped_total: u32 = 0;
    for ch in wanted.iter().copied() {
        let Some(g) = by_encoding.get(&(ch as u32)) else {
            missing.push(ch);
            continue;
        };
        let width_px = match g.dwidth_x {
            6 => WIDTH_HALF,
            12 => WIDTH_FULL,
            other => {
                skipped_width.push((ch, other));
                continue;
            }
        };
        let (rows, clipped) = compose(g, font.ascent, width_px);
        clipped_total += clipped;
        if clipped > 0 {
            clipped_glyphs.push((ch, clipped));
        }
        glyphs.push(GlyphOut { ch, width_px, rows });
    }

    let half = glyphs.iter().filter(|g| g.width_px == WIDTH_HALF).count();
    println!("charset: {} chars wanted", wanted.len());
    println!(
        "  selected {} glyphs ({half} halfwidth 6px, {} fullwidth 12px)",
        glyphs.len(),
        glyphs.len() - half
    );
    println!("  missing from font: {}", missing.len());
    if !missing.is_empty() {
        let shown: String = missing.iter().take(10).collect();
        println!("    first 10: {shown}");
    }
    if !skipped_width.is_empty() {
        println!("  skipped (unsupported DWIDTH): {}", skipped_width.len());
        for (ch, w) in skipped_width.iter().take(10) {
            println!("    U+{:04X} {ch}: DWIDTH {w}", *ch as u32);
        }
    }
    println!("  pixels clipped to canvas: {clipped_total}");
    for (ch, n) in clipped_glyphs.iter().take(10) {
        println!(
            "    U+{:04X} {ch}: {n}px outside the canvas (clipped)",
            *ch as u32
        );
    }

    let provenance = format!(
        "source=fusion-pixel-font release={RELEASE} file={ZIP_NAME} sha256={ZIP_SHA256} variant={variant} generator=gen-bigfont {}",
        env!("CARGO_PKG_VERSION")
    );
    let bytes = write_asset(&glyphs, &provenance)?;
    self_check(&bytes, &glyphs, &provenance)?;

    if let Some(parent) = std::path::Path::new(output).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(output, &bytes).map_err(|e| format!("cannot write {output}: {e}"))?;

    println!("asset: {output}");
    println!(
        "  self-check: header, index order and all {} bitmaps re-read ok",
        glyphs.len()
    );
    if bytes.len() > SIZE_BUDGET_BYTES {
        eprintln!(
            "WARNING: {} bytes is over the {}-byte budget",
            bytes.len(),
            SIZE_BUDGET_BYTES
        );
    }
    println!("  size: {} bytes (budget {SIZE_BUDGET_BYTES})", bytes.len());
    println!("  asset sha256: {}", sha256_hex(&bytes));
    println!("  provenance: {provenance}");
    println!(
        "done — record the new asset sha256/size in docs/bigfont-asset-format.md if either changed"
    );
    Ok(())
}

fn read_bdf_from_zip(zip_bytes: &[u8], bdf_name: &str) -> Result<String, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("not a readable zip: {e}"))?;
    let mut entry = archive
        .by_name(bdf_name)
        .map_err(|_| format!("{bdf_name} not found inside the release zip (variant mismatch?)"))?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read {bdf_name}: {e}"))?;
    Ok(text)
}

/// Re-parse the generated bytes and compare against what we meant to write,
/// so a corrupted asset can never be committed by a green run.
fn self_check(bytes: &[u8], glyphs: &[GlyphOut], provenance: &str) -> Result<(), String> {
    let view = AssetView::parse(bytes)?;
    if view.glyph_count() != glyphs.len() {
        return Err("self-check: glyph count mismatch".to_string());
    }
    if view.provenance() != provenance {
        return Err("self-check: provenance mismatch".to_string());
    }
    for (i, g) in glyphs.iter().enumerate() {
        if view.char_at(i) != g.ch as u32 || view.width_at(i) != g.width_px {
            return Err(format!("self-check: index entry {i} mismatch"));
        }
        let rows: Vec<u16> = view.rows_at(i).collect();
        if rows != g.rows {
            return Err(format!("self-check: bitmap mismatch for {:?}", g.ch));
        }
    }
    Ok(())
}

fn dump(asset_path: &str, text: &str) -> Result<(), String> {
    let bytes = std::fs::read(asset_path).map_err(|e| format!("cannot read {asset_path}: {e}"))?;
    let view = AssetView::parse(&bytes)?;
    println!(
        "{asset_path}: {} glyphs, provenance: {}",
        view.glyph_count(),
        view.provenance()
    );
    for ch in text.chars() {
        println!("U+{:04X} {ch}:", ch as u32);
        match view.find(ch) {
            None => println!("  (absent — tofu at runtime)"),
            Some(i) => {
                let width = usize::from(view.width_at(i));
                for row in view.rows_at(i) {
                    let line: String = (0..width)
                        .map(|c| if row & (1 << (15 - c)) != 0 { '#' } else { '.' })
                        .collect();
                    println!("  {line}");
                }
            }
        }
    }
    Ok(())
}
