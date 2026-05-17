# Global host config

## Concept

The global host config is the machine-local source of truth for a Steampipe
cluster. It exists so `cluster-ctl steam ...` can work without a project root:
NixOS hosts can publish the config from the module, and non-NixOS hosts can
write the same JSON by hand.

The important property is symmetry. The CLI does not care who wrote the file;
it only cares that the file matches the schema below and is discoverable via the
standard precedence list.

## Schema

`schemaVersion` is currently `1`. The full JSON shape is:

```json
{
  "schemaVersion": 1,
  "vmCount": 7,
  "bridge": "br-cluster",
  "subnet": "10.0.100",
  "prefix": 24,
  "hostIp": "10.0.100.254",
  "tapOwner": "can",
  "loginStateDir": "/var/lib/steampipe/logins",
  "credentialsPath": "/etc/steampipe/credentials.toml",
  "accounts": [
    { "vm": "vm-1", "steamUser": "testaccount1" },
    { "vm": "vm-2", "steamUser": "testaccount2" }
  ]
}
```

Field reference:

- `schemaVersion` (`u32`): schema discriminator. `cluster-ctl` currently
  accepts `1` only.
- `vmCount` (`u8`): total VM pool size for the host config.
- `bridge` (`string`): bridge name used by the VM network.
- `subnet` (`string`): IPv4 subnet prefix without the final octet, such as
  `10.0.100`.
- `prefix` (`u8`): network prefix length, usually `24`.
- `hostIp` (`string`): host-side IP on the cluster network.
- `tapOwner` (`string | null`): optional owner for TAP devices.
- `loginStateDir` (`path string`): directory containing Steam login state.
- `credentialsPath` (`path string | null`): optional path to the credentials
  TOML or age-encrypted file.
- `accounts` (`array`): configured Steam usernames, one per VM.
- `accounts[].vm` (`string`): VM name, such as `vm-3`.
- `accounts[].steamUser` (`string`): Steam username for that VM.

## Discovery precedence

`cluster-ctl` checks the following locations in order:

```text
1. --host-config <path>
2. $XDG_CONFIG_HOME/steampipe/host.json
3. /etc/steampipe/host.json
4. /etc/steampipe/module.json    (legacy NixOS-module path)
```

If nothing is found, Steam-group commands keep their existing project-root
behaviour.

## NixOS module walkthrough

```nix
{ services.steampipe-cluster = {
    enable = true;
    accounts = {
      vm-1 = { steamUser = "alice"; steamPass = config.age.secrets.alice-pw.path; };
      vm-2 = { steamUser = "bob"; steamPass = config.age.secrets.bob-pw.path; };
    };
  };
}
```

From `$HOME`, `cluster-ctl steam info` should render the discovered module and
its selected row in the trace:

```text
Host config source: /etc/steampipe/module.json (legacy NixOS-module path; schemaVersion 1)

Discovery trace:
  1. --host-config flag                                (not set)
  2. ~/.config/steampipe/host.json                     (not found)
  3. /etc/steampipe/host.json                          (not found)
  4. /etc/steampipe/module.json                        (selected)
```

## Hand-written walkthrough

1. Create `~/.config/steampipe/host.json`:

   ```json
   { "schemaVersion": 1, "vmCount": 7, "bridge": "br-cluster",
     "subnet": "10.0.100", "prefix": 24, "hostIp": "10.0.100.254",
     "tapOwner": null, "loginStateDir": "/var/lib/steampipe/logins",
     "credentialsPath": null, "accounts": [] }
   ```

2. Verify it with `cluster-ctl steam info`.
3. Optional: create a credentials TOML anywhere convenient and set
   `credentialsPath` to point at it.

## Troubleshooting

- If `steam info` shows `(not found)` for every row, check `XDG_CONFIG_HOME`
  or `HOME`, confirm the file exists, and make sure you picked the right file
  name (`host.json` versus `module.json`).
- If you see `schemaVersion mismatch`, the file still needs `schemaVersion: 1`
  for this release.
- If `--host-config /tmp/foo.json` errors while reading the file, check the
  path and permissions with `ls -l /tmp/foo.json`.
