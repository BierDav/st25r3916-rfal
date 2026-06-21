fn main() {
    println!("cargo:rerun-if-changed=rfal/");
    println!("cargo:rerun-if-changed=src/rfal_platform.h");

    if cfg!(feature = "compile-c") {
        compile_c();
    }

    generate_bindings();
}

/// Returns the ST25R transceiver chip variant name defined by Cargo features.
///
/// Mutually exclusive features:
/// - `st25r3916` -> defines `ST25R3916`
/// - `st25r3916b` -> defines `ST25R3916B`
///
/// Returns one of the define strings, or panics if zero or more than one are active.
fn get_chip_variant() -> &'static str {
    let st25r3916 = cfg!(feature = "st25r3916");
    let st25r3916b = cfg!(feature = "st25r3916b");
    match (st25r3916, st25r3916b) {
        (true, false) => "ST25R3916",
        (false, true) => "ST25R3916B",
        (false, false) => panic!(
            "st25r3916-rfal error: Exactly one chip variant feature must be enabled. \
             Neither 'st25r3916' nor 'st25r3916b' is enabled. \
             Please enable either 'st25r3916' or 'st25r3916b' (or use default features)."
        ),
        (true, true) => {
            println!("cargo:warning=st25r3916-rfal: Both 'st25r3916' and 'st25r3916b' features are enabled. Defaulting to ST25R3916B to prevent build failures during cargo sync/indexing.");
            "ST25R3916B"
        }
    }
}

/// Compiles the RFAL C library.
///
/// # Note on compiler discovery:
/// Non-upstream-LLVM targets (such as Xtensa-based ESP32 chips today) do not have
/// built-in naming conventions in Cargo's `cc` crate. Consumers targeting these
/// platforms must supply `CC_<target>` and `AR_<target>` environment variables (or
/// equivalent `.cargo/config.toml` target entries) pointing to the custom GCC toolchain.
/// If these variables are not set, `build.rs` will bubble up `cc`'s compilation error
/// instead of silently using the host compiler. Upstream-LLVM targets (such as ARM
/// thumbv* and RISC-V riscv32*) are resolved automatically by `cc` using default naming tables.
fn compile_c() {
    let mut builder = cc::Build::new();

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("xtensa") {
        builder.flag("-mlongcalls");
    }

    builder
        .pic(false)
        .include("rfal/inc")
        .include("rfal/src")
        .include("rfal/src/st25r3916")
        .include("src")
        .flag("-Wno-type-limits");

    let variant = get_chip_variant();
    builder.define(variant, None);
    builder.file("src/platform_log.c");

    let rfal_files = glob::glob("rfal/src/**/*.c").expect("Failed to read glob pattern");

    for entry in rfal_files {
        match entry {
            Ok(path) => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                if filename == "st25r3916_led.c" || filename == "st25r3916_aat.c" {
                    continue;
                }
                builder.file(&path);
            }
            Err(e) => println!("cargo:warning=Glob error: {:?}", e),
        }
    }

    builder.compile("rfal");
}

struct TargetBindgenConfig {
    /// Explicit `clang -target=...` to pass. `None` means let bindgen infer
    /// from the normal Rust target triple (correct for any target that is
    /// upstream LLVM — which is most non-Xtensa embedded targets: ARM
    /// Cortex-M `thumbv*`, RISC-V `riscv32*`, etc).
    clang_target: Option<String>,
    /// Extra `-I` directories needed only for this target's SDK/toolchain
    /// headers. Empty for targets that don't need anything beyond the
    /// crate's own include dirs.
    extra_include_dirs: Vec<std::path::PathBuf>,
}

fn resolve_target_bindgen_config(target_triple: &str) -> TargetBindgenConfig {
    if target_triple.contains("xtensa") || target_triple.contains("riscv32") {
        esp_bindgen_config(target_triple)
    } else {
        TargetBindgenConfig {
            clang_target: None,
            extra_include_dirs: vec![],
        }
    }
}

