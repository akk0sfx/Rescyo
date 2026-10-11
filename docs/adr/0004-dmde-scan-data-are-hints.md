# DMDE scan data are untrusted hints, verified against the Source

`import-dmde` requires the Source at import time. Volume candidates and directory starts from `devscan.log` are stored as Hints and each directory start is confirmed by reading the Source (cluster-grid alignment, parses as a directory; the cluster's own `..` overrides DMDE's parent). We then run our own FAT engine seeded with the confirmed starts. Importing DMDE's results verbatim was rejected: the log may come from a different device, a different moment, or a hostile sender, and it is parsed with strict size and count limits.
