//! Build script: compiles ST's RFAL C library and generates Rust bindings.
//!
//! Environment variables understood (besides the usual `CC_<target>`,
//! `CFLAGS_<target>`, `AR_<target>` handled by the `cc` crate and
//! `BINDGEN_EXTRA_CLANG_ARGS[_<target>]` / `LIBCLANG_PATH` handled by bindgen):
//!
//! * `RFAL_SKIP_C_BUILD` - when set to anything but `0`/empty, skip compiling
//!   the C sources (bindings are still generated). The resulting crate will
//!   not link into a final binary; this exists for doc builds and IDE/check
//!   setups without a cross C compiler.
//! * `DOCS_RS` - set by docs.rs; implies `RFAL_SKIP_C_BUILD`.
//! * `ESP_IDF_PATH` - extra newlib headers for ESP-IDF bindgen runs.
//! * `DEP_ESP_IDF_EMBUILD_ENV_PATH` - set by cargo from esp-idf-sys (feature
//!   `esp-idf`): the `PATH` with the ESP-IDF GCC, used when no `CC_<target>`
//!   is given.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=rfal");
    println!("cargo:rerun-if-changed=src/c");
    for var in [
        "RFAL_SKIP_C_BUILD",
        "DOCS_RS",
        "ESP_IDF_PATH",
        "LIBCLANG_PATH",
        "DEP_ESP_IDF_EMBUILD_ENV_PATH",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let target = Target::from_env();
    for var in [
        "BINDGEN_EXTRA_CLANG_ARGS".to_string(),
        format!("BINDGEN_EXTRA_CLANG_ARGS_{}", target.triple),
        format!(
            "BINDGEN_EXTRA_CLANG_ARGS_{}",
            target.triple.replace('-', "_")
        ),
        format!("CC_{}", target.triple),
        format!("CC_{}", target.triple.replace('-', "_")),
        "TARGET_CC".to_string(),
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let defines = c_defines();
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

    // Bindings first: the C build compiles a layout check derived from them.
    let abi_check = generate_bindings(&target, &defines, &out_dir);

    if skip_c_build() {
        if env::var_os("DOCS_RS").is_none() {
            println!(
                "cargo:warning=st25r3916-rfal: RFAL_SKIP_C_BUILD is set, skipping the \
                 RFAL C build; the crate will not link into a binary"
            );
        }
    } else {
        compile_c(&target, &defines, &abi_check);
    }
}

fn skip_c_build() -> bool {
    if env::var_os("DOCS_RS").is_some() {
        return true;
    }
    matches!(env::var("RFAL_SKIP_C_BUILD"), Ok(v) if !v.is_empty() && v != "0")
}

fn feature(name: &str) -> bool {
    let var = format!("CARGO_FEATURE_{}", name.to_uppercase().replace('-', "_"));
    env::var_os(var).is_some()
}

// ---------------------------------------------------------------------------
// Target classification
// ---------------------------------------------------------------------------

struct Target {
    triple: String,
    os: String,
    arch: String,
    host: String,
}

impl Target {
    fn from_env() -> Self {
        Self {
            triple: env::var("TARGET").expect("TARGET not set"),
            os: env::var("CARGO_CFG_TARGET_OS").unwrap_or_default(),
            arch: env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default(),
            host: env::var("HOST").unwrap_or_default(),
        }
    }

    /// No operating system and (usually) no C library: `thumbv*-none-*`,
    /// `riscv32*-unknown-none-elf`, `xtensa-*-none-elf`, ...
    fn is_bare_metal(&self) -> bool {
        self.os == "none"
    }

    /// Espressif targets need Espressif's toolchain (and, for Xtensa, its
    /// LLVM fork), so they get special bindgen treatment.
    fn is_esp(&self) -> bool {
        self.os == "espidf" || self.arch == "xtensa"
    }

    fn is_cross(&self) -> bool {
        self.triple != self.host
    }

    /// Bare-metal ARM (EABI): C enums take the smallest integer type that fits
    /// their values. That is GCC's `arm-none-eabi` default and the
    /// `repr(C)` enum layout rustc uses for these targets, but not clang's
    /// default, so bindgen (and a clang `CC`) must be told explicitly.
    /// Otherwise e.g. `rfalNfcDiscoverParam` is 180 bytes in C and 228 in Rust.
    fn has_short_enums(&self) -> bool {
        self.arch == "arm" && self.os == "none"
    }

    /// Thumb-1-only cores (Cortex-M0/M0+/M23, ARMv4T/v5TE in Thumb mode). rustc
    /// does not expose `thumb2` as a stable target feature, hence the triple.
    fn is_thumb1(&self) -> bool {
        ["thumbv4t", "thumbv5te", "thumbv6m", "thumbv8m.base"]
            .iter()
            .any(|prefix| self.triple.starts_with(prefix))
    }
}

// ---------------------------------------------------------------------------
// Feature -> C define mapping
// ---------------------------------------------------------------------------

/// Chip variant define. `st25r3916` and `st25r3916b` are mutually exclusive;
/// if both are enabled (e.g. through feature unification) `ST25R3916B` wins
/// and a warning is emitted, so `cargo check --all-features` keeps working.
fn chip_variant() -> &'static str {
    match (feature("st25r3916"), feature("st25r3916b")) {
        (true, false) => "ST25R3916",
        (false, true) => "ST25R3916B",
        (false, false) => panic!(
            "st25r3916-rfal: exactly one chip feature must be enabled: \
             `st25r3916` or `st25r3916b` (the default)"
        ),
        (true, true) => {
            println!(
                "cargo:warning=st25r3916-rfal: both `st25r3916` and `st25r3916b` are \
                 enabled; building for ST25R3916B"
            );
            "ST25R3916B"
        }
    }
}

