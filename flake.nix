{
  description = "Fast, AST-based structural outline for source files";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    flake-parts.url = "github:hercules-ci/flake-parts";
  };
  outputs = {
    flake-parts,
    crane,
    ...
  } @ inputs:
    flake-parts.lib.mkFlake {inherit inputs;} {
      # Allows definition of system-specific attributes
      # without needing to declare the system explicitly!
      #
      # Quick rundown of the provided arguments:
      # - config is a reference to the full configuration, lazily evaluated
      # - self' is the outputs as provided here, without system. (self'.packages.default)
      # - inputs' is the input without needing to specify system (inputs'.foo.packages.bar)
      # - pkgs is an instance of nixpkgs for your specific system
      # - system is the system this configuration is for
      perSystem = {
        config,
        self',
        inputs',
        pkgs,
        system,
        lib,
        ...
      }: let
        craneLib = crane.mkLib pkgs;
        # Only keeps markdown files and regular cargo source files (e.g. .rs, Cargo.toml, Cargo.lock)
        markdownFilter = path: _type: lib.hasSuffix ".md" path;
        markdownOrCargo = path: type:
          type == "directory" || (markdownFilter path type) || (craneLib.filterCargoSources path type);
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = markdownOrCargo;
          name = "source";
        };

        # Read package metadata from Cargo.toml so we don't have to
        # duplicate it here.
        cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
        crateName = craneLib.crateNameFromCargoToml {cargoToml = ./Cargo.toml;};

        # Cargo defaults the binary name to the package name when the crate
        # exposes a single `src/main.rs` and no explicit `[[bin]]` targets
        # override it. Honour any `[[bin]]` entry if one is added later.
        mainProgram =
          if (cargoToml ? bin) && (builtins.length cargoToml.bin > 0)
          then (builtins.head cargoToml.bin).name
          else crateName.pname;

        commonArgs = {
          inherit src;
          inherit (crateName) pname version;
          strictDeps = true;

          buildInputs =
            [
              # Add additional build inputs here
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              # Additional darwin specific inputs can be set here
              pkgs.libiconv
            ];

          # Additional environment variables can be set directly
          # MY_CUSTOM_VAR = "some value";
        };

        # Build *just* the cargo dependencies, so we can reuse
        # all of that work (e.g. via cachix) when running in CI
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # Build the actual crate itself, reusing the dependency
        # artifacts from above.
        astBro = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            doCheck = false;
            meta = {
              inherit (cargoToml.package) description;
              homepage = cargoToml.package.repository;
              license = lib.getLicenseFromSpdxId cargoToml.package.license;
              inherit mainProgram;
            };
          }
        );
      in {
        checks = {
          # Build the crate as part of `nix flake check` for convenience
          inherit astBro;

          # Run clippy (and deny all warnings) on the crate source,
          # again, reusing the dependency artifacts from above.
          #
          # Note that this is done as a separate derivation so that
          # we can block the CI if there are issues here, but not
          # prevent downstream consumers from building our crate by itself.
          astBroClippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );

          astBroDoc = craneLib.cargoDoc (
            commonArgs
            // {
              inherit cargoArtifacts;
              # This can be commented out or tweaked as necessary, e.g. set to
              # `--deny rustdoc::broken-intra-doc-links` to only enforce that lint
              env.RUSTDOCFLAGS = "--deny warnings";
            }
          );

          # Run tests with cargo-nextest
          # Consider setting `doCheck = false` on `astBro` if you do not want
          # the tests to run twice
          astBroNextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
        };

        # This is equivalent to packages.<system>.default
        packages = {
          default = astBro;
        };

        apps.default = {
          type = "app";
          program = lib.getExe astBro;
          inherit (astBro) meta;
        };

        devShells.default = craneLib.devShell {
          # Inherit inputs from checks.
          checks = self'.checks;

          # Additional dev-shell environment variables can be set directly
          # MY_CUSTOM_DEVELOPMENT_VAR = "something else";

          # Extra inputs can be added here; cargo and rustc are provided by default.
          packages = [
            # pkgs.ripgrep
          ];
        };
      };

      flake = {
        # The usual flake attributes can be defined here, including
        # system-agnostic and/or arbitrary outputs.
      };

      # Declared systems that your flake supports. These will be enumerated in perSystem
      systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    };
}
