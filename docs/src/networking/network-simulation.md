# Network Simulation

Simulate real-world network conditions on VM TAP devices using `tc netem`.
Requires sudo.

## Usage

```bash
# Add 80ms latency with 10ms jitter to vm-3
sudo cluster-ctl netem vm-3 --latency 80 --jitter 10

# Add 2% packet loss to all VMs
sudo cluster-ctl netem all --loss 2

# Limit bandwidth to 1000 kbit/s on vm-1
sudo cluster-ctl netem vm-1 --rate 1000

# Combine everything
sudo cluster-ctl netem all \
  --latency 50 --jitter 20 --loss 1 --rate 5000

# Check what's applied
cluster-ctl --vm-count 7 netem-show

# Clear all rules
sudo cluster-ctl --vm-count 7 netem-reset
```

> `netem` (apply rules) operates on the cluster's existing lease state, so
> `--vm-count` is not required. `netem-show` and `netem-reset` still take it
> for now.

## Testing scenarios

Network simulation is useful for testing:

- **Packet loss** - how your netcode handles dropped packets
- **High latency** - behavior under intercontinental-scale delays
- **Bandwidth limits** - constrained mobile or satellite connections
- **Combined degradation** - realistic worst-case conditions

## Parameters

| Flag | Unit | Description |
|------|------|-------------|
| `--latency` | ms | Added one-way delay |
| `--jitter` | ms | Random variation on latency |
| `--loss` | % | Packet drop probability |
| `--rate` | kbit/s | Bandwidth cap |
