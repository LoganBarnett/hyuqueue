{
  description = "Human work queue";
  inputs = {
    # LLM: Do NOT change this URL unless explicitly directed. This is the
    # correct format for nixpkgs stable (25.11 is correct, not nixos-25.11).
    nixpkgs.url = "github:NixOS/nixpkgs/25.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    crane,
  } @ inputs: let
    forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
    overlays = [
      (import rust-overlay)
    ];
    pkgsFor = system:
      import nixpkgs {
        inherit system;
        overlays = overlays;
      };

    workspaceCrates = {
      # CLI — thin HTTP client for the server.
      cli = {
        name = "hyuqueue-cli";
        binary = "hyuqueue";
        description = "CLI client";
      };

      # Server — daemon that owns SQLite, LLM workers, and the HTTP API.
      server = {
        name = "hyuqueue-server";
        binary = "hyuqueue-server";
        description = "Server daemon";
      };

      # TUI — ratatui-based secondary client.
      tui = {
        name = "hyuqueue-tui";
        binary = "hyuqueue-tui";
        description = "TUI client";
      };
    };

    devPackages = pkgs: let
      rust = pkgs.rust-bin.stable.latest.default.override {
        extensions = [
          "rust-src"
          "rust-analyzer"
          "rustfmt"
        ];
      };
    in [
      rust
      pkgs.cargo-sweep
      pkgs.pkg-config
      pkgs.openssl
      pkgs.jq
      # Elm toolchain
      pkgs.elmPackages.elm
      pkgs.elmPackages.elm-format
      pkgs.elm2nix
      # Unified formatter
      pkgs.treefmt
      pkgs.alejandra
      pkgs.prettier
      pkgs.just
    ];
  in {
    devShells = forAllSystems (system: let
      pkgs = pkgsFor system;
    in {
      default = pkgs.mkShell {
        buildInputs = devPackages pkgs;
        shellHook = ''
          echo "hyuqueue development environment"
          echo ""
          echo "Available Cargo packages (use 'cargo build -p <name>'):"
          cargo metadata --no-deps --format-version 1 2>/dev/null | \
            jq -r '.packages[].name' | \
            sort | \
            sed 's/^/  • /' || echo "  Run 'cargo init' to get started"

          echo ""
          echo "Elm frontend (frontend/):"
          echo "  Build:   cd frontend && elm make src/Main.elm --output public/elm.js"
          echo "  Format:  treefmt"
          echo "  After changing elm.json dependency versions, regenerate Nix files:"
          echo "    cd frontend"
          echo "    elm2nix convert 2>/dev/null > elm-srcs.nix"
          echo "    elm2nix snapshot"
          echo "    git add elm-srcs.nix registry.dat && git commit"

          # Symlink cargo-husky hooks into .git/hooks/ using paths relative
          # to .git/hooks/ so the repo stays valid after moves or copies.
          _git_root=$(git rev-parse --show-toplevel 2>/dev/null)
          if [ -n "$_git_root" ] && [ "$(pwd)" = "$_git_root" ] && [ -d ".cargo-husky/hooks" ]; then
            for _hook in .cargo-husky/hooks/*; do
              [ -x "$_hook" ] || continue
              _name=$(basename "$_hook")
              _dest="$_git_root/.git/hooks/$_name"
              _target=$(${pkgs.coreutils}/bin/realpath --relative-to="$_git_root/.git/hooks" "$(pwd)/$_hook")
              if [ ! -L "$_dest" ] || [ "$(readlink "$_dest")" != "$_target" ]; then
                ln -sf "$_target" "$_dest"
                echo "Installed git hook: $_name -> $_target"
              fi
            done
          fi
        '';
      };
    });

    packages = forAllSystems (system: let
      pkgs = pkgsFor system;
      craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default);

      sqlFilter = path: _type: builtins.match ".*\\.sql$" path != null;
      src = pkgs.lib.cleanSourceWith {
        src = ./.;
        filter = path: type:
          (sqlFilter path type) || (craneLib.filterCargoSources path type);
      };

      commonArgs = {
        inherit src;
        # LLM: Do NOT add darwin.apple_sdk.frameworks here - they were removed
        # in nixpkgs 25.11+. Use libiconv for Darwin builds instead.
        buildInputs = with pkgs;
          [
            openssl
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin (with pkgs.darwin; [
            libiconv
          ]);
        nativeBuildInputs = with pkgs; [
          pkg-config
        ];
        # Run only unit tests (--lib --bins), skip integration tests in tests/
        # directories.  Integration tests may require external services not
        # available in Nix sandbox.
        cargoTestExtraArgs = "--lib --bins";
      };

      cratePackages =
        pkgs.lib.mapAttrs (
          key: crate: let
            pkgFile = ./. + "/nix/packages/${key}.nix";
          in
            if builtins.pathExists pkgFile
            then import pkgFile {inherit craneLib commonArgs pkgs;}
            else
              craneLib.buildPackage (commonArgs
                // {
                  pname = crate.name;
                  cargoExtraArgs = "-p ${crate.name}";
                })
        )
        workspaceCrates;
    in
      cratePackages
      // {
        default = craneLib.buildPackage (commonArgs // {pname = "hyuqueue";});
        emacs = import ./nix/packages/emacs.nix {inherit pkgs;};
      });

    apps = forAllSystems (system: let
      pkgs = pkgsFor system;
    in
      pkgs.lib.mapAttrs (key: crate: {
        type = "app";
        program = "${self.packages.${system}.${key}}/bin/${crate.binary}";
      })
      workspaceCrates);

    nixosModules = {
      server = import ./nix/modules/nixos-server.nix {inherit self;};
      default = self.nixosModules.server;
    };

    darwinModules = {
      server = import ./nix/modules/darwin-server.nix {inherit self;};
      default = self.darwinModules.server;
    };
  };
}