fn esp_bindgen_config(target_triple: &str) -> TargetBindgenConfig {
    let (mcu, arch_prefix) = if target_triple.contains("xtensa") {
        let mcu = if let Some(idx) = target_triple.find("xtensa-") {
            let after_xtensa = &target_triple[idx + "xtensa-".len()..];
            after_xtensa.split('-').next().unwrap_or("esp32s3")
        } else {
            "esp32s3"
        };
        (mcu, "xtensa")
    } else {
        let mcu = if let Some(idx) = target_triple.find("riscv32imac-esp-") {
            let after_riscv = &target_triple[idx + "riscv32imac-esp-".len()..];
            after_riscv.split('-').next().unwrap_or("esp32c6")
        } else {
            "esp32c6"
        };
        (mcu, "riscv32")
    };

    let clang_target = if arch_prefix == "xtensa" {
        format!("xtensa-{}-elf", mcu)
    } else {
        format!("riscv32-{}-elf", mcu)
    };
    
    let mut extra_include_dirs = Vec::new();

    if let Ok(esp_idf_path) = std::env::var("ESP_IDF_PATH") {
        extra_include_dirs.push(std::path::PathBuf::from(format!(
            "{}/components/newlib/platform_include",
            esp_idf_path
        )));
    }

    if let Some(include_dir) = find_esp_include_dir(target_triple) {
        extra_include_dirs.push(include_dir);
    } else {
        println!(
            "cargo:warning=Could not find ESP toolchain headers. Bindgen may fail if standard headers are missing."
        );
    }

    TargetBindgenConfig {
        clang_target: Some(clang_target),
        extra_include_dirs,
    }
}

fn generate_bindings() {
    let variant = get_chip_variant();
    let mut builder = bindgen::Builder::default()
        .header("rfal/inc/rfal_nfc.h")
        .header("rfal/inc/rfal_rf.h")
        .clang_arg("-Irfal/inc")
        .clang_arg("-Irfal/src")
        .clang_arg("-Irfal/src/st25r3916")
        .clang_arg("-Isrc")
        .clang_arg("-D__BINDGEN__")
        .clang_arg("-D__attribute__(x)=")
        .clang_arg("-D__extension__=")
        .clang_arg(format!("-D{}", variant))
        .use_core()
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    let target = std::env::var("TARGET").unwrap_or_default();
    let target_config = resolve_target_bindgen_config(&target);

    if let Some(ref clang_target) = target_config.clang_target {
        builder = builder.clang_arg(format!("--target={}", clang_target));
    }

    for dir in &target_config.extra_include_dirs {
        builder = builder.clang_arg(format!("-I{}", dir.display()));
    }

    let bindings = builder.generate().expect("Unable to generate bindings");
    let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bindings_path = out_path.join("bindings.rs");
    bindings
        .write_to_file(&bindings_path)
        .expect("Couldn't write bindings!");

    // --- Post-processing: strip const-check blocks ---
    let bindings_content =
        std::fs::read_to_string(&bindings_path).expect("Could not read generated bindings");

    let mut result = String::new();
    let mut in_const_block = false;
    let mut brace_depth = 0i32;

    for line in bindings_content.lines() {
        if line.contains("const _: () = {") {
            in_const_block = true;
            brace_depth = 1;
            continue;
        }
        if in_const_block {
            brace_depth += line.matches('{').count() as i32;
            brace_depth -= line.matches('}').count() as i32;
            if brace_depth == 0 {
                in_const_block = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }

    // --- Append RFAL_ERR constants extracted from the header ---
    let header_content =
        std::fs::read_to_string("rfal/inc/rfal_utils.h").expect("Could not read rfal_utils.h");

    result.push_str("\n// --- Dynamically Extracted RFAL Errors ---\n");
    for line in header_content.lines() {
        if line.starts_with("#define RFAL_ERR_") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let name = parts[1];
                let val_str = parts[2];
                let number: String = val_str.chars().filter(|c| c.is_ascii_digit()).collect();
                if !number.is_empty() {
                    result.push_str(&format!("pub const {}: u16 = {};\n", name, number));
                }
            }
        }
    }

    std::fs::write(&bindings_path, result).expect("Could not write filtered bindings");

    // --- Generate Rust type wrappers from the processed bindings ---
    generate_type_wrappers(&bindings_path, &out_path);
}

