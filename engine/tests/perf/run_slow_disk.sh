#!/bin/sh
# Runs an image with a 6 ms flush (like a cloud managed disk) on a Docker volume and measures it.
#   tests/perf/run_slow_disk.sh edivanteixeira/stuntman:<tag> [STUNTMAN_DURABILITY]
set -e
IMG=${1:?image}; DUR=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
docker run --rm -v "$HERE":/src -w /src gcc:14 gcc -shared -fPIC -O2 -o /src/slow_fsync.so slow_fsync.c -ldl
VOL=stuntman-bench-$$
docker volume create "$VOL" >/dev/null
ID=$(docker run -d --rm -v "$VOL":/data -v "$HERE/slow_fsync.so":/slow_fsync.so -e LD_PRELOAD=/slow_fsync.so ${DUR:+-e STUNTMAN_DURABILITY=$DUR} \
  -p 127.0.0.1::8025 -p 127.0.0.1::8028 "$IMG")
trap 'docker stop "$ID" >/dev/null; docker volume rm "$VOL" >/dev/null; rm -f "$HERE/slow_fsync.so"' EXIT
API=http://127.0.0.1:$(docker port "$ID" 8025 | head -1 | cut -d: -f2)
APIS=http://127.0.0.1:$(docker port "$ID" 8028 | head -1 | cut -d: -f2)
for i in $(seq 60); do curl -sf "$API/api/health" >/dev/null && break; sleep 1; done
python3 "$HERE/bench_writes.py" "$API" "$APIS"
