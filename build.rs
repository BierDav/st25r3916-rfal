fn main() {
    println!("cargo:rerun-if-changed=rfal/");
    println!("cargo:rerun-if-changed=src/rfal_platform.h");

    if cfg!(feature = "compile-c") {
        compile_c();
    }

    generate_bindings();
}

fn compile_c() {
    let mut builder = cc::Build::new();

    // Add all required include directories back
    builder
        .include("rfal/inc")
        .include("rfal/src")
        .include("rfal/src/st25r3916") // REQUIRED: Path to rfal_features.h
        .include("src");               // Path to your rfal_platform.h

    // Target chip definition
    builder.define("ST25R3916", None);

    builder.file("src/platform_log.c");
    
    let rfal_files = glob::glob("rfal/src/**/*.c").expect("Failed to read glob pattern");
    
    for entry in rfal_files {
        match entry {
            Ok(path) => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();

                // Exclude specific evaluation board files
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

fn generate_bindings() {
    let mut builder = bindgen::Builder::default()
        .header("rfal/inc/rfal_nfc.h")
        // Mirror the include paths from the C compiler
        .clang_arg("-Irfal/inc")
        .clang_arg("-Irfal/src")
        .clang_arg("-Irfal/src/st25r3916")
        .clang_arg("-Isrc")
        .clang_arg("-DST25R3916")
        .clang_arg("-D__BINDGEN__")
        .clang_arg("-D__attribute__(x)=")
        .clang_arg("-D__extension__=")
        .use_core()
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // --- NEW: ESP-IDF SYSCALLS & HEADERS DISCOVERY ---
    // This looks for the Xtensa toolchain headers so bindgen can find <string.h> etc.
    if let Ok(esp_idf_path) = std::env::var("ESP_IDF_PATH") {
        // If building inside an ESP-IDF environment, add those includes
        builder = builder.clang_arg(format!("-I{}/components/newlib/platform_include", esp_idf_path));
    }

    // Explicitly set the target for Clang so it uses the right sizes for types (int, long, etc.)
    builder = builder.clang_arg("--target=xtensa-esp32s3-elf");

    // Try to find the toolchain's internal headers automatically
    // Based on your logs, your toolchain is in ~/.espressif/
    let home = std::env::var("HOME").unwrap_or_default();
    let toolchain_include = format!("{}/.espressif/tools/xtensa-esp-elf/esp-14.2.0_20251107/xtensa-esp-elf/xtensa-esp-elf/include", home);

    if std::path::Path::new(&toolchain_include).exists() {
        builder = builder.clang_arg(format!("-I{}", toolchain_include));
    } else {
        println!("cargo:warning=Could not find Xtensa toolchain headers at {}. Bindgen might fail.", toolchain_include);
    }
    // --------------------------------------------------

    let bindings = builder
        .generate()
        .expect("Unable to generate bindings");

    let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}