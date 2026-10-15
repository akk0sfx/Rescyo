//! Generates Rust constants and a Slint global from `assets/design/tokens.toml`.

use std::error::Error;
use std::fmt::Write as _;
use std::path::PathBuf;

const COLORS: [&str; 14] = [
    "ink-0",
    "ink-1",
    "ink-2",
    "ink-3",
    "line",
    "text",
    "text-2",
    "text-3",
    "rescue",
    "rescue-hi",
    "ok",
    "warn",
    "bad",
    "info",
];

fn rgba(hex: &str) -> Result<[u8; 4], Box<dyn Error>> {
    let h = hex.strip_prefix('#').ok_or("colour without #")?;
    let byte = |i: usize| -> Result<u8, Box<dyn Error>> {
        Ok(u8::from_str_radix(h.get(i..i + 2).ok_or("short colour")?, 16)?)
    };
    match h.len() {
        6 => Ok([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Ok([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => Err(format!("bad colour {hex}").into()),
    }
}

fn ident(name: &str) -> String {
    name.replace('-', "_").to_uppercase()
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let path = manifest.join("../../assets/design/tokens.toml");
    println!("cargo::rerun-if-changed={}", path.display());
    let doc: toml::Table = std::fs::read_to_string(&path)?.parse()?;
    let table = |k: &str| -> Result<&toml::Table, Box<dyn Error>> {
        Ok(doc
            .get(k)
            .and_then(|v| v.as_table())
            .ok_or(format!("missing [{k}]"))?)
    };
    let colors = table("color")?;
    let mut rs = String::from("// Generated from assets/design/tokens.toml by build.rs. Do not edit.\n\n");
    let mut slint = String::from(
        "// Generated from assets/design/tokens.toml by rescyo-design. Do not edit.\n\nexport global Tokens {\n    in-out property <bool> dark: true;\n",
    );
    for theme in ["dark", "light"] {
        let t = colors
            .get(theme)
            .and_then(|v| v.as_table())
            .ok_or(format!("missing [color.{theme}]"))?;
        writeln!(
            rs,
            "/// The {theme} palette.\npub const {}: Palette = Palette {{",
            theme.to_uppercase()
        )?;
        for name in COLORS {
            let hex = t
                .get(name)
                .and_then(|v| v.as_str())
                .ok_or(format!("{theme}.{name} missing"))?;
            let [r, g, b, a] = rgba(hex)?;
            writeln!(
                rs,
                "    {}: Rgba {{ r: {r}, g: {g}, b: {b}, a: {a} }},",
                name.replace('-', "_")
            )?;
        }
        rs.push_str("};\n\n");
    }
    // Slint: one property per colour switching on `dark`.
    let dark = colors.get("dark").and_then(|v| v.as_table()).ok_or("no dark")?;
    let light = colors.get("light").and_then(|v| v.as_table()).ok_or("no light")?;
    for name in COLORS {
        let d = rgba(dark.get(name).and_then(|v| v.as_str()).ok_or("dark")?)?;
        let l = rgba(light.get(name).and_then(|v| v.as_str()).ok_or("light")?)?;
        let hex = |c: [u8; 4]| format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]);
        writeln!(
            slint,
            "    out property <color> {name}: dark ? {} : {};",
            hex(d),
            hex(l)
        )?;
    }
    for (section, unit) in [("radius", "px"), ("space", "px")] {
        rs.push_str(&format!("/// {section} tokens in px.\npub mod {section} {{\n"));
        for (k, v) in table(section)? {
            let n = v.as_integer().ok_or(format!("{section}.{k} not an integer"))?;
            writeln!(rs, "    pub const {}: u32 = {n};", ident(k))?;
            writeln!(slint, "    out property <length> {section}-{k}: {n}{unit};")?;
        }
        rs.push_str("}\n\n");
    }
    let ty = table("type")?;
    let scale: Vec<i64> = ty
        .get("scale")
        .and_then(|v| v.as_array())
        .ok_or("type.scale")?
        .iter()
        .filter_map(toml::Value::as_integer)
        .collect();
    writeln!(
        rs,
        "/// The type scale in px.\npub const TYPE_SCALE: [u32; {}] = {scale:?};",
        scale.len()
    )?;
    for k in ["body", "eyebrow"] {
        let n = ty
            .get(k)
            .and_then(|v| v.as_integer())
            .ok_or(format!("type.{k}"))?;
        writeln!(rs, "pub const TYPE_{}: u32 = {n};", ident(k))?;
        writeln!(slint, "    out property <length> type-{k}: {n}px;")?;
    }
    for (i, n) in scale.iter().enumerate() {
        writeln!(slint, "    out property <length> type-{i}: {n}px;")?;
    }
    let tracking = ty
        .get("eyebrow-tracking")
        .and_then(|v| v.as_float())
        .ok_or("eyebrow-tracking")?;
    writeln!(rs, "pub const EYEBROW_TRACKING_EM: f32 = {tracking:?};")?;
    writeln!(slint, "    out property <float> eyebrow-tracking: {tracking:?};")?;
    for (k, v) in table("font")? {
        let f = v.as_str().ok_or(format!("font.{k}"))?;
        writeln!(rs, "pub const FONT_{}: &str = {f:?};", ident(k))?;
        writeln!(slint, "    out property <string> font-{k}: {f:?};")?;
    }
    slint.push_str("}\n");
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    std::fs::write(out.join("tokens.rs"), rs)?;
    std::fs::write(out.join("tokens.slint"), slint)?;
    Ok(())
}
