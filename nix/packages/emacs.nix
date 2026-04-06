# Emacs package for hyuqueue built with trivialBuild.
# Called from flake.nix with: import ./emacs.nix { inherit pkgs; }
{pkgs}: let
  inherit (pkgs) lib;
in
  pkgs.emacsPackages.trivialBuild {
    pname = "hyuqueue";
    ename = "hyuqueue";
    version = "0.1.0";
    src = ../../emacs;
    packageRequires = [pkgs.emacsPackages.transient];
    meta = {
      homepage = "https://gitea.proton/logan/hyuqueue";
      license = lib.licenses.gpl3;
    };
  }
