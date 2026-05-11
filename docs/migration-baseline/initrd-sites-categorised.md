# Initrd Site Inventory

Captured on 2026-05-11 before adding steampipe's reusable cluster VM builder.

## Steampipe checkout

Command:

```sh
rg -n 'boot\.initrd\.' .
```

Result: no direct project call sites.

## Regicide checkout

Command:

```sh
rg -n 'boot\.initrd\.' .
```

Result: no live NixOS module call sites. Matches were only in
`docs/src/planning/pre-landing-cleanup/05-nixos-scripted-initrd.md`, which is
the planning document for this migration.

## Inherited default

The scripted-initrd warning for the crosvm UI-full flavor comes from
microvm.nix's hypervisor default, not from project-local `boot.initrd.*`
settings. In the locked microvm.nix module, `boot.initrd.systemd.enable` is
defaulted on only for:

```nix
[ "qemu" "cloud-hypervisor" "firecracker" "stratovirt" ]
```

`crosvm` is excluded there, so the generated crosvm VM uses scripted initrd
until a consumer explicitly sets `boot.initrd.systemd.enable = true`.

## Categorisation

| Category | Sites | Action |
|---|---:|---|
| `boot.initrd.availableKernelModules` | 0 | No project-local migration needed. |
| `boot.initrd.kernelModules` | 0 | No project-local migration needed. |
| `boot.initrd.preMountCommands` | 0 | No bash translation needed. |
| `boot.initrd.postMountCommands` | 0 | No bash translation needed. |
| `boot.initrd.network.postCommands` | 0 | No stage-1 network translation needed. |
| `boot.initrd.extraUtilsCommands` | 0 | No stage-1 utility translation needed. |
| `boot.initrd.network.enable` | 0 | No project-local migration needed. |
| `boot.initrd.network.ssh.*` | 0 | No stage-1 SSH translation needed. |

The Phase E implementation therefore adds a narrow option that sets
`boot.initrd.systemd.enable` for selected VM flavors. It does not translate
legacy initrd shell hooks because there are no project-local hooks to preserve.
