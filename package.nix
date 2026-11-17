{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "logid";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  meta = {
    description = "Control Logitech HID++ mice (MX Master 4) from Linux";
    mainProgram = "logid";
    platforms = lib.platforms.linux;
  };
}
