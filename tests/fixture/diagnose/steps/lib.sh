#!/usr/bin/env bash

parse_reserved_vm_id_from_log() {
    local log_path="$1"
    [ -f "$log_path" ] || return 0

    sed -n 's/^  Reserved: vm-\([0-9][0-9]*\)\(,.*\)\?$/\1/p' "$log_path" | head -n1
}
