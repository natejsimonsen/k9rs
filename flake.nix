{
  description = "k9rs — a Rust port of k9s, the Kubernetes terminal UI";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
      cargo = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "k9rs";
          version = cargo.package.version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          # Tests run in CI; the e2e suite needs a writable HOME and /tmp.
          doCheck = false;

          meta = {
            description = cargo.package.description;
            homepage = "https://github.com/natejsimonsen/k9rs";
            license = nixpkgs.lib.licenses.asl20;
            platforms = systems;
            mainProgram = "k9rs";
          };
        };
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.default}/bin/k9rs";
        };
      });
    };
}
