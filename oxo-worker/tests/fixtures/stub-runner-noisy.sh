#!/bin/sh
# More than the pipe buffer of stdout noise, then the ok line.
read -r _line
i=0
while [ $i -lt 2000 ]; do
  echo "noise noise noise noise noise noise noise noise noise noise noise"
  i=$((i+1))
done
echo '{"outcome":"ok"}'