fn c_defines() -> Vec<(&'static str, &'static str)> {
    let mut defines = vec![(chip_variant(), "1")];
    for (feat, define) in [
        ("apple-ecp", "RFAL_FEATURE_APPLE_ECP"),
        ("force-wupa", "RFAL_FEATURE_NFCA_FORCE_WUPA"),
        ("poll-keep-field-on", "RFAL_FEATURE_POLL_KEEP_FIELD_ON"),
    ] {
        if feature(feat) {
            defines.push((define, "1"));
        }
    }
    // ST25R_COM_SINGLETXRX is always defined by src/c/rfal_platform.h.
    defines
}

const INCLUDE_DIRS: [&str; 4] = ["rfal/inc", "rfal/src", "rfal/src/st25r3916", "src/c"];
const FREESTANDING_INCLUDE_DIR: &str = "src/c/freestanding";

// ---------------------------------------------------------------------------
// C compilation
// ---------------------------------------------------------------------------

fn compile_c(target: &Target, defines: &[(&str, &str)], abi_check: &Path) {
    let mut build = cc::Build::new();
    build.includes(INCLUDE_DIRS).warnings(false);

    if target.has_short_enums() {
        // GCC's default here already; makes a clang `CC` agree with bindgen.
        build.flag("-fshort-enums");
    }
    if target.is_thumb1() {
        // GCC's Thumb-1 switch tables (at -Os) call libgcc's
        // `__gnu_thumb1_case_*`, which Rust's compiler-builtins lacks: the
        // final binary would not link.
        build.flag("-fno-jump-tables");
    }
    if target.is_bare_metal() || target.is_esp() {
        // Firmware images are linked statically at fixed addresses.
        build.pic(false);
    }
    if target.is_bare_metal() {
        // Don't require a C library: our tiny <string.h> shim plus the
        // compiler's own freestanding headers are all RFAL needs.
        build
            .flag("-ffreestanding")
            .include(FREESTANDING_INCLUDE_DIR);
    }
    if target.arch == "xtensa" {
        build.flag("-mlongcalls");
    }
    if target.is_esp() && target_cc_var(target).is_none() {
        // esp-idf-sys installs the GCC toolchain but does not put it on PATH.
        if let Some(gcc) = esp_idf_sys_gcc(target) {
            let ar = gcc.with_file_name(format!(
                "{}-ar{}",
                esp_gcc_prefix(target),
                env::consts::EXE_SUFFIX
            ));
            build.compiler(&gcc);
            if ar.is_file() {
                build.archiver(ar);
            }
        }
    }
    for (name, value) in defines {
        build.define(name, Some(*value));
    }

    for entry in glob::glob("rfal/src/**/*.c").expect("valid glob pattern") {
        let path = entry.expect("readable rfal/src entry");
        // Antenna auto-tuning is not exposed and needs <stdlib.h> (abs()).
        if path.file_name().is_some_and(|f| f == "st25r3916_aat.c") {
            continue;
        }
        build.file(path);
    }
    // Fails the C build if this compiler lays out an RFAL type differently
    // from the bindings.
    build.file(abi_check);

    build.compile("rfal");
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// Writes `bindings.rs` and `rfal_types_gen.rs` to `out_dir`, plus the C
/// layout check whose path is returned.
fn generate_bindings(target: &Target, defines: &[(&str, &str)], out_dir: &Path) -> PathBuf {
    let mut builder = bindgen::Builder::default()
        .header("rfal/inc/rfal_nfc.h")
        .header("rfal/inc/rfal_rf.h")
        .clang_args(INCLUDE_DIRS.iter().map(|d| format!("-I{d}")))
        .clang_args(defines.iter().map(|(n, v)| format!("-D{n}={v}")))
        .clang_arg("-D__BINDGEN__")
        // Edition 2024 code (`unsafe extern` blocks, unsafe ops wrapped in
        // `unsafe {}` inside unsafe fns) for the MSRV rather than for whichever
        // rustc runs this, so the output does not depend on the compiler.
        .rust_target(
            env!("CARGO_PKG_RUST_VERSION")
                .parse()
                .expect("rust-version is a valid bindgen RustTarget"),
        )
        .rust_edition(bindgen::RustEdition::Edition2024)
        .wrap_unsafe_ops(true)
        .use_core()
        // Deterministic output independent of whether rustfmt is installed:
        // the type-wrapper generator below parses the bindings line by line.
        .formatter(bindgen::Formatter::Prettyplease)
        // Compile-time asserts that rustc lays out each type as libclang did;
        // `abi_check_c` turns the same numbers into C asserts for the C compiler.
        .layout_tests(true)
        .generate_comments(false)
        .allowlist_function("rfal.*")
        .allowlist_type("rfal.*|ReturnCode")
        .allowlist_var("RFAL_.*|rfal.*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    if target.is_bare_metal() {
        builder = builder.clang_arg(format!("-I{FREESTANDING_INCLUDE_DIR}"));
    }
    if target.has_short_enums() {
        builder = builder.clang_arg("-fshort-enums");
    }

    if target.is_esp() {
        builder = builder.clang_arg(format!("--target={}", esp_clang_target(target)));
        for dir in esp_include_dirs(target) {
            builder = builder.clang_arg(format!("-isystem{}", dir.display()));
        }
    } else if target.is_cross()
        && !target.is_bare_metal()
        && !has_user_bindgen_args(target)
        // Hosted cross target (e.g. a Linux SBC): clang needs the target's libc
        // headers. Best effort: ask the C compiler `cc` would use for its sysroot.
        && let Some(sysroot) = compiler_sysroot()
    {
        builder = builder.clang_arg(format!("--sysroot={}", sysroot.display()));
    }

    let bindings = builder
        .generate()
        .expect("st25r3916-rfal: bindgen failed (is libclang installed? see README)");

    let mut content = bindings.to_string();

    // RFAL's error codes are plain `#define RFAL_ERR_X ((ReturnCode)N)` macros,
    // which bindgen cannot evaluate because of the cast; extract them by hand.
    content.push_str("\n// --- RFAL_ERR_* constants extracted from rfal_utils.h ---\n");
    let header =
        std::fs::read_to_string("rfal/inc/rfal_utils.h").expect("could not read rfal_utils.h");
    for (name, value) in extract_error_defines(&header) {
        content.push_str(&format!("pub const {name}: u16 = {value};\n"));
    }

    std::fs::write(out_dir.join("bindings.rs"), &content).expect("could not write bindings.rs");
    std::fs::write(
        out_dir.join("rfal_types_gen.rs"),
        generate_type_wrappers(&content),
    )
    .expect("could not write rfal_types_gen.rs");

    let abi_check = out_dir.join("abi_check.c");
    std::fs::write(&abi_check, abi_check_c(&content)).expect("could not write abi_check.c");
    abi_check
}

fn has_user_bindgen_args(target: &Target) -> bool {
    [
        "BINDGEN_EXTRA_CLANG_ARGS".to_string(),
        format!("BINDGEN_EXTRA_CLANG_ARGS_{}", target.triple),
        format!(
            "BINDGEN_EXTRA_CLANG_ARGS_{}",
            target.triple.replace('-', "_")
        ),
    ]
    .iter()
    .any(|v| env::var_os(v).is_some())
}

/// `<gcc> -print-sysroot` of the compiler `cc` resolves for this target.
fn compiler_sysroot() -> Option<PathBuf> {
    let compiler = cc::Build::new()
        .cargo_metadata(false)
        .try_get_compiler()
        .ok()?;
    if compiler.is_like_clang() {
        return None;
    }
    let out = Command::new(compiler.path())
        .arg("-print-sysroot")
        .output()
        .ok()?;
    let sysroot = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    (out.status.success() && !sysroot.as_os_str().is_empty() && sysroot.is_dir()).then_some(sysroot)
}

/// Clang target triple for Espressif chips. Xtensa needs Espressif's LLVM
/// fork, which names targets `xtensa-<mcu>-elf`; the RISC-V ESP chips are
/// plain upstream `riscv32` with an ESP vendor.
fn esp_clang_target(target: &Target) -> String {
    if target.arch == "xtensa" {
        // Rust triples: xtensa-esp32-espidf, xtensa-esp32s3-none-elf, ...
        let mcu = target.triple.split('-').nth(1).unwrap_or("esp32");
        format!("xtensa-{mcu}-elf")
    } else {
        format!("{}-esp-elf", target.arch)
    }
}

/// libc (newlib) headers for bindgen on ESP targets.
fn esp_include_dirs(target: &Target) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(idf) = env::var("ESP_IDF_PATH") {
        let dir = Path::new(&idf).join("components/newlib/platform_include");
        if dir.is_dir() {
            dirs.push(dir);
        }
    }
    let toolchain = esp_toolchain(target);

    // 1. The compiler explicitly configured for this target, else the one
    //    esp-idf-sys installed.
    let cc = target_cc_var(target)
        .map(PathBuf::from)
        .or_else(|| esp_idf_sys_gcc(target));
    if let Some(dir) = cc.and_then(|cc| gcc_libc_include_dir(&cc, toolchain)) {
        dirs.push(dir);
        return dirs;
    }

    // 2. Toolchains installed by embuild/espup next to the project or in $HOME.
    let mut roots = Vec::new();
    if let Ok(manifest) = env::var("CARGO_MANIFEST_DIR") {
        roots.extend(Path::new(&manifest).ancestors().map(Path::to_path_buf));
    }
    if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home));
    }
    for root in roots {
        for base in [".embuild/espressif/tools", ".espressif/tools"] {
            let pattern = root.join(format!(
                "{base}/{toolchain}/*/{toolchain}/{toolchain}/include"
            ));
            if let Some(dir) = glob_first(&pattern.to_string_lossy()) {
                dirs.push(dir);
                return dirs;
            }
        }
    }

    println!(
        "cargo:warning=st25r3916-rfal: could not find the {toolchain} libc headers; \
         set CC_{} or BINDGEN_EXTRA_CLANG_ARGS_{} if bindgen fails",
        target.triple, target.triple
    );
    dirs
}

