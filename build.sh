#!/bin/sh
podman build -f docker/Dockerfile.packages -t lumen-packages .
mkdir -p dist
podman run --rm -v ./dist:/output lumen-packages