# The coordinator's container image: `hostkeeper-coordinator-<version>-<platform>.image.tar` of a
# release (docs/deploy/verify-a-release.md), built by the `image` job of
# .github/workflows/artifacts.yml from a context that holds the released binary
# `hostkeeper-coordinator` and the operator web app `ui/` of the commit (files 0644, directories
# 0755), and nothing else.
#
# The base is distroless (glibc and libgcc, no shell, no package manager), pinned by the digest
# of its multi-platform index, so a rebuild from the same commit starts from the same bytes; the
# image job builds it twice and releases it only when the two builds are byte for byte equal.
# The binary is the entrypoint and runs as distroless's `nonroot` user (65532). Its
# configuration is its flags, given as the container's arguments (`serve --state-file ...`): the
# image sets no environment variable and no default arguments. The web app is copied to
# /usr/share/hostkeeper-coordinator/ui, owned by root, which `serve` serves under /ui/ when it is
# given no --ui-dir. It is copied without --chmod (which would set the mode of its directories
# too, so that no one could enter them): it keeps the modes of the context. A test of the
# packaging (crates/xtask/tests/packaging/container.rs) holds this file to these rules, and
# `cargo xtask release check-image-ui` reads the built image back.
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:08476c244888bfb425bae1ae4dd7c17f093e972aab8426883d97f3c6f280f7c1
COPY --chmod=0555 hostkeeper-coordinator /usr/bin/hostkeeper-coordinator
COPY ui /usr/share/hostkeeper-coordinator/ui
USER 65532:65532
ENTRYPOINT ["/usr/bin/hostkeeper-coordinator"]