// ---------------------------------------------------------------------------
// Type-wrapper generator
// ---------------------------------------------------------------------------

fn generate_type_wrappers(bindings_path: &std::path::Path, out_dir: &std::path::Path) {
    use std::collections::HashMap;

    let content =
        std::fs::read_to_string(bindings_path).expect("Failed to read bindings for type gen");

    // Step 1: collect all `pub type rfal<Name> = ::core::ffi::c_{uint|int|...};`
    let mut type_repr: HashMap<String, &'static str> = HashMap::new();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("pub type rfal") {
            if let Some((name_suffix, rhs)) = rest.split_once(" = ") {
                let type_name = format!("rfal{}", name_suffix.trim());
                let repr = if rhs.contains("c_uint") || rhs.contains("c_ulong") {
                    "u32"
                } else if rhs.contains("c_int") {
                    "i32"
                } else if rhs.contains("c_ushort") {
                    "u16"
                } else if rhs.contains("c_short") {
                    "i16"
                } else {
                    continue; // skip function pointers, struct aliases, etc.
                };
                type_repr.insert(type_name, repr);
            }
        }
    }

    // Step 2: collect `pub const {TypeName}_{SUFFIX}: {TypeName} = N;` for each type
    let mut type_constants: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    for line in content.lines() {
        if !line.starts_with("pub const ") {
            continue;
        }
        for type_name in type_repr.keys() {
            let intro = format!("pub const {}_", type_name);
            if let Some(rest) = line.strip_prefix(&intro) {
                if let Some((suffix, val_section)) = rest.split_once(": ") {
                    if let Some((_, raw_val)) = val_section.split_once(" = ") {
                        let raw_val = raw_val.trim_end_matches(';').trim();
                        if let Ok(val) = raw_val.parse::<i64>() {
                            type_constants
                                .entry(type_name.clone())
                                .or_default()
                                .push((suffix.to_string(), val));
                        }
                    }
                }
                break;
            }
        }
    }

    // Step 3: emit generated code
    let mut out = String::from(
        "// AUTO-GENERATED by build.rs — do not edit.\n\
         // Re-runs automatically on every `cargo build`.\n\n\
         use strum::{Display, EnumString, FromRepr};\n\n",
    );

    let mut names: Vec<String> = type_constants
        .keys()
        .filter(|t| !type_constants[*t].is_empty())
        .cloned()
        .collect();
    names.sort();

    for c_type in &names {
        let repr = type_repr[c_type];
        let rust_name = capitalize_first(c_type); // rfalMode -> RfalMode

        let mut variants: Vec<(String, i64)> = type_constants[c_type].clone();
        variants.sort_by_key(|(_, v)| *v);
        variants.dedup_by_key(|(_, v)| *v); // remove duplicate discriminants

        if variants.is_empty() {
            continue;
        }

        let suffix_strs: Vec<&str> = variants.iter().map(|(n, _)| n.as_str()).collect();
        let common = common_underscore_prefix(&suffix_strs);

        let fallback = common
            .trim_end_matches('_')
            .rsplit('_')
            .next()
            .map(|w| capitalize_word(w))
            .unwrap_or_default();

        // 1. Appended strum's Display, EnumString, and FromRepr macros here:
        out.push_str(&format!(
            "#[allow(dead_code)]\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, FromRepr)]\n\
             #[repr({})]\n\
             pub enum {} {{\n",
            repr, rust_name
        ));
        for (c_name, value) in &variants {
            let rem = c_name.strip_prefix(&common).unwrap_or(c_name);
            let variant = make_variant_name(rem, &fallback);
            out.push_str(&format!("    {} = {},\n", variant, value));
        }
        out.push_str("}\n\n");

        // 2. Erased the manual TryFrom block! Strum's FromRepr handles it natively.
        // If you still want the TryFrom trait interface for polymorphism, you can wrap strum's from_repr:
        out.push_str(&format!(
            "impl TryFrom<{repr}> for {name} {{\n\
    \x20   type Error = ();\n\
    \x20   #[inline]\n\
    \x20   fn try_from(v: {repr}) -> Result<Self, ()> {{\n\
    \x20       Self::from_repr(v).ok_or(())\n\
    \x20   }}\n\
    }}\n\n",
            repr = repr,
            name = rust_name
        ));

        // From<RustName> for repr
        out.push_str(&format!(
            "impl From<{name}> for {repr} {{\n    #[inline]\n    fn from(v: {name}) -> {repr} {{ v as {repr} }}\n}}\n\n",
            name = rust_name,
            repr = repr
        ));
    }

    // Step 4: RfalError from RFAL_ERR_* u16 constants
    let mut errors: Vec<(String, u16)> = Vec::new();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("pub const RFAL_ERR_") {
            if let Some((name, val_part)) = rest.split_once(": u16 = ") {
                let val_str = val_part.trim_end_matches(';');
                if let Ok(val) = val_str.parse::<u16>() {
                    errors.push((screaming_snake_to_camel(name), val));
                }
            }
        }
    }

    if !errors.is_empty() {
        errors.sort_by_key(|(_, v)| *v);

        // Added strum derivations to RfalError
        out.push_str(
            "#[allow(dead_code)]\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, FromRepr)]\n\
             #[repr(u16)]\n\
             pub enum RfalError {\n",
        );
        for (name, val) in &errors {
            out.push_str(&format!("    {} = {},\n", name, val));
        }
        out.push_str("}\n\n");

        // Wrapped strum's FromRepr to fulfill the TryFrom trait cleanly
        out.push_str(
            "impl TryFrom<u16> for RfalError {\n\
    \x20   type Error = ();\n\
    \x20   #[inline]\n\
    \x20   fn try_from(v: u16) -> Result<Self, ()> {\n\
    \x20       Self::from_repr(v).ok_or(())\n\
    \x20   }\n\
    }\n\n",
        );
        out.push_str(
            "impl From<RfalError> for u16 {\n    #[inline]\n    fn from(v: RfalError) -> u16 { v as u16 }\n}\n",
        );
    }

    let out_path = out_dir.join("rfal_types_gen.rs");
    std::fs::write(&out_path, out).expect("Failed to write rfal_types_gen.rs");
}
// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// Capitalize only the first character, leave the rest unchanged.
/// `rfalMode` → `RfalMode`
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(ch) => ch.to_ascii_uppercase().to_string() + c.as_str(),
    }
}