/// Name of the Espressif GCC toolchain (and of its libc header directory).
fn esp_toolchain(target: &Target) -> &'static str {
    if target.arch == "xtensa" {
        "xtensa-esp-elf"
    } else {
        "riscv32-esp-elf"
    }
}

/// Name prefix of the GCC binaries. On Xtensa only the per-chip wrappers
/// (`xtensa-esp32-elf-gcc`, ...) select the chip's core configuration; the
/// plain `xtensa-esp-elf-gcc` (cc's default) builds big-endian objects.
fn esp_gcc_prefix(target: &Target) -> String {
    if target.arch == "xtensa" {
        let mcu = target.triple.split('-').nth(1).unwrap_or("esp32");
        format!("xtensa-{mcu}-elf")
    } else {
        esp_toolchain(target).to_string()
    }
}

/// C compiler configured for this target through the environment.
fn target_cc_var(target: &Target) -> Option<String> {
    [
        format!("CC_{}", target.triple),
        format!("CC_{}", target.triple.replace('-', "_")),
        "TARGET_CC".to_string(),
    ]
    .into_iter()
    .find_map(|v| env::var(v).ok())
}

/// The GCC of the ESP-IDF toolchain esp-idf-sys installed: cargo passes its
/// `PATH` (embuild's `EMBUILD_ENV_PATH` metadata) to direct dependents, i.e.
/// to this crate with the `esp-idf` feature.
fn esp_idf_sys_gcc(target: &Target) -> Option<PathBuf> {
    let path = env::var_os("DEP_ESP_IDF_EMBUILD_ENV_PATH")?;
    let gcc = format!("{}-gcc{}", esp_gcc_prefix(target), env::consts::EXE_SUFFIX);
    env::split_paths(&path)
        .map(|dir| dir.join(&gcc))
        .find(|p| p.is_file())
}

