#!/usr/bin/env bash
# One command for the Linux side: builds the test image, then runs build, tests, clippy and the
# end-to-end audio test in a container. The repository is mounted read-only, and cargo writes to
# named volumes, so the Windows engine/target directory is never touched.
#
#   bash tools/linux-docker/run.sh
#
# Needs Docker with a running daemon. Set HUSHCORE_LINUX_IMAGE to override the image tag.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
IMAGE="${HUSHCORE_LINUX_IMAGE:-hushcore-linux-e2e:ubuntu24.04}"
BASE="ubuntu:24.04"

if ! command -v docker >/dev/null 2>&1; then
    echo "run.sh: docker is not installed" >&2
    exit 2
fi
if ! docker info >/dev/null 2>&1; then
    echo "run.sh: the Docker daemon is not running; start Docker Desktop and retry" >&2
    exit 2
fi

# Git Bash paths must reach the Windows docker CLI as Windows paths, and MSYS must not rewrite the
# container paths.
host_path() {
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -m "$1"
    else
        printf '%s\n' "$1"
    fi
}
HOST_REPO="$(host_path "$REPO")"
HOST_HERE="$(host_path "$HERE")"
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

docker build -t "$IMAGE" --build-arg "BASE=$BASE" -f "$HOST_HERE/Dockerfile" "$HOST_HERE"

docker volume create hushcore-cargo-home >/dev/null
docker volume create hushcore-cargo-target >/dev/null

docker run --rm \
    --name hushcore-linux-e2e \
    -v "$HOST_REPO:/src:ro" \
    -v hushcore-cargo-home:/cargo \
    -v hushcore-cargo-target:/target \
    "$IMAGE" \
    bash /src/tools/linux-docker/container/entrypoint.sh
