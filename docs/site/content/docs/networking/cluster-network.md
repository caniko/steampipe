+++
title = "Cluster Network"
description = "Bridge, TAP, and NAT setup"
weight = 10
+++

## Setting up the cluster network

```bash
sudo cluster-ctl --vm-count 7 net-up
```

This creates a bridge topology:

```
 Host (10.0.100.254)
       │
  ┌────┴────┐  br-cluster
  │ Bridge  │
  ├─────────┤
  │tap-vm-1 │──── vm-1 (10.0.100.1)
  │tap-vm-2 │──── vm-2 (10.0.100.2)
  │   ...   │
  │tap-vm-7 │──── vm-7 (10.0.100.7)
  └─────────┘

  NAT: 10.0.100.0/24 → masquerade via default interface
```

Each VM's MAC address is deterministic (`52:54:00:cb:00:0N`).

## Tearing down

```bash
sudo cluster-ctl --vm-count 7 net-down
```

Removes all TAP devices, the bridge, and the nftables `cluster` table.

## Custom nft path

```bash
sudo cluster-ctl --vm-count 7 net-up --nft /run/current-system/sw/bin/nft
```

## Multiple clusters

Run independent clusters simultaneously with the `--cluster` flag:

```bash
# Start a "nightly" cluster
sudo cluster-ctl --vm-count 7 --cluster nightly net-up
cluster-ctl --vm-count 7 --cluster nightly up --runners-dir ./result

# Start a "feature-branch" cluster with 3 VMs
sudo cluster-ctl --vm-count 3 --cluster feature net-up
cluster-ctl --vm-count 3 --cluster feature up --runners-dir ./result-feature
```

Each cluster gets its own state directory (`$XDG_STATE_HOME/steampipe/<cluster-name>/`).