/// Capitalize first char, lowercase the rest of a single word.
/// `HELLO` → `Hello`, `FSX` → `Fsx`
fn capitalize_word(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        None => String::new(),
        Some(ch) => ch.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
    }
}

/// Convert SCREAMING_SNAKE_CASE to CamelCase, one word at a time.
/// Words beginning with a digit are kept verbatim (digit + rest lowercased).
fn screaming_snake_to_camel(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) if c.is_ascii_alphabetic() => {
                    c.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                // digit-leading segment: keep digit, lowercase the rest
                Some(c) => c.to_string() + &chars.as_str().to_ascii_lowercase(),
            }
        })
        .collect()
}

/// Longest common prefix of all strings, trimmed to the last `_` boundary.
/// For a single-element slice the prefix is everything up to the last `_`.
fn common_underscore_prefix(strs: &[&str]) -> String {
    if strs.is_empty() {
        return String::new();
    }
    let first = strs[0];
    if strs.len() == 1 {
        return match first.rfind('_') {
            Some(pos) => first[..pos + 1].to_string(),
            None => String::new(),
        };
    }
    let common_len = strs[1..].iter().fold(first.len(), |acc, s| {
        let eq = first
            .chars()
            .zip(s.chars())
            .take_while(|(a, b)| a == b)
            .count();
        acc.min(eq)
    });
    let prefix = &first[..common_len];
    match prefix.rfind('_') {
        Some(pos) => prefix[..pos + 1].to_string(),
        None => String::new(),
    }
}

