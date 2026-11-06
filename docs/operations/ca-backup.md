# Backing up the coordinator's CA

Right after the CA is made, copy its key file and its certificate file once to removable media,
encrypt the copy to an operator's offline key, and keep it sealed and offline; the coordinator's
code only guarantees that the key is a single PKCS#8 file (mode 0600) and that the certificate,
whose SHA-256 every daemon config pins, is made once and never again.

`hostkeeper-coordinator backup` never copies the CA's key or its certificate, so this copy is the
only one. The rest of the coordinator's state (the state file, the ops file and the enrollment
store, with the files beside them) is backed up and restored as
[coordinator-state.md](coordinator-state.md) says.
