{
  description = "k9rs — a Rust port of k9s, the Kubernetes terminal UI";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs { inherit system; }));
      cargo = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      # `k9rs version` info from a clean checkout (build.rs reads these);
      # dirty trees and tarballs show "dev" / "n/a".
      d = self.lastModifiedDate or "";
      buildInfo = nixpkgs.lib.optionalAttrs (self ? rev && d != "") {
        K9RS_COMMIT = self.rev;
        K9RS_DATE = with builtins;
          "${substring 0 4 d}-${substring 4 2 d}-${substring 6 2 d}T${substring 8 2 d}:${substring 10 2 d}:${substring 12 2 d}Z";
      };
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage (buildInfo // {
          pname = "k9rs";
          version = cargo.package.version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          # Tests run in CI; the e2e suite needs a writable HOME and /tmp.
          doCheck = false;

          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = pkgs.lib.optionalString
            (pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform) ''
              installShellCompletion --cmd k9rs \
                --bash <($out/bin/k9rs completion bash) \
                --zsh <($out/bin/k9rs completion zsh) \
                --fish <($out/bin/k9rs completion fish)
            '';

          meta = {
            description = cargo.package.description;
            homepage = "https://github.com/natejsimonsen/k9rs";
            license = nixpkgs.lib.licenses.asl20;
            platforms = systems;
            mainProgram = "k9rs";
          };
        });
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.default}/bin/k9rs";
        };
      });
    };
}
