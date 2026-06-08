{
  description = "Human work queue";
  inputs = {
    # LLM: Do NOT change this URL unless explicitly directed. This is the
    # correct format for nixpkgs stable (25.11 is correct, not nixos-25.11).
    nixpkgs.url = "github:NixOS/nixpkgs/25.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
    # The rust-template flake exposes shared Nix helpers (service
    # modules for NixOS/Darwin, package iteration, devShell snippets)
    # under `inputs.foundation.lib.*`.  Pin to a release tag so
    # template-side breakage doesn't surprise us on `nix flake
    # update` — bump the pin when we do an intentional sync and
    # append the new commit hash to `rust-template.json`.
    foundation.url = "github:LoganBarnett/rust-template/v0.7.0";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    crane,
    foundation,
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

    # Per-system package + app derivation.  Split out as `let`-bound
    # since both the `packages` and `apps` flake outputs need it.
    perSystem = system: let
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

      # `foundation.lib.mkRustPackages` iterates the crate map,
      # honoring per-crate overrides at `nix/packages/<key>.nix` so
      # the server's frontend-bundled build (`nix/packages/server.nix`)
      # continues to apply.
      rustOutputs = foundation.lib.mkRustPackages {
        inherit self pkgs craneLib commonArgs;
        crates = workspaceCrates;
      };
    in {
      packages =
        rustOutputs.packages
        // {
          default = craneLib.buildPackage (commonArgs // {pname = "hyuqueue";});
          emacs = import ./nix/packages/emacs.nix {inherit pkgs;};
        };
      apps = rustOutputs.apps;
    };
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

          ${foundation.lib.cargoHuskyHookSnippet pkgs}
        '';
      };
    });

    packages = forAllSystems (system: (perSystem system).packages);

    apps = forAllSystems (system: (perSystem system).apps);

    nixosModules = {
      server = import ./nix/modules/nixos-server.nix {inherit self foundation;};
      default = self.nixosModules.server;
    };

    darwinModules = {
      server = import ./nix/modules/darwin-server.nix {inherit self foundation;};
      default = self.darwinModules.server;
    };
  };
}
