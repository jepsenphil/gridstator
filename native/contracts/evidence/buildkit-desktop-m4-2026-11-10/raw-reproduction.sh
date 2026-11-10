#!/bin/sh
set -eu
export PATH=/Applications/Docker.app/Contents/Resources/bin:/usr/bin:/bin:/usr/sbin:/sbin
export DOCKER_HOST=unix:///Users/philjepsen/.docker/run/docker.sock
base=/Users/philjepsen/gridstator-validation.I50ONpwm/docker-tools/buildx-metadata-repro
mkdir -p "$base/context"
printf '%s\n' 'FROM alpine:3.20' 'LABEL gridstator.repro=metadata' > "$base/context/Dockerfile"
docker buildx build --load --metadata-file "$base/metadata.json" --tag gridstator-validation/e3-metadata-repro:latest "$base/context"
cat "$base/metadata.json"
docker image inspect gridstator-validation/e3-metadata-repro:latest --format '{{json .}}' > "$base/inspect.json"
docker image rm gridstator-validation/e3-metadata-repro:latest
