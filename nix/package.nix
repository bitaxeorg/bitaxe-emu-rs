{
  lib,
  makeRustPlatform,
  rust-bin,
  fetchurl,
  copyDesktopItems,
  makeDesktopItem,
  pkg-config,
  removeReferencesTo,
  zstd,
  dejavu_fonts,
  wayland,
  libxkbcommon,
  fontconfig,
  vulkan-loader,
  # rax source tree, for the Xtensa LX7 core.
  rax,
  # esp32s3-rax source tree, the ESP32-S3 on rax.
  esp32s3-rax,
}:

let
  # The toolchain of rust-toolchain.toml, without the development components.
  rust = rust-bin.fromRustupToolchain {
    inherit ((lib.importTOML ../rust-toolchain.toml).toolchain) channel;
    profile = "minimal";
  };
  rustPlatform = makeRustPlatform {
    cargo = rust;
    rustc = rust;
  };
  # ESP32-S3 internal ROM image, the default of `--rom`.
  rom = fetchurl {
    url = "https://raw.githubusercontent.com/espressif/qemu/esp-develop-9.2.2-20261109/pc-bios/esp32s3_rev0_rom.bin";
    hash = "sha256-tccJDMIu/OUcEyPO8xypb81nOlsdGxAX4hnTk/JlmRM=";
  };
  # Libraries the BLIT desktop backend opens at run time.
  runtimeLibs = [
    wayland
    libxkbcommon
    fontconfig
    vulkan-loader
  ];
  fonts = "${dejavu_fonts}/share/fonts/truetype";
in
rustPlatform.buildRustPackage {
  pname = "bitaxe-emu";
  inherit ((lib.importTOML ../Cargo.toml).package) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      (lib.fileset.fileFilter (file: file.hasExt "png") ../assets)
      ../assets/esp32s3-bootloader.bin
    ];
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = {
      "blit-0.1.0" = "sha256-YSoaLiCByY9OxwOUZxRTAi1wed6yieWCXJ04h00BxAg=";
    };
  };

  # esp32s3-rax is a path dependency next to the checkout, and rax one next to
  # esp32s3-rax: a copy of esp32s3-rax in the source, pointing to the rax tree.
  postPatch = ''
    cp -r ${esp32s3-rax} esp32s3-rax
    chmod -R u+w esp32s3-rax
    substituteInPlace esp32s3-rax/Cargo.toml \
      --replace-fail '"../upstream/rax"' '"${rax}"'
    substituteInPlace Cargo.toml \
      --replace-fail '"../esp32s3-rax"' '"./esp32s3-rax"'
  '';

  nativeBuildInputs = [
    copyDesktopItems
    pkg-config
    removeReferencesTo
  ];
  buildInputs = [ zstd ];

  # Both binaries: bitaxe-emu and esp32s3-run (of esp32s3-rax).
  cargoBuildFlags = [
    "-p"
    "bitaxe-emu-rs"
    "-p"
    "esp32s3-rax"
  ];
  cargoTestFlags = [
    "-p"
    "bitaxe-emu-rs"
    "-p"
    "esp32s3-rax"
  ];

  env = {
    BITAXE_EMU_FONT = "${fonts}/DejaVuSans.ttf";
    BITAXE_EMU_FONT_MONO = "${fonts}/DejaVuSansMono.ttf";
    ESP32S3_ROM = "${rom}";
  };

  # The launcher entry; its window has the title "Bitaxe emulator".
  desktopItems = [
    (makeDesktopItem {
      name = "bitaxe-emu";
      desktopName = "Bitaxe Emulator";
      genericName = "Bitaxe board emulator";
      comment = "Run Bitaxe miner firmware on an emulated board";
      exec = "bitaxe-emu";
      icon = "bitaxe-emu";
      categories = [
        "Development"
        "Emulator"
      ];
      keywords = [
        "bitaxe"
        "bitcoin"
        "esp32"
        "miner"
      ];
    })
  ];

  postInstall = ''
    install -Dm644 ${../assets/bitaxe-emu.svg} $out/share/icons/hicolor/scalable/apps/bitaxe-emu.svg
  '';

  # The rax source paths left in panic messages are not a run-time dependency.
  # The RPATH is extended after the fixup, which drops the entries no DT_NEEDED
  # uses.
  postFixup = ''
    remove-references-to -t ${rax} -t ${esp32s3-rax} $out/bin/*
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/bitaxe-emu
  '';

  meta = {
    description = "Bitaxe emulator: the ESP32-S3 runs the miner firmware with the board around it";
    homepage = "https://github.com/bitaxeorg/bitaxe-emu-rs";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "bitaxe-emu";
    platforms = lib.platforms.linux;
  };
}
