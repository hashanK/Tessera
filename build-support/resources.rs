// Shared by the build scripts (`include!`d, not a crate). Writes a compiled resource file
// (.res) holding the Tessera icon (from `assets/tessera.ico`) and a version block, so the
// executables get their icon in Explorer, the Start menu and the taskbar, and a proper name in
// Task Manager. The MSVC linker accepts .res files directly, so no resource compiler is needed.
//
// `assets/tessera.ico` is made from `assets/icon.png` (see assets/README.md).

/// Builds `<OUT_DIR>/<file>.res` and tells the linker to embed it in this package's binaries.
#[allow(dead_code)]
fn embed_resources(description: &str, file_name: &str) {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(format!("{file_name}.res"));
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    std::fs::write(&out, resources(description, file_name, &version)).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
    println!("cargo:rerun-if-changed=../../build-support/resources.rs");
    println!("cargo:rerun-if-changed=../../assets/tessera.ico");
}

/// The images in an .ico file: (width, height, bits per pixel, image bytes). A size of 0 means 256.
fn ico_images(ico: &[u8]) -> Vec<(u8, u8, u16, &[u8])> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]) as usize;
    assert!(ico.len() > 6 && u16_at(2) == 1, "assets/tessera.ico is not an icon file");
    (0..u16_at(4) as usize)
        .map(|n| {
            let e = 6 + 16 * n;
            let (len, offset) = (u32_at(e + 8), u32_at(e + 12));
            (ico[e], ico[e + 1], u16_at(e + 6), &ico[offset..offset + len])
        })
        .collect()
}

fn resources(description: &str, file_name: &str, version: &str) -> Vec<u8> {
    let mut res = Vec::new();
    // A .res file starts with an empty entry.
    res_entry(&mut res, 0, 0, &[], 0, 0);
    let root = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let ico = std::fs::read(root.join("assets/tessera.ico")).expect("assets/tessera.ico is missing");
    let images = ico_images(&ico);
    let mut group = vec![0, 0, 1, 0];
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, &(w, h, bits, image)) in images.iter().enumerate() {
        let id = i as u16 + 1;
        res_entry(&mut res, 3, id, image, 0x1010, 0x0409); // RT_ICON
        group.extend_from_slice(&[w, h, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&bits.to_le_bytes());
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    res_entry(&mut res, 14, 1, &group, 0x1030, 0x0409); // RT_GROUP_ICON
    res_entry(&mut res, 16, 1, &version_info(description, file_name, version), 0x0030, 0x0409); // RT_VERSION
    res
}

/// One resource with a numeric type and name. The header is a fixed 32 bytes in that case.
fn res_entry(res: &mut Vec<u8>, kind: u16, name: u16, data: &[u8], flags: u16, lang: u16) {
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes());
    for v in [0xFFFF, kind, 0xFFFF, name] {
        res.extend_from_slice(&u16::to_le_bytes(v));
    }
    res.extend_from_slice(&0u32.to_le_bytes()); // data version
    res.extend_from_slice(&flags.to_le_bytes());
    res.extend_from_slice(&lang.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // version
    res.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    res.extend_from_slice(data);
    while res.len() % 4 != 0 {
        res.push(0);
    }
}

/// VS_VERSIONINFO. Task Manager shows FileDescription as the process name.
fn version_info(description: &str, file_name: &str, version: &str) -> Vec<u8> {
    let mut parts = version.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let v: [u16; 4] = [parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0), 0];
    let ms = (v[0] as u32) << 16 | v[1] as u32;
    let ls = (v[2] as u32) << 16 | v[3] as u32;
    let mut fixed = Vec::new();
    for d in [0xFEEF04BDu32, 0x0001_0000, ms, ls, ms, ls, 0x3F, 0, 0x0004_0004, 1, 0, 0, 0] {
        fixed.extend_from_slice(&d.to_le_bytes());
    }
    let dotted = format!("{}.{}.{}.0", v[0], v[1], v[2]);
    let author = std::env::var("CARGO_PKG_AUTHORS").unwrap_or_default().replace(':', ", ");
    let copyright = format!("\u{a9} 2026 {author}. Free software under the MIT License.");
    let strings: Vec<Vec<u8>> = [
        ("CompanyName", author.as_str()),
        ("FileDescription", description),
        ("FileVersion", dotted.as_str()),
        ("InternalName", file_name),
        ("LegalCopyright", copyright.as_str()),
        ("OriginalFilename", &format!("{file_name}.exe")),
        ("ProductName", "Tessera"),
        ("ProductVersion", version),
    ]
    .iter()
    .map(|(k, s)| {
        let value = wide(s);
        version_node(k, &value, (value.len() / 2) as u16, 1, &[])
    })
    .collect();
    let table = version_node("040904B0", &[], 0, 1, &strings);
    let string_info = version_node("StringFileInfo", &[], 0, 1, &[table]);
    let translation = version_node("Translation", &[0x09, 0x04, 0xB0, 0x04], 4, 0, &[]);
    let var_info = version_node("VarFileInfo", &[], 0, 1, &[translation]);
    version_node("VS_VERSION_INFO", &fixed, fixed.len() as u16, 0, &[string_info, var_info])
}

fn version_node(key: &str, value: &[u8], value_len: u16, kind: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut n = vec![0, 0];
    n.extend_from_slice(&value_len.to_le_bytes());
    n.extend_from_slice(&kind.to_le_bytes());
    n.extend_from_slice(&wide(key));
    let pad = |n: &mut Vec<u8>| n.resize(n.len().div_ceil(4) * 4, 0);
    pad(&mut n);
    n.extend_from_slice(value);
    for child in children {
        pad(&mut n);
        n.extend_from_slice(child);
    }
    let len = n.len() as u16;
    n[0..2].copy_from_slice(&len.to_le_bytes());
    n
}

/// UTF-16LE with a terminating null.
fn wide(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}
