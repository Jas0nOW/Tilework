fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/tilework.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let manifest = root.join("app.manifest");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        // The app icon (Start menu, Explorer, Apps list) is linked from a .res written here, so no
        // resource compiler or extra crate is needed. link.exe accepts .res files as inputs.
        let ico = std::fs::read(root.join("assets/tilework.ico")).expect("assets/tilework.ico");
        let res = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("tilework-icon.res");
        std::fs::write(&res, icon_res(&ico)).expect("write icon resource");
        println!("cargo:rustc-link-arg-bins={}", res.display());
        if std::env::var("PROFILE").as_deref() == Ok("release") {
            println!("cargo:rustc-link-arg=/DEBUG:NONE");
            println!("cargo:rustc-link-arg=/Brepro");
        }
    }
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// Resource id 1 is the first icon group, which Windows shows as the executable's icon.
const ICON_GROUP_ID: u16 = 1;

/// Converts an .ico file into a compiled resource file (.res) holding one RT_GROUP_ICON and one
/// RT_ICON per image. Format: https://learn.microsoft.com/windows/win32/menurc/resourceheader
fn icon_res(ico: &[u8]) -> Vec<u8> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    assert!(ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1, "not an .ico file");
    let count = u16_at(4) as usize;
    let mut out = Vec::new();
    entry(&mut out, 0, 0, &[]); // a .res file starts with an empty entry
    let mut group = Vec::new();
    group.extend_from_slice(&[0, 0, 1, 0]);
    group.extend_from_slice(&(count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + 16 * i;
        let (size, offset) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
        let image = ico.get(offset..offset + size).expect("truncated .ico");
        let id = (i + 1) as u16;
        entry(&mut out, RT_ICON, id, image);
        group.extend_from_slice(&ico[e..e + 12]); // width, height, colors, reserved, planes, bit count, bytes
        group.extend_from_slice(&id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, ICON_GROUP_ID, &group);
    out
}

fn entry(out: &mut Vec<u8>, kind: u16, name: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes()); // header size
    out.extend_from_slice(&[0xFF, 0xFF]);
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&[0xFF, 0xFF]);
    out.extend_from_slice(&name.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // data version
    let flags: u16 = if kind == 0 { 0 } else { 0x1030 }; // MOVEABLE | PURE | DISCARDABLE
    out.extend_from_slice(&flags.to_le_bytes());
    let language: u16 = if kind == 0 { 0 } else { 0x0409 };
    out.extend_from_slice(&language.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // version
    out.extend_from_slice(&0u32.to_le_bytes()); // characteristics
    out.extend_from_slice(data);
    while !out.len().is_multiple_of(4) { out.push(0); }
}
