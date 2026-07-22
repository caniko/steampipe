# Host-level Steam game user module.
#
# Creates a NixOS user with Steam, GameMode, and Wine/Proton for
# running game executables (dummy Steam accounts, playtesting agents).
# Platform-specific hooks (ASUS fan profiles, Infernix node toggles)
# stay in the consuming host config — this module provides only the
# generic gaming stack.
{
  config,
  lib,
  pkgs,
  ...
}: let
  inherit (lib) mkEnableOption mkIf mkMerge mkOption optionalAttrs types;

  cfg = config.steampipe.gameUser;

  # Steam Linux Runtime (pressure-vessel) overlays its own /usr/lib over the
  # FHS env, so the libgamemode/libgamemodeauto we ship via extraLibraries are
  # invisible inside SLR. /nix/store IS bind-mounted in, so we resolve gamemode
  # via absolute store paths and pin LD_LIBRARY_PATH so libgamemodeauto's
  # internal dlopen("libgamemode.so.0") finds the matching client lib.
  gamemoderun-slr = pkgs.writeShellScriptBin "gamemoderun" ''
    export LD_LIBRARY_PATH="${pkgs.gamemode.lib}/lib''${LD_LIBRARY_PATH:+:}''${LD_LIBRARY_PATH-}"
    export LD_PRELOAD="${pkgs.gamemode.lib}/lib/libgamemodeauto.so.0''${LD_PRELOAD:+:}''${LD_PRELOAD-}"
    exec "$@"
  '';
in {
  options.steampipe.gameUser = {
    enable = mkEnableOption "a host-level Steam game user";

    username = mkOption {
      type = types.str;
      example = "jarvis";
      description = "Linux username for the game account.";
    };

    password = mkOption {
      type = types.nullOr types.str;
      default = "123456";
      description = ''
        Default login password for the game account. Set to null to leave the
        account without a password. This is intended for public or disposable
        game accounts; consumers should override it when they need a stronger
        password.
      '';
    };

    uid = mkOption {
      type = types.int;
      default = 1002;
      description = "UID for the game user.";
    };

    extraGroups = mkOption {
      type = types.listOf types.str;
      default = ["gamemode" "networkmanager" "video" "audio" "input"];
      description = "Supplementary groups for the game user.";
    };

    homeDirectory = mkOption {
      type = types.str;
      default = "/home/${cfg.username}";
      description = "Home directory for the game user.";
    };

    shell = mkOption {
      type = types.package;
      default = pkgs.bash;
      description = "Login shell for the game user.";
    };

    steam.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Install Steam with GameMode integration and protontricks.";
    };

    steam.extraPackages = mkOption {
      type = types.listOf types.package;
      default = [];
      description = "Additional packages added to programs.steam.extraPackages.";
    };

    gamemode.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Enable GameMode with softrealtime and renice.";
    };

    wine.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Install Wine staging, protonup-ng, and steamcmd.";
    };
  };

  config = mkIf cfg.enable (mkMerge [
    {
      # NixOS user for game execution
      users.users.${cfg.username} = {
        isNormalUser = true;
        home = cfg.homeDirectory;
        uid = cfg.uid;
        extraGroups = cfg.extraGroups;
        shell = cfg.shell;
        initialPassword = cfg.password;
      };
    }

    (mkIf cfg.steam.enable {
      nixpkgs.config.allowUnfree = true;

      programs.steam = {
        enable = true;
        package = pkgs.steam.override {
          extraLibraries = pkgs_: [pkgs_.gamemode.lib];
        };
        extraPackages = [gamemoderun-slr pkgs.gamemode] ++ cfg.steam.extraPackages;
        protontricks.enable = true;
      };
    })

    (mkIf cfg.gamemode.enable {
      programs.gamemode = {
        enable = true;
        settings = {
          general = {
            softrealtime = "auto";
            renice = 20;
          };
        };
      };
    })

    (mkIf cfg.wine.enable {
      environment.systemPackages = with pkgs; [
        wineWowPackages.stagingFull
        protonup-ng
        steamcmd
      ];
    })
  ]);
}