/// `<toolchain_root>/<toolchain>/include` for a GCC at `<toolchain_root>/bin/<gcc>`,
/// falling back to `-print-sysroot`.
fn gcc_libc_include_dir(cc: &Path, toolchain: &str) -> Option<PathBuf> {
    let root = cc.parent()?.parent()?;
    let candidate = root.join(toolchain).join("include");
    if candidate.join("string.h").is_file() {
        return Some(candidate);
    }
    let out = Command::new(cc).arg("-print-sysroot").output().ok()?;
    let sysroot = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    let candidate = sysroot.join("include");
    candidate.join("string.h").is_file().then_some(candidate)
}

fn glob_first(pattern: &str) -> Option<PathBuf> {
    glob::glob(pattern).ok()?.flatten().next()
}

/// Parses `#define RFAL_ERR_NAME ((ReturnCode)N)` lines.
fn extract_error_defines(header: &str) -> Vec<(String, u16)> {
    header
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            if parts.next()? != "#define" {
                return None;
            }
            let name = parts.next()?;
            if !name.starts_with("RFAL_ERR_") {
                return None;
            }
            let digits: String = parts.next()?.chars().filter(char::is_ascii_digit).collect();
            Some((name.to_string(), digits.parse().ok()?))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// C layout check: bindgen's layout tests assert that rustc lays out every type
// as libclang did. The same numbers as C `_Static_assert`s, compiled with the
// RFAL sources, assert that the C compiler agrees too, so a C ABI flag that
// differs between the C build and bindgen (enum size, packing, ...) fails the
// build instead of corrupting memory.
// ---------------------------------------------------------------------------

fn abi_check_c(bindings: &str) -> String {
    let mut checks = String::new();
    for (kind, item, expected) in layout_tests(bindings) {
        // Types and fields bindgen invented have no C name.
        if item.contains("__bindgen") || item.contains("::_") {
            continue;
        }
        let expr = match kind {
            "Size" => format!("sizeof({item})"),
            "Alignment" => format!("_Alignof({item})"),
            _ => match item.split_once("::") {
                Some((ty, field)) => format!("offsetof({ty}, {})", c_field_name(field)),
                None => continue,
            },
        };
        checks.push_str(&format!(
            "_Static_assert({expr} == {expected}u, \"st25r3916-rfal: {expr} differs \
             between the C compiler and the Rust bindings (see build.rs)\");\n"
        ));
    }
    format!(
        "/* AUTO-GENERATED by build.rs from the layout tests in bindings.rs - do not edit. */\n\
         #include <stddef.h>\n\
         #include \"rfal_nfc.h\"\n\
         #include \"rfal_rf.h\"\n\
         \n\
         #if (defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L) || defined(__GNUC__)\n\
         {checks}\
         #endif\n"
    )
}

/// `(kind, item, value)` of every bindgen layout test, where kind is `Size`,
/// `Alignment` or `Offset` and item is `Type` or `Type::field`. Parsed from
/// `["Size of T"][::core::mem::size_of::<T>() - 24usize];`, which prettyplease
/// may wrap over several lines.
fn layout_tests(bindings: &str) -> Vec<(&'static str, &str, u64)> {
    let mut tests = Vec::new();
    for (kind, prefix) in [
        ("Size", "\"Size of "),
        ("Alignment", "\"Alignment of "),
        ("Offset", "\"Offset of field: "),
    ] {
        for (start, _) in bindings.match_indices(prefix) {
            let rest = &bindings[start + prefix.len()..];
            let parsed = rest
                .find('"')
                .zip(rest.find("usize]"))
                .and_then(|(end, value_end)| {
                    // `...() - 24usize]`: the number before the first `usize]`.
                    let (_, value) = rest[..value_end].rsplit_once("- ")?;
                    Some((&rest[..end], value.trim().parse().ok()?))
                });
            let (item, value) = parsed.unwrap_or_else(|| {
                panic!("st25r3916-rfal: unparsable bindgen layout test at {prefix}{rest:.60}")
            });
            tests.push((kind, item, value));
        }
    }
    tests
}

/// C name of a bindgen field: bindgen appends `_` to Rust keywords (`type_`).
fn c_field_name(field: &str) -> &str {
    const RUST_ONLY_KEYWORDS: [&str; 26] = [
        "as", "async", "await", "box", "crate", "dyn", "fn", "impl", "in", "let", "loop", "match",
        "mod", "move", "mut", "pub", "ref", "self", "super", "trait", "type", "unsafe", "use",
        "where", "yield", "gen",
    ];
    match field.strip_suffix('_') {
        Some(stem) if RUST_ONLY_KEYWORDS.contains(&stem) => stem,
        _ => field,
    }
}

// ---------------------------------------------------------------------------
// Type-wrapper generator: turns `pub type rfalX = c_uint;` + its
// `pub const rfalX_Y: rfalX = N;` constants into a Rust enum `RfalX` with
// `RfalX::from_repr`, `TryFrom<rfalX>` and `From<RfalX> for rfalX`. The
// `RFAL_ERR_*` codes stay plain `u16` constants; the error type is the
// hand-written `crate::RfalError`.
// ---------------------------------------------------------------------------

fn generate_type_wrappers(content: &str) -> String {
    use std::collections::BTreeMap;

    // Step 1: the integer typedefs of C enums, `pub type rfal<Name> = ::core::ffi::c_*;`.
    // (`uint8_t` & co. come out as `u8`, so these are enums.) With short enums
    // (bare-metal ARM) the type is the smallest that fits, so it differs by target.
    let mut type_repr: BTreeMap<String, &'static str> = BTreeMap::new();
    for line in content.lines() {
        let Some(rest) = line.trim().strip_prefix("pub type rfal") else {
            continue;
        };
        let Some((name, rhs)) = rest.split_once(" = ") else {
            continue;
        };
        let repr = match rhs.trim_end_matches(';').trim() {
            "::core::ffi::c_uchar" => "u8",
            "::core::ffi::c_schar" => "i8",
            "::core::ffi::c_ushort" => "u16",
            "::core::ffi::c_short" => "i16",
            "::core::ffi::c_uint" => "u32",
            "::core::ffi::c_int" => "i32",
            _ => continue, // function pointers, struct aliases, arrays, ...
        };
        type_repr.insert(format!("rfal{}", name.trim()), repr);
    }

    // Step 2: `pub const {Type}_{SUFFIX}: {Type} = N;`
    let mut constants: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    for line in content.lines() {
        let Some(rest) = line.trim().strip_prefix("pub const ") else {
            continue;
        };
        let Some((name, rhs)) = rest.split_once(": ") else {
            continue;
        };
        let Some((ty, value)) = rhs.split_once(" = ") else {
            continue;
        };
        if !type_repr.contains_key(ty) {
            continue;
        }
        let Some(suffix) = name.strip_prefix(ty).and_then(|s| s.strip_prefix('_')) else {
            continue;
        };
        if let Ok(value) = value.trim_end_matches(';').trim().parse::<i64>() {
            constants
                .entry(ty.to_string())
                .or_default()
                .push((suffix.to_string(), value));
        }
    }

    let mut out =
        String::from("// AUTO-GENERATED by build.rs from the RFAL bindings - do not edit.\n\n");

    for (c_type, mut variants) in constants {
        let repr = type_repr[&c_type];
        let rust_name = capitalize_first(&c_type);
        variants.sort_by_key(|(_, v)| *v);
        variants.dedup_by_key(|(_, v)| *v); // aliases share a discriminant

        let suffixes: Vec<&str> = variants.iter().map(|(n, _)| n.as_str()).collect();
        let common = common_underscore_prefix(&suffixes);
        let fallback = common
            .trim_end_matches('_')
            .rsplit('_')
            .next()
            .map(capitalize_word)
            .unwrap_or_default();

        let variants: Vec<(String, &str, i64)> = variants
            .iter()
            .map(|(c_name, value)| {
                let rem = c_name.strip_prefix(&common).unwrap_or(c_name);
                (make_variant_name(rem, &fallback), c_name.as_str(), *value)
            })
            .collect();

        out.push_str(&format!(
            "/// Rust mirror of the C enum `{c_type}`.\n\
             #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\n\
             #[cfg_attr(feature = \"defmt\", derive(defmt::Format))]\n\
             #[non_exhaustive]\n\
             #[repr({repr})]\n\
             pub enum {rust_name} {{\n"
        ));
        for (variant, c_name, value) in &variants {
            out.push_str(&format!("    /// `{c_name}`\n    {variant} = {value},\n"));
        }
        out.push_str(&format!(
            "}}\n\n\
             impl {rust_name} {{\n\
             \x20   /// The variant with this C value, or `None` if RFAL defines no `{c_type}` constant for it.\n\
             \x20   #[inline]\n\
             \x20   pub const fn from_repr(v: {c_type}) -> Option<Self> {{\n\
             \x20       match v {{\n"
        ));
        for (variant, _, value) in &variants {
            out.push_str(&format!("            {value} => Some(Self::{variant}),\n"));
        }
        out.push_str(&format!(
            "            _ => None,\n\
             \x20       }}\n\
             \x20   }}\n\
             }}\n\n\
             impl TryFrom<{c_type}> for {rust_name} {{\n\
             \x20   /// The value is not one of RFAL's `{c_type}` constants.\n\
             \x20   type Error = ();\n\
             \x20   #[inline]\n\
             \x20   fn try_from(v: {c_type}) -> Result<Self, ()> {{\n\
             \x20       Self::from_repr(v).ok_or(())\n\
             \x20   }}\n\
             }}\n\n\
             impl From<{rust_name}> for {c_type} {{\n\
             \x20   #[inline]\n\
             \x20   fn from(v: {rust_name}) -> {c_type} {{\n\
             \x20       v as {c_type}\n\
             \x20   }}\n\
             }}\n\n"
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Naming helpers
// ---------------------------------------------------------------------------

/// `rfalMode` -> `RfalMode`
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(ch) => ch.to_ascii_uppercase().to_string() + c.as_str(),
    }
}

/// `HELLO` -> `Hello`
fn capitalize_word(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        None => String::new(),
        Some(ch) => ch.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
    }
}

/// `SCREAMING_SNAKE` -> `ScreamingSnake`; digit-led words are kept verbatim.
fn screaming_snake_to_camel(s: &str) -> String {
    s.split('_')
        .filter(|w| !w.is_empty())
        .map(capitalize_word)
        .collect()
}

/// Longest common prefix of all strings, cut back to the last `_`.
fn common_underscore_prefix(strs: &[&str]) -> String {
    let Some(first) = strs.first() else {
        return String::new();
    };
    let common_len = if strs.len() == 1 {
        first.len()
    } else {
        strs[1..].iter().fold(first.len(), |acc, s| {
            acc.min(
                first
                    .bytes()
                    .zip(s.bytes())
                    .take_while(|(a, b)| a == b)
                    .count(),
            )
        })
    };
    match first[..common_len].rfind('_') {
        Some(pos) => first[..=pos].to_string(),
        None => String::new(),
    }
}

/// CamelCase variant name; digit-led names get `fallback_prefix` prepended.
fn make_variant_name(remaining: &str, fallback_prefix: &str) -> String {
    if remaining.starts_with(|c: char| c.is_ascii_digit()) {
        format!("{fallback_prefix}{}", screaming_snake_to_camel(remaining))
    } else {
        screaming_snake_to_camel(remaining)
    }
}
