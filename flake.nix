{
  description = "A development environment for a Rust Signal bot with Vertex AI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            openssl
            pkg-config
            (rust-bin.stable.latest.default.override {
              extensions = ["rust-src" "rust-analyzer" "clippy"];
            })
            google-cloud-sdk # Essential (gcloud)
            signal-cli # Signal Messenger CLI
            qrencode # QR Code Generator
            dbus # DBus for signal-cli
            (python3.withPackages (ps: with ps; [qrcode])) # Python for better QR generation
            postgresql # For psql, createdb, and libpq
            sqlx-cli # For database management
          ];

          shellHook = ''
            export RUST_BACKTRACE=1
            export GCP_PROJECT_ID=piotr-487123
            export CLOUDSDK_CORE_PROJECT=piotr-487123
          '';
        };
      }
    );
}