/// Convert the stripped constant suffix into a valid CamelCase Rust identifier.
/// If the name starts with a digit, `fallback_prefix` (derived from the common
/// prefix) is prepended so the identifier is valid.
fn make_variant_name(remaining: &str, fallback_prefix: &str) -> String {
    if remaining
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        format!("{}{}", fallback_prefix, screaming_snake_to_camel(remaining))
    } else {
        screaming_snake_to_camel(remaining)
    }
}

fn find_esp_include_dir(target_triple: &str) -> Option<std::path::PathBuf> {
    // 1. Try finding it from the active compiler in the environment
    let target_cc_var = format!("CC_{}", target_triple.replace('-', "_"));
    if let Ok(cc_val) = std::env::var(&target_cc_var).or_else(|_| std::env::var("CC")) {
        let cc_path = std::path::Path::new(&cc_val);
        if let Some(include_dir) = find_include_dir_from_compiler(cc_path) {
            return Some(include_dir);
        }
    }

    // 2. Try searching in parent directories of CARGO_MANIFEST_DIR
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let mut current = std::path::PathBuf::from(manifest_dir);
        loop {
            // Search in .embuild for both xtensa and riscv32
            for arch in &["xtensa-esp-elf", "riscv32-esp-elf"] {
                let embuild_pattern = current.join(format!(
                    ".embuild/espressif/tools/{}/*/{}/{}/include",
                    arch, arch, arch
                ));
                if let Some(path) = glob_first(&embuild_pattern.to_string_lossy()) {
                    return Some(path);
                }
            }

            // Search in .espressif
            for arch in &["xtensa-esp-elf", "riscv32-esp-elf"] {
                let espressif_pattern = current.join(format!(
                    ".espressif/tools/{}/*/{}/{}/include",
                    arch, arch, arch
                ));
                if let Some(path) = glob_first(&espressif_pattern.to_string_lossy()) {
                    return Some(path);
                }
            }

            if !current.pop() {
                break;
            }
        }
    }

    // 3. Fallback to user home directory
    let home = std::env::var("HOME").unwrap_or_default();

    for arch in &["xtensa-esp-elf", "riscv32-esp-elf"] {
        let home_espressif_pattern = format!(
            "{}/.espressif/tools/{}/*/{}/{}/include",
            home, arch, arch, arch
        );
        if let Some(path) = glob_first(&home_espressif_pattern) {
            return Some(path);
        }

        let home_embuild_pattern = format!(
            "{}/.embuild/espressif/tools/{}/*/{}/{}/include",
            home, arch, arch, arch
        );
        if let Some(path) = glob_first(&home_embuild_pattern) {
            return Some(path);
        }
    }

    None
}

fn find_include_dir_from_compiler(cc_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let parent = cc_path.parent()?; // bin/
    let toolchain_root = parent.parent()?; // xtensa-esp-elf/ or riscv32-esp-elf/

    // Check for standard layout: toolchain_root/target/include
    if let Ok(entries) = std::fs::read_dir(toolchain_root) {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_dir() {
                    let candidate = path.join("include");
                    if candidate.exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }

    // Check for flat layout: toolchain_root/include
    let candidate = toolchain_root.join("include");
    if candidate.exists() {
        return Some(candidate);
    }

    None
}

fn glob_first(pattern: &str) -> Option<std::path::PathBuf> {
    if let Ok(entries) = glob::glob(pattern) {
        if let Some(Ok(path)) = entries.into_iter().next() {
            return Some(path);
        }
    }
    None
}
