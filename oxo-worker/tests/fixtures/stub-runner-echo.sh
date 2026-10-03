#!/bin/sh
# Echoes the received input back as the failure reason, to prove the stdin contract.
read -r line
printf '{"outcome":"failed","reason":%s,"phase":"echo"}\n' "$(printf '%s' "$line" | sed 's/\\/\\\\/g; s/"/\\"/g; s/^/"/; s/$/"/')"
