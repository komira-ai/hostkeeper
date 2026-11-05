# The coordinator's container image: `hostkeeper-coordinator-<version>-<platform>.image.tar` of a
# release (docs/deploy/verify-a-release.md), built by the `image` job of
# .github/workflows/artifacts.yml from a context that holds the released binary
# `hostkeeper-coordinator` and nothing else.
#
# The base is distroless (glibc and libgcc, no shell, no package manager), pinned by the digest
# of its multi-platform index, so a rebuild from the same commit starts from the same bytes; the
# image job builds it twice and releases it only when the two builds are byte for byte equal.
# The binary is the entrypoint and runs as distroless's `nonroot` user (65532). Its
# configuration is its flags, given as the container's arguments (`serve --state-file ...`): the
# image sets no environment variable and no default arguments. A test of the packaging
# (crates/xtask/tests/packaging/container.rs) holds this file to these rules.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1
COPY --chmod=0555 hostkeeper-coordinator /usr/bin/hostkeeper-coordinator
USER 65532:65532
ENTRYPOINT ["/usr/bin/hostkeeper-coordinator"]
