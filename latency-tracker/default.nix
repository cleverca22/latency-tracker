{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "latency-tracker";
  version = "0.0.1";
  src = ./.;
  cargoLock.lockFile = ./Cargo.lock;
}
